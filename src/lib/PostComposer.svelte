<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { platform } from "@tauri-apps/plugin-os";
  import MentionAutocomplete from "$lib/MentionAutocomplete.svelte";
  import type { PendingAttachment } from "$lib/types";
  import { isImage, isVideo, uploadFiles } from "$lib/utils";

  const MAX_POST_LENGTH = 10_000;
  const isMobile = platform() === "android" || platform() === "ios";

  let {
    nodeId,
    onsubmitted,
  }: {
    nodeId: string;
    onsubmitted: () => void;
  } = $props();

  let newPost = $state("");
  let posting = $state(false);
  let attachments = $state<PendingAttachment[]>([]);
  let uploading = $state(false);
  let fileInput = $state<HTMLInputElement>(null!);
  let cameraInput = $state<HTMLInputElement>(null!);
  let mentionQuery = $state("");
  let mentionActive = $state(false);
  let mentionAutocomplete = $state<MentionAutocomplete>();
  let errorMessage = $state("");

  async function handleFiles(e: Event) {
    const input = e.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) return;
    uploading = true;
    try {
      const uploaded = await uploadFiles(files);
      attachments = [...attachments, ...uploaded];
    } catch (e) {
      errorMessage = `Failed to upload file`;
      console.error("Failed to upload file:", e);
      setTimeout(() => (errorMessage = ""), 4000);
    }
    uploading = false;
    input.value = "";
  }

  function removeAttachment(index: number) {
    const removed = attachments[index];
    if (removed) URL.revokeObjectURL(removed.previewUrl);
    attachments = attachments.filter((_, i) => i !== index);
  }

  async function submitPost() {
    if ((!newPost.trim() && attachments.length === 0) || posting) return;
    posting = true;
    try {
      const media = attachments.map(
        ({ hash, ticket, mime_type, filename, size }) => ({
          hash,
          ticket,
          mime_type,
          filename,
          size,
        }),
      );
      await invoke("create_post", {
        content: newPost,
        media: media.length > 0 ? media : null,
      });
      for (const a of attachments) URL.revokeObjectURL(a.previewUrl);
      newPost = "";
      attachments = [];
      onsubmitted();
    } catch (e) {
      errorMessage = "Failed to create post";
      console.error("Failed to create post:", e);
      setTimeout(() => (errorMessage = ""), 4000);
    }
    posting = false;
  }

  function handleMentionInput(e: Event) {
    const textarea = e.target as HTMLTextAreaElement;
    const cursorPos = textarea.selectionStart;
    const textBeforeCursor = textarea.value.slice(0, cursorPos);
    const match = textBeforeCursor.match(/@(\w*)$/);
    if (match) {
      mentionActive = true;
      mentionQuery = match[1];
    } else {
      mentionActive = false;
      mentionQuery = "";
    }
  }

  function insertMention(pubkey: string) {
    const textarea = document.querySelector(
      ".compose textarea",
    ) as HTMLTextAreaElement;
    const cursorPos = textarea.selectionStart;
    const textBeforeCursor = newPost.slice(0, cursorPos);
    const textAfterCursor = newPost.slice(cursorPos);
    const match = textBeforeCursor.match(/@(\w*)$/);
    if (match) {
      const beforeMention = textBeforeCursor.slice(0, match.index);
      newPost = `${beforeMention}@${pubkey} ${textAfterCursor}`;
    }
    mentionActive = false;
    mentionQuery = "";
    textarea.focus();
  }

  function handleKey(e: KeyboardEvent) {
    if (mentionAutocomplete?.handleKey(e)) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submitPost();
    }
  }
</script>

