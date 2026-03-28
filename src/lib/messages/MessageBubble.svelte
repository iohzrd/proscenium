<script lang="ts">
  import type { StoredMessage } from "$lib/types";
  import type { BlobCache } from "$lib/blobs";
  import MessageMedia from "./MessageMedia.svelte";

  let {
    msg,
    isSent,
    isFailed,
    isRetrying,
    blobs,
    onretry,
  }: {
    msg: StoredMessage;
    isSent: boolean;
    isFailed: boolean;
    isRetrying: boolean;
    blobs: BlobCache;
    onretry: (msgId: string) => void;
  } = $props();

  function formatTime(ts: number): string {
    return new Date(ts).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
  }
</script>

<div
  class="message-row"
  class:sent={isSent}
  class:received={!isSent}
  class:failed-msg={isSent && isFailed}
>
  <div class="message-bubble">
    {#if msg.media && msg.media.length > 0}
      <MessageMedia media={msg.media} {blobs} />
    {/if}
    {#if msg.content}
      <p class="message-text">{msg.content}</p>
    {/if}
    <div class="message-meta">
      <span class="message-time">{formatTime(msg.timestamp)}</span>
      {#if isSent}
        {#if msg.read}
          <span class="delivery-status read" title="Read">Read</span>
        {:else if msg.delivered}
          <span class="delivery-status delivered" title="Delivered"
            >Delivered</span
          >
        {:else if isFailed}
          <button
            class="delivery-status failed"
            onclick={() => onretry(msg.id)}
            title="Tap to retry"
          >
            Failed -- Tap to retry
          </button>
        {:else if isRetrying}
          <span class="delivery-status retrying">Retrying...</span>
        {:else}
          <span class="delivery-status pending" title="Sending...">Sending</span
          >
        {/if}
      {/if}
    </div>
  </div>
</div>

<style>
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
</style>
