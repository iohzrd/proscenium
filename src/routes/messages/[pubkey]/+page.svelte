<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import MessageComposer from "$lib/MessageComposer.svelte";
  import ChatHeader from "$lib/messages/ChatHeader.svelte";
  import MessageBubble from "$lib/messages/MessageBubble.svelte";
  import TypingIndicator from "$lib/messages/TypingIndicator.svelte";
  import type { StoredMessage, Profile, MediaAttachment } from "$lib/types";
  import { getDisplayName, getCachedAvatarTicket } from "$lib/utils";
  import { createBlobCache } from "$lib/blobs";
  import { hapticNotification } from "$lib/haptics";
  import { useNodeInit, useEventListeners } from "$lib/composables";

  let pubkey: string = $derived(page.params.pubkey ?? "");
  let peerName = $state("");
  let peerProfile = $state<Profile | null>(null);
  let messages = $state<StoredMessage[]>([]);
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
  let failedIds = $state(new Set<string>());
  let retryingIds = $state(new Set<string>());
  const SEND_TIMEOUT_MS = 30_000;

  const blobs = createBlobCache();

  const node = useNodeInit(async () => {
    peerName = await getDisplayName(pubkey, node.pubkey);
    try {
      peerProfile = await invoke("get_remote_profile", { pubkey });
    } catch {
      // peer profile may not be available
    }
    const msgs: StoredMessage[] = await invoke("get_dm_messages", {
      peerPubkey: pubkey,
      limit: 50,
      before: null,
    });
    messages = msgs;
    hasMore = msgs.length >= 50;

    await invoke("mark_dm_read", { peerPubkey: pubkey });

    // Send read receipts for unread incoming messages
    for (const msg of msgs) {
      if (msg.from_pubkey !== node.pubkey && !msg.read) {
        invoke("send_dm_signal", {
          to: pubkey,
          signalType: "read",
          messageId: msg.id,
        }).catch(() => {});
      }
    }

    requestAnimationFrame(() => scrollToBottom());
  });

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
    if (loadingMore || !hasMore || messages.length === 0) return;
    loadingMore = true;
    try {
      const oldest = messages[0];
      const olderMsgs: StoredMessage[] = await invoke("get_dm_messages", {
        peerPubkey: pubkey,
        limit: 50,
        before: oldest.timestamp,
      });
      if (olderMsgs.length === 0) {
        hasMore = false;
      } else {
        const prevHeight = messagesContainer?.scrollHeight ?? 0;
        messages = [...olderMsgs, ...messages];
        hasMore = olderMsgs.length >= 50;
        requestAnimationFrame(() => {
          if (messagesContainer) {
            const newHeight = messagesContainer.scrollHeight;
            messagesContainer.scrollTop = newHeight - prevHeight;
          }
        });
      }
    } catch (e) {
      console.error("Failed to load older messages:", e);
    }
    loadingMore = false;
  }

  function sendTypingSignal() {
    const now = Date.now();
    if (now - lastTypingSent < 3000) return;
    lastTypingSent = now;
    invoke("send_dm_signal", {
      to: pubkey,
      signalType: "typing",
      messageId: null,
    }).catch(() => {});
  }

  async function handleSend(
    text: string,
    media: MediaAttachment[] | null,
  ): Promise<void> {
    const msg: StoredMessage = await invoke("send_dm", {
      to: pubkey,
      content: text,
      media,
    });
    messages = [...messages, msg];
    sendTimestamps.set(msg.id, Date.now());
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
      console.error("Retry failed:", e);
    }
    retryingIds.delete(msgId);
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

  // Check for timed-out messages every 5s
  $effect(() => {
    const interval = setInterval(() => {
      const now = Date.now();
      let changed = false;
      for (const [msgId, sentAt] of sendTimestamps) {
        const msg = messages.find((m) => m.id === msgId);
        if (!msg || msg.delivered || msg.read) {
          sendTimestamps.delete(msgId);
          failedIds.delete(msgId);
          changed = true;
          continue;
        }
        if (now - sentAt > SEND_TIMEOUT_MS && !retryingIds.has(msgId)) {
          failedIds.add(msgId);
          changed = true;
        }
      }
      if (changed) {
        failedIds = new Set(failedIds);
      }
    }, 5000);
    return () => clearInterval(interval);
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "dm-received": (raw) => {
        const payload = raw as {
          from: string;
          message: StoredMessage;
        };
        if (payload.from === pubkey) {
          messages = [...messages, payload.message];
          invoke("mark_dm_read", { peerPubkey: pubkey });
          // Send read receipt
          invoke("send_dm_signal", {
            to: pubkey,
            signalType: "read",
            messageId: payload.message.id,
          }).catch(() => {});
          if (shouldAutoScroll) {
            requestAnimationFrame(() => scrollToBottom());
          }
        }
      },
      "dm-delivered": (raw) => {
        const payload = raw as { message_id: string };
        messages = messages.map((m) =>
          m.id === payload.message_id ? { ...m, delivered: true } : m,
        );
        sendTimestamps.delete(payload.message_id);
        failedIds.delete(payload.message_id);
        retryingIds.delete(payload.message_id);
        failedIds = new Set(failedIds);
        retryingIds = new Set(retryingIds);
      },
      "typing-indicator": (raw) => {
        const payload = raw as { peer: string };
        if (payload.peer === pubkey) {
          peerTyping = true;
          if (typingTimeout) clearTimeout(typingTimeout);
          typingTimeout = setTimeout(() => {
            peerTyping = false;
          }, 4000);
        }
      },
      "dm-read": (raw) => {
        const payload = raw as { message_id: string };
        messages = messages.map((m) =>
          m.id === payload.message_id ? { ...m, read: true } : m,
        );
      },
    });
    return () => {
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