<div class="compose">
  <MentionAutocomplete
    bind:this={mentionAutocomplete}
    query={mentionQuery}
    selfId={nodeId}
    visible={mentionActive}
    onselect={insertMention}
  />
  <textarea
    class="input-base compose-textarea"
    bind:value={newPost}
    placeholder="What's on your mind?"
    rows="3"
    maxlength={MAX_POST_LENGTH}
    onkeydown={handleKey}
    oninput={handleMentionInput}
  ></textarea>
  <div class="compose-meta">
    <span class="hint">Shift+Enter for newline</span>
    <span
      class="char-count"
      class:warn={newPost.length > MAX_POST_LENGTH * 0.9}
    >
      {newPost.length}/{MAX_POST_LENGTH}
    </span>
  </div>

  {#if attachments.length > 0}
    <div class="attachment-previews">
      {#each attachments as att, i}
        <div class="attachment-preview">
          {#if isImage(att.mime_type)}
            <img src={att.previewUrl} alt={att.filename} />
          {:else if isVideo(att.mime_type)}
            <video src={att.previewUrl} muted></video>
          {:else}
            <div class="file-icon">{att.filename}</div>
          {/if}
          <button
            class="remove-btn"
            onclick={() => removeAttachment(i)}
            aria-label="Remove attachment">&times;</button
          >
        </div>
      {/each}
    </div>
  {/if}

  {#if errorMessage}
    <p class="compose-error">{errorMessage}</p>
  {/if}

  <div class="compose-actions">
    {#if isMobile}
      <button
        class="btn-elevated attach-btn"
        onclick={() => cameraInput.click()}
        disabled={uploading}
      >
        {uploading ? "..." : "Camera"}
      </button>
      <button
        class="btn-elevated attach-btn"
        onclick={() => fileInput.click()}
        disabled={uploading}
      >
        {uploading ? "..." : "Gallery"}
      </button>
    {:else}
      <button
        class="btn-elevated attach-btn"
        onclick={() => fileInput.click()}
        disabled={uploading}
      >
        {uploading ? "Uploading..." : "Attach"}
      </button>
    {/if}
    <input
      bind:this={cameraInput}
      type="file"
      accept="image/*,video/*"
      capture="environment"
      onchange={handleFiles}
      hidden
    />
    <input
      bind:this={fileInput}
      type="file"
      multiple
      accept="image/*,video/*,audio/*,.pdf,.txt"
      onchange={handleFiles}
      hidden
    />
    <button
      class="btn-accent post-btn"
      onclick={submitPost}
      disabled={posting || (!newPost.trim() && attachments.length === 0)}
    >
      {posting ? "Posting..." : "Post"}
    </button>
  </div>
</div>

<style>
  .compose {
    position: relative;
    margin-bottom: 1.25rem;
  }

  .compose-textarea {
    border-radius: var(--radius-xl);
    padding: 0.75rem;
    font-size: var(--text-lg);
    resize: vertical;
  }

  .compose-meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 0.25rem;
  }

  .hint {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .char-count {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .char-count.warn {
    color: var(--color-warning);
  }

  .compose-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }

  .attach-btn {
    border-radius: var(--radius-lg);
    padding: 0.55rem 1rem;
    font-size: var(--text-base);
    font-weight: 500;
  }

  .post-btn {
    flex: 1;
    border-radius: var(--radius-lg);
    padding: 0.55rem;
    font-size: var(--text-base);
  }

  .attachment-previews {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
    flex-wrap: wrap;
  }

  .attachment-preview {
    position: relative;
    width: 80px;
    height: 80px;
    border-radius: var(--radius-md);
    overflow: hidden;
    border: 1px solid var(--border);
  }

  .attachment-preview img,
  .attachment-preview video {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .attachment-preview .file-icon {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg-deep);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    text-align: center;
    padding: 0.25rem;
    word-break: break-all;
  }

  .remove-btn {
    position: absolute;
    top: -4px;
    right: -4px;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--overlay-medium);
    color: var(--text-on-accent);
    border: none;
    font-size: var(--text-base);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
  }

  .compose-error {
    color: var(--color-error-light);
    font-size: var(--text-sm);
    margin: 0.25rem 0 0;
  }
</style>
