<script lang="ts">
  import type { MediaAttachment } from "$lib/types";
  import type { BlobCache } from "$lib/blobs";
  import { showContextMenu } from "$lib/context-menu";
  import MediaLoader, { mediaKind } from "$lib/MediaLoader.svelte";
  import { formatSize } from "$lib/utils";

  let {
    media,
    blobs,
  }: {
    media: MediaAttachment[];
    blobs: BlobCache;
  } = $props();

  // Replaces the webview's default menu, which offers to open the blob URL.
  function handleContextMenu(
    e: MouseEvent,
    att: MediaAttachment,
    label: string,
  ) {
    showContextMenu(e, [
      { text: label, action: () => blobs.saveFileAs(att) },
    ]).catch((err) => console.error("Failed to show context menu:", err));
  }
</script>

<div class="message-media">
  {#each media as att}
    {@const kind = mediaKind(att.mime_type)}
    {#if kind === "image"}
      <MediaLoader {att} {blobs}>
        {#snippet ready(url)}
          <img
            src={url}
            alt={att.filename}
            class="media-img"
            oncontextmenu={(e) => handleContextMenu(e, att, "Save Image As...")}
          />
        {/snippet}
      </MediaLoader>
      <button class="dm-save-as-btn" onclick={() => blobs.saveFileAs(att)}
        >Save As</button
      >
    {:else if kind === "video"}
      <MediaLoader {att} {blobs}>
        {#snippet ready(url)}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <video
            src={url}
            controls
            class="media-video"
            preload="metadata"
            oncontextmenu={(e) => handleContextMenu(e, att, "Save Video As...")}
          >
            <track kind="captions" />
          </video>
        {/snippet}
      </MediaLoader>
      <button class="dm-save-as-btn" onclick={() => blobs.saveFileAs(att)}
        >Save As</button
      >
    {:else if kind === "audio"}
      <div class="audio-attachment">
        <span class="audio-filename">{att.filename}</span>
        <MediaLoader {att} {blobs}>
          {#snippet ready(url)}
            <audio src={url} controls preload="metadata"></audio>
          {/snippet}
        </MediaLoader>
        <button class="dm-save-as-btn" onclick={() => blobs.saveFileAs(att)}
          >Save As</button
        >
      </div>
    {:else}
      <button class="file-attachment" onclick={() => blobs.saveFileAs(att)}>
        <span class="file-icon">&#128196;</span>
        <span class="file-name">{att.filename}</span>
        <span class="file-size">{formatSize(att.size)}</span>
      </button>
    {/if}
  {/each}
</div>

<style>
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

  .dm-save-as-btn {
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    font-size: var(--text-xs, 0.7rem);
    padding: 0.15rem 0.4rem;
    cursor: pointer;
    margin-top: 0.2rem;
    transition:
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .dm-save-as-btn:hover {
    border-color: var(--accent-medium);
    color: var(--accent-light);
  }
</style>
