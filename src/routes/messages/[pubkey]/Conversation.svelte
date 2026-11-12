<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import MessageComposer from "#lib/MessageComposer.svelte";
  import ChatHeader from "#lib/messages/ChatHeader.svelte";
  import MessageBubble from "#lib/messages/MessageBubble.svelte";
  import TypingIndicator from "#lib/messages/TypingIndicator.svelte";
  import type { StoredMessage, Profile, MediaAttachment } from "#lib/types.ts";
  import {
    comparePosition,
    dedupeById,
    getDisplayName,
    getCachedAvatarTicket,
    isWindowActive,
  } from "#lib/utils.ts";
  import { createBlobCache } from "#lib/blobs.ts";
  import { hapticNotification } from "#lib/haptics.ts";
  import {
    useNodeInit,
    useEventListeners,
    useToast,
  } from "#lib/composables/index.ts";

  // The parent creates one instance per peer, so `pubkey` does not change
  // during the lifetime of this component.
  let { pubkey }: { pubkey: string } = $props();

  const PAGE_SIZE = 50;
  // An own message without a delivery acknowledgement is shown as failed after this time.
  const SEND_TIMEOUT_MS = 30_000;
  const SEND_CHECK_INTERVAL_MS = 5_000;
  const TYPING_SIGNAL_INTERVAL_MS = 3000;
  const TYPING_DISPLAY_MS = 4000;
  // Bound on remembered `dm-delivered` ids whose message is not in the list yet.
  const MAX_EARLY_DELIVERED = 200;

  let peerName = $state("");
  let peerProfile = $state<Profile | null>(null);
  let messages = $state<StoredMessage[]>([]);
  // Oldest message of the pages loaded from storage: the cursor of the next older
  // page. Messages merged from events do not move it, so a merged message with an
  // old timestamp does not skip the stored messages after it.
  let pageCursor: StoredMessage | null = null;
  let hasMore = $state(true);
  let loadingMore = $state(false);
  let messagesContainer = $state<HTMLDivElement>(null!);
  let shouldAutoScroll = $state(true);
  let peerTyping = $state(false);
  let typingTimeout: ReturnType<typeof setTimeout> | null = null;
  let lastTypingSent = 0;
  let composer = $state<ReturnType<typeof MessageComposer>>(null!);

  // Message delivery tracking
  const sendTimestamps = new Map<string, number>();
  // Ids from `dm-delivered` events that arrived before `send_dm` returned the message.
  const deliveredEarly = new Set<string>();
  let failedIds = $state(new Set<string>());
  let retryingIds = $state(new Set<string>());
  // True once `mark_dm_read` succeeded for this conversation view.
  let markedRead = false;

  const toast = useToast();
  const blobs = createBlobCache({ onError: toast.show });

  const node = useNodeInit(async () => {
    peerName = await getDisplayName(pubkey, node.pubkey);
    try {
      peerProfile = await invoke("get_remote_profile", { pubkey });
    } catch {
      // peer profile may not be available
    }
    const page: StoredMessage[] = await invoke("get_dm_messages", {
      peerPubkey: pubkey,
      limit: PAGE_SIZE,
      before: null,
      beforeId: null,
    });
    messages = mergeMessages(page, messages);
    pageCursor = oldestOf(page);
    hasMore = page.length >= PAGE_SIZE;

    // Own messages still waiting for an acknowledgement, such as queued ones
    // from an earlier session.
    for (const msg of messages) {
      if (msg.from_pubkey === node.pubkey && !msg.delivered && !msg.read) {
        sendTimestamps.set(msg.id, msg.timestamp);
      }
    }
    checkSendTimeouts();

    await markRead();
    requestAnimationFrame(() => {
      scrollToBottom();
      loadOlderUntilScrollable();
    });
  });

  /**
   * Load older pages while the message list does not overflow its container:
   * without a scrollbar no scroll event reaches `handleScroll`.
   */
  function loadOlderUntilScrollable() {
    if (!messagesContainer || !hasMore || loadingMore) return;
    if (messagesContainer.scrollHeight <= messagesContainer.clientHeight) {
      loadOlder();
    }
  }

  /** Union of both lists by id (entries of `preferred` win), ascending by `(timestamp, id)`. */
  /** The first message of `page` in list order, or null for an empty page. */
  function oldestOf(page: StoredMessage[]): StoredMessage | null {
    return [...page].sort(comparePosition)[0] ?? null;
  }

  function mergeMessages(
    preferred: StoredMessage[],
    others: StoredMessage[],
  ): StoredMessage[] {
    return dedupeById([...preferred, ...others]).sort(comparePosition);
  }

  /**
   * Mark the conversation read and send one read receipt per unread incoming
   * message in the list. Returns early unless the window is active. After the
   * first successful call, also returns early if the list has no unread
   * incoming message.
   */
  async function markRead() {
    if (!isWindowActive()) return;
    const unread = messages.filter(
      (m) => m.from_pubkey !== node.pubkey && !m.read,
    );
    if (unread.length === 0 && markedRead) return;
    try {
      await invoke("mark_dm_read", { peerPubkey: pubkey });
      markedRead = true;
    } catch (e) {
      console.error("Failed to mark conversation read:", e);
      return;
    }
    if (unread.length === 0) return;
    const unreadIds = new Set(unread.map((m) => m.id));
    messages = messages.map((m) =>
      unreadIds.has(m.id) ? { ...m, read: true } : m,
    );
    for (const id of unreadIds) {
      invoke("send_dm_signal", {
        to: pubkey,
        signalType: "read",
        messageId: id,
      }).catch(() => {});
    }
  }

  function handleActivation() {
    if (!node.loading) markRead();
  }

  function scrollToBottom() {
    if (messagesContainer) {
      messagesContainer.scrollTop = messagesContainer.scrollHeight;
    }
  }

  function handleScroll() {
    if (!messagesContainer) return;
    const { scrollTop, scrollHeight, clientHeight } = messagesContainer;
    shouldAutoScroll = scrollHeight - scrollTop - clientHeight < 100;

    if (scrollTop < 100 && hasMore && !loadingMore) {
      loadOlder();
    }
  }

  async function loadOlder() {
    const cursor = pageCursor;
    if (loadingMore || !hasMore || !cursor) return;
    loadingMore = true;
    try {
      const olderMsgs: StoredMessage[] = await invoke("get_dm_messages", {
        peerPubkey: pubkey,
        limit: PAGE_SIZE,
        before: cursor.timestamp,
        beforeId: cursor.id,
      });
      pageCursor = oldestOf(olderMsgs) ?? cursor;
      const known = new Set(messages.map((m) => m.id));
      const added = olderMsgs.filter((m) => !known.has(m.id));
      if (olderMsgs.length === 0) {
        hasMore = false;
      } else if (added.length === 0) {
        // Every message of this page was merged from an event already.
        hasMore = olderMsgs.length >= PAGE_SIZE;
        requestAnimationFrame(loadOlderUntilScrollable);
      } else {
        const prevHeight = messagesContainer?.scrollHeight ?? 0;
        messages = mergeMessages(messages, added);
        hasMore = olderMsgs.length >= PAGE_SIZE;
        requestAnimationFrame(() => {
          if (messagesContainer) {
            const newHeight = messagesContainer.scrollHeight;
            messagesContainer.scrollTop = newHeight - prevHeight;
          }
          loadOlderUntilScrollable();
        });
      }
    } catch (e) {
      toast.show("Failed to load older messages");
      console.error("Failed to load older messages:", e);
    }
    loadingMore = false;
  }

  function sendTypingSignal() {
    const now = Date.now();
    if (now - lastTypingSent < TYPING_SIGNAL_INTERVAL_MS) return;
    lastTypingSent = now;
    invoke("send_dm_signal", {
      to: pubkey,
      signalType: "typing",
      messageId: null,
    }).catch(() => {});
  }

  /** Rejects if `send_dm` rejects; the composer shows the error. */
  async function handleSend(
    text: string,
    media: MediaAttachment[] | null,
  ): Promise<void> {
    const msg: StoredMessage = await invoke("send_dm", {
      to: pubkey,
      content: text,
      media,
      replyTo: null,
    });
    const acknowledgedEarly = deliveredEarly.delete(msg.id);
    const delivered = msg.delivered || acknowledgedEarly;
    messages = mergeMessages([{ ...msg, delivered }], messages);
    if (!delivered) sendTimestamps.set(msg.id, Date.now());
    hapticNotification("success");
    requestAnimationFrame(() => scrollToBottom());
  }

  async function retryMessage(msgId: string) {
    retryingIds.add(msgId);
    retryingIds = new Set(retryingIds);
    failedIds.delete(msgId);
    failedIds = new Set(failedIds);
    sendTimestamps.set(msgId, Date.now());
    try {
      await invoke("flush_dm_outbox");
    } catch (e) {
      toast.show(`Retry failed: ${e}`);
      console.error("Retry failed:", e);
    }
    retryingIds.delete(msgId);
    retryingIds = new Set(retryingIds);
  }

  function markDelivered(messageId: string) {
    if (messages.some((m) => m.id === messageId)) {
      messages = messages.map((m) =>
        m.id === messageId ? { ...m, delivered: true } : m,
      );
    } else {
      if (deliveredEarly.size >= MAX_EARLY_DELIVERED) deliveredEarly.clear();
      deliveredEarly.add(messageId);
    }
    sendTimestamps.delete(messageId);
    failedIds.delete(messageId);
    retryingIds.delete(messageId);
    failedIds = new Set(failedIds);
    retryingIds = new Set(retryingIds);
  }

  function isSameDay(ts1: number, ts2: number): boolean {
    const d1 = new Date(ts1);
    const d2 = new Date(ts2);
    return (
      d1.getFullYear() === d2.getFullYear() &&
      d1.getMonth() === d2.getMonth() &&
      d1.getDate() === d2.getDate()
    );
  }

  function formatDate(ts: number): string {
    const d = new Date(ts);
    const today = new Date();
    if (isSameDay(ts, today.getTime())) return "Today";
    const yesterday = new Date(today);
    yesterday.setDate(yesterday.getDate() - 1);
    if (isSameDay(ts, yesterday.getTime())) return "Yesterday";
    return d.toLocaleDateString([], {
      month: "short",
      day: "numeric",
      year: d.getFullYear() !== today.getFullYear() ? "numeric" : undefined,
    });
  }

  function shouldShowDate(index: number): boolean {
    if (index === 0) return true;
    return !isSameDay(messages[index].timestamp, messages[index - 1].timestamp);
  }

  /** Move tracked own messages older than `SEND_TIMEOUT_MS` to `failedIds`. */
  function checkSendTimeouts() {
    const now = Date.now();
    let changed = false;
    for (const [msgId, sentAt] of sendTimestamps) {
      const msg = messages.find((m) => m.id === msgId);
      if (!msg || msg.delivered || msg.read) {
        sendTimestamps.delete(msgId);
        changed = failedIds.delete(msgId) || changed;
        continue;
      }
      if (
        now - sentAt > SEND_TIMEOUT_MS &&
        !retryingIds.has(msgId) &&
        !failedIds.has(msgId)
      ) {
        failedIds.add(msgId);
        changed = true;
      }
    }
    if (changed) {
      failedIds = new Set(failedIds);
    }
  }

  onMount(() => {
    node.init();
    const sendCheck = setInterval(checkSendTimeouts, SEND_CHECK_INTERVAL_MS);
    const cleanupListeners = useEventListeners({
      "dm-received": (raw) => {
        const payload = raw as {
          from: string;
          message: StoredMessage;
        };
        if (payload.from !== pubkey) return;
        messages = mergeMessages(messages, [payload.message]);
        // Marks read and sends the receipt only while the window is active;
        // otherwise `handleActivation` does it later.
        if (!node.loading) markRead();
        if (shouldAutoScroll) {
          requestAnimationFrame(() => scrollToBottom());
        }
      },
      // Messages of this conversation that another device of this user received
      // or sent.
      "dm-synced": (raw) => {
        const payload = raw as { peer: string; messages: StoredMessage[] };
        if (payload.peer !== pubkey) return;
        messages = mergeMessages(messages, payload.messages);
      },
      "dm-delivered": (raw) => {
        markDelivered((raw as { message_id: string }).message_id);
      },
      "typing-indicator": (raw) => {
        const payload = raw as { peer: string };
        if (payload.peer === pubkey) {
          peerTyping = true;
          if (typingTimeout) clearTimeout(typingTimeout);
          typingTimeout = setTimeout(() => {
            peerTyping = false;
          }, TYPING_DISPLAY_MS);
        }
      },
      "dm-read": (raw) => {
        const payload = raw as { message_id: string };
        messages = messages.map((m) =>
          m.id === payload.message_id ? { ...m, read: true } : m,
        );
      },
    });
    window.addEventListener("focus", handleActivation);
    document.addEventListener("visibilitychange", handleActivation);
    return () => {
      window.removeEventListener("focus", handleActivation);
      document.removeEventListener("visibilitychange", handleActivation);
      clearInterval(sendCheck);
      if (typingTimeout) clearTimeout(typingTimeout);
      blobs.revokeAll();
      composer?.revokeAttachments();
      cleanupListeners();
    };
  });
