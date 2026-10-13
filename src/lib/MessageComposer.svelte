<script lang="ts">
  import { onDestroy } from "svelte";
  import type { MediaAttachment } from "$lib/types";
  import { MAX_ATTACHMENTS, isImage } from "$lib/utils";
  import { autogrow, useFileUpload } from "$lib/composables";
  import {
    MAX_CONTENT_BYTES,
    isMobilePlatform,
    isSubmitEnter,
    toMediaAttachments,
    utf8Length,
  } from "$lib/messages/compose";

  let {
    onsubmit,
    oninput,
  }: {
    onsubmit: (text: string, media: MediaAttachment[] | null) => Promise<void>;
    oninput?: () => void;
  } = $props();

  const isMobile = isMobilePlatform();
  const upload = useFileUpload();

  let messageText = $state("");
  let sending = $state(false);
  let sendError = $state("");
  let sendErrorTimer: ReturnType<typeof setTimeout> | undefined;
  let fileInput = $state<HTMLInputElement>(null!);
  let cameraInput = $state<HTMLInputElement>(null!);

  let trimmed = $derived(messageText.trim());
  let contentBytes = $derived(utf8Length(trimmed));
  let overLimit = $derived(contentBytes > MAX_CONTENT_BYTES);
  let tooManyAttachments = $derived(
    upload.attachments.length > MAX_ATTACHMENTS,
  );
  // No attachment is added while a message is being sent: `upload.clear()`
  // runs when it succeeds, and an upload that completes later would be
  // appended to the emptied composer.
  let canAttach = $derived(
    !sending &&
      !upload.uploading &&
      upload.attachments.length < MAX_ATTACHMENTS,
  );
  let canSend = $derived(
    !sending &&
      !upload.uploading &&
      !overLimit &&
      !tooManyAttachments &&
      (trimmed.length > 0 || upload.attachments.length > 0),
  );

  async function send() {
    if (!canSend) return;
    sending = true;
    sendError = "";
    clearTimeout(sendErrorTimer);
    try {
      const media = toMediaAttachments(upload.attachments);
      await onsubmit(trimmed, media.length > 0 ? media : null);
      messageText = "";
      upload.clear();
    } catch (e) {
      sendError = String(e);
      sendErrorTimer = setTimeout(() => (sendError = ""), 5000);
    }
    sending = false;
  }

  function handleKeydown(e: KeyboardEvent) {
    if (isSubmitEnter(e, isMobile)) {
      e.preventDefault();
      send();
    }
  }

  export function revokeAttachments() {
    upload.revokeAll();
  }

  onDestroy(() => clearTimeout(sendErrorTimer));
</script>

<input
  type="file"
  multiple
  class="hidden-input"
  bind:this={fileInput}
  onchange={upload.handleFiles}
/>
<input
  type="file"
  accept="image/*,video/*"
  capture="environment"
  class="hidden-input"
  bind:this={cameraInput}
  onchange={upload.handleFiles}
/>

{#if overLimit}
  <div class="send-error">
    {contentBytes}/{MAX_CONTENT_BYTES} bytes
  </div>
{/if}
{#if tooManyAttachments}
  <div class="send-error">
    {upload.attachments.length} attachments; the limit is {MAX_ATTACHMENTS}.
  </div>
{/if}
{#if upload.errorMessage}
  <div class="send-error" role="alert">{upload.errorMessage}</div>
{/if}
{#if sendError}
  <div class="send-error" role="alert">{sendError}</div>
{/if}

{#if upload.attachments.length > 0}
  <div class="attachment-preview">
    {#each upload.attachments as att, i}
      <div class="attachment-item">
        {#if isImage(att.mime_type)}
          <img src={att.previewUrl} alt={att.filename} />
        {:else}
          <span class="att-file">{att.filename}</span>
        {/if}
        <button
          class="att-remove"
          onclick={() => upload.removeAttachment(i)}
          disabled={sending}
          aria-label="Remove attachment">x</button
        >
      </div>
    {/each}
  </div>
{/if}

<div class="compose-bar">
  {#if isMobile}
    <button
      class="attach-btn"
      onclick={() => cameraInput?.click()}
      disabled={!canAttach}
      title="Take photo"
    >
      {upload.uploading ? "..." : "Cam"}
    </button>
  {/if}
  <button
    class="attach-btn"
    onclick={() => fileInput?.click()}
    disabled={!canAttach}
    title="Attach file"
    aria-label="Attach file"
  >
    {upload.uploading ? "..." : "+"}
  </button>
  <textarea
    class="input-base compose-input"
    placeholder="Type a message..."
    bind:value={messageText}
    onkeydown={handleKeydown}
    oninput={() => oninput?.()}
    maxlength={MAX_CONTENT_BYTES}
    rows="1"
    use:autogrow></textarea>
  <button class="btn-accent send-btn" onclick={send} disabled={!canSend}>
    {sending ? "..." : "Send"}
  </button>
</div>

<style>
  .send-error {
    background: var(--color-error-light-bg);
    color: var(--color-error-light);
    font-size: var(--text-base);
    padding: 0.4rem 1rem;
    border-top: 1px solid var(--color-error-light-border);
  }

  .attachment-preview {
    display: flex;
    gap: 0.5rem;
    padding: 0.5rem 1rem;
    border-top: 1px solid var(--border);
    background: var(--bg-surface);
    overflow-x: auto;
    flex-shrink: 0;
  }

  .attachment-item {
    position: relative;
    flex-shrink: 0;
  }

  .attachment-item img {
    width: 60px;
    height: 60px;
    object-fit: cover;
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
  }

  .att-file {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 60px;
    height: 60px;
    background: var(--bg-elevated);
    border-radius: var(--radius-md);
    font-size: var(--text-xs);
    color: var(--text-secondary);
    text-align: center;
    padding: 4px;
    word-break: break-all;
    overflow: hidden;
  }

  .att-remove {
    position: absolute;
    top: -6px;
    right: -6px;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--color-error-light);
    color: var(--text-on-accent);
    border: none;
    font-size: var(--text-sm);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
  }

  .compose-bar {
    display: flex;
    align-items: flex-end;
    gap: 0.5rem;
    padding: 0.75rem 1rem;
    padding-bottom: calc(0.75rem + env(safe-area-inset-bottom));
    border-top: 1px solid var(--border);
    background: var(--bg-base);
    flex-shrink: 0;
  }

  .attach-btn {
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    color: var(--accent-medium);
    font-size: var(--text-xl);
    width: 36px;
    height: 36px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .attach-btn:hover:not(:disabled) {
    background: var(--bg-elevated);
    color: var(--accent-light);
  }

  .compose-input {
    flex: 1;
    border-radius: var(--radius-lg);
    font-size: var(--text-base);
    resize: none;
    min-height: 36px;
    max-height: 40vh;
  }

  .send-btn {
    border-radius: var(--radius-lg);
  }
</style>
