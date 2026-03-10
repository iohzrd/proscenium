<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Avatar from "$lib/Avatar.svelte";
  import MessageComposer from "$lib/MessageComposer.svelte";
  import type { StoredMessage, Profile, MediaAttachment } from "$lib/types";
  import {
    shortId,
    getDisplayName,
    getCachedAvatarTicket,
    isImage,
    isVideo,
    isAudio,
    formatSize,
  } from "$lib/utils";
  import { createBlobCache } from "$lib/blobs";
  import { hapticNotification } from "$lib/haptics";
  import { useNodeInit, useEventListeners } from "$lib/composables.svelte";

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

  function formatTime(ts: number): string {
    return new Date(ts).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
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
    <div class="chat-header">
      <a href="/messages" class="back-btn">&larr;</a>
      <Avatar
        {pubkey}
        name={peerName}
        ticket={peerProfile?.avatar_ticket ?? getCachedAvatarTicket(pubkey)}
        size={32}
      />
      <div class="header-info">
        <span class="header-name">{peerName}</span>
      </div>
    </div>

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
        <div
          class="message-row"
          class:sent={msg.from_pubkey === node.pubkey}
          class:received={msg.from_pubkey !== node.pubkey}
          class:failed-msg={msg.from_pubkey === node.pubkey &&
            failedIds.has(msg.id)}
        >
          <div class="message-bubble">
            {#if msg.media && msg.media.length > 0}
              <div class="message-media">
                {#each msg.media as att}
                  {#if isImage(att.mime_type)}
                    {#await blobs.getBlobUrl(att) then url}
                      <img src={url} alt={att.filename} class="media-img" />
                    {/await}
                  {:else if isVideo(att.mime_type)}
                    {#await blobs.getBlobUrl(att) then url}
                      <video
                        src={url}
                        controls
                        class="media-video"
                        preload="metadata"
                      ></video>
                    {/await}
                  {:else if isAudio(att.mime_type)}
                    {#await blobs.getBlobUrl(att) then url}
                      <div class="audio-attachment">
                        <span class="audio-filename">{att.filename}</span>
                        <audio src={url} controls preload="metadata"></audio>
                      </div>
                    {/await}
                  {:else}
                    <button
                      class="file-attachment"
                      onclick={() => blobs.downloadFile(att)}
                    >
                      <span class="file-icon">&#128196;</span>
                      <span class="file-name">{att.filename}</span>
                      <span class="file-size">{formatSize(att.size)}</span>
                    </button>
                  {/if}
                {/each}
              </div>
            {/if}
            {#if msg.content}
              <p class="message-text">{msg.content}</p>
            {/if}
            <div class="message-meta">
              <span class="message-time">{formatTime(msg.timestamp)}</span>
              {#if msg.from_pubkey === node.pubkey}
                {#if msg.read}
                  <span class="delivery-status read" title="Read">Read</span>
                {:else if msg.delivered}
                  <span class="delivery-status delivered" title="Delivered"
                    >Delivered</span
                  >
                {:else if failedIds.has(msg.id)}
                  <button
                    class="delivery-status failed"
                    onclick={() => retryMessage(msg.id)}
                    title="Tap to retry"
                  >
                    Failed -- Tap to retry
                  </button>
                {:else if retryingIds.has(msg.id)}
                  <span class="delivery-status retrying">Retrying...</span>
                {:else}
                  <span class="delivery-status pending" title="Sending..."
                    >Sending</span
                  >
                {/if}
              {/if}
            </div>
          </div>
        </div>
      {:else}
        <div class="empty-chat">
          <p>No messages yet. Say hello!</p>
        </div>
      {/each}

      {#if peerTyping}
        <div class="typing-indicator">
          <span class="typing-name">{peerName}</span> is typing
          <span class="typing-dots">
            <span class="dot"></span>
            <span class="dot"></span>
            <span class="dot"></span>
          </span>
        </div>
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

  .chat-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-base);
    flex-shrink: 0;
  }

  .back-btn {
    color: var(--accent-medium);
    text-decoration: none;
    font-size: var(--text-icon-lg);
    padding: 0.25rem;
  }

  .back-btn:hover {
    color: var(--accent-light);
  }

  .header-info {
    flex: 1;
    min-width: 0;
  }

  .header-name {
    font-weight: 600;
    font-size: var(--text-lg);
    color: var(--text-primary);
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

  .message-row {
    display: flex;
  }

  .message-row.sent {
    justify-content: flex-end;
  }

  .message-row.received {
    justify-content: flex-start;
  }

  .message-bubble {
    max-width: 75%;
    padding: 0.5rem 0.75rem;
    border-radius: var(--radius-2xl);
    word-break: break-word;
  }

  .sent .message-bubble {
    background: var(--accent);
    color: var(--text-on-accent);
    border-bottom-right-radius: var(--radius-sm);
  }

  .received .message-bubble {
    background: var(--bg-surface);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-bottom-left-radius: var(--radius-sm);
  }

  .message-text {
    margin: 0;
    white-space: pre-wrap;
    font-size: var(--text-base);
    line-height: 1.4;
  }

  .message-media {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-bottom: 0.3rem;
  }

  .media-img {
    max-width: 100%;
    max-height: 300px;
    border-radius: var(--radius-lg);
    object-fit: contain;
    cursor: pointer;
  }

  .media-video {
    max-width: 100%;
    max-height: 300px;
    border-radius: var(--radius-lg);
  }

  .audio-attachment {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    width: 100%;
  }

  .audio-filename {
    color: var(--accent-light);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .audio-attachment audio {
    width: 100%;
    height: 36px;
    border-radius: var(--radius-sm);
  }

  .file-attachment {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border-hover);
    border-radius: var(--radius-md);
    padding: 0.4rem 0.6rem;
    color: var(--accent-light);
    font-size: var(--text-base);
    cursor: pointer;
  }

  .file-attachment:hover {
    background: var(--bg-elevated-hover);
  }

  .file-icon {
    font-size: var(--text-icon);
  }

  .file-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-size {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }

  .message-meta {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-top: 0.2rem;
    justify-content: flex-end;
  }

  .message-time {
    font-size: var(--text-xs);
    opacity: 0.6;
  }

  .delivery-status {
    font-size: var(--text-xs);
    opacity: 0.6;
  }

  .delivery-status.delivered {
    color: var(--color-delivered);
  }

  .delivery-status.read {
    color: var(--color-read);
  }

  .failed-msg .message-bubble {
    border: 1px solid var(--danger-bg);
  }

  .delivery-status.failed {
    background: none;
    border: none;
    color: var(--color-error-light);
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 0;
    text-decoration: underline;
    text-decoration-style: dotted;
  }

  .delivery-status.retrying {
    color: var(--color-warning);
  }

  .typing-indicator {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.3rem 0;
    color: var(--text-secondary);
    font-size: var(--text-sm);
    font-style: italic;
  }

  .typing-name {
    color: var(--accent-medium);
    font-style: normal;
    font-weight: 600;
  }

  .typing-dots {
    display: inline-flex;
    gap: 2px;
    margin-left: 2px;
  }

  .dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--text-secondary);
    animation: bounce 1.2s infinite;
  }

  .dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes bounce {
    0%,
    60%,
    100% {
      transform: translateY(0);
    }
    30% {
      transform: translateY(-4px);
    }
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