</script>

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Loading conversation...</p>
  </div>
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
  </div>
{:else}
  <div class="chat-layout">
    <ChatHeader
      {pubkey}
      {peerName}
      avatarTicket={peerProfile?.avatar_ticket ?? getCachedAvatarTicket(pubkey)}
    />

    <div
      class="messages-container"
      bind:this={messagesContainer}
      onscroll={handleScroll}
    >
      {#if loadingMore}
        <div class="loading-more">
          <span class="btn-spinner"></span> Loading...
        </div>
      {/if}

      {#each messages as msg, i (msg.id)}
        {#if shouldShowDate(i)}
          <div class="date-separator">
            <span>{formatDate(msg.timestamp)}</span>
          </div>
        {/if}
        <MessageBubble
          {msg}
          isSent={msg.from_pubkey === node.pubkey}
          isFailed={failedIds.has(msg.id)}
          isRetrying={retryingIds.has(msg.id)}
          {blobs}
          onretry={retryMessage}
        />
      {:else}
        <div class="empty-chat">
          <p>No messages yet. Say hello!</p>
        </div>
      {/each}

      {#if peerTyping}
        <TypingIndicator {peerName} />
      {/if}
    </div>

    <MessageComposer
      bind:this={composer}
      onsubmit={handleSend}
      oninput={sendTypingSignal}
    />
  </div>
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

<style>
  .chat-layout {
    display: flex;
    flex-direction: column;
    height: calc(100dvh - 60px - env(safe-area-inset-top, 0px));
    margin: -1rem -1rem calc(-2rem - env(safe-area-inset-bottom, 0px));
  }

  .messages-container {
    flex: 1;
    overflow-y: auto;
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .loading-more {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.4rem;
    padding: 0.5rem;
    color: var(--text-secondary);
    font-size: var(--text-base);
  }

  .date-separator {
    display: flex;
    justify-content: center;
    padding: 0.75rem 0 0.5rem;
  }

  .date-separator span {
    background: var(--bg-elevated);
    color: var(--text-secondary);
    font-size: var(--text-sm);
    padding: 0.2rem 0.75rem;
    border-radius: var(--radius-full);
  }

  .empty-chat {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-tertiary);
    font-size: var(--text-base);
  }
</style>
