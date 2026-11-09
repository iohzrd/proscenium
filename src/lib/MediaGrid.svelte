<script lang="ts">
  import { getBlobContext } from "#lib/blobs.ts";
  import { showContextMenu } from "#lib/context-menu.ts";
  import MediaLoader, { mediaKind } from "#lib/MediaLoader.svelte";
  import type { MediaAttachment } from "#lib/types.ts";
  import { formatSize } from "#lib/utils.ts";

  let {
    media,
    onlightbox,
  }: {
    media: MediaAttachment[];
    onlightbox?: (src: string, alt: string, att: MediaAttachment) => void;
  } = $props();

  const blobs = getBlobContext();

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

{#if media.length > 0}
  <!-- Not keyed: two attachments of one post can carry the same hash. -->
  <div class="media-grid" class:grid={media.length > 1}>
    {#each media as att}
      {@const kind = mediaKind(att.mime_type)}
      {#if kind === "image"}
        <div class="media-item">
          <MediaLoader {att} {blobs}>
            {#snippet ready(url)}
              <button
                class="media-img-btn"
                onclick={() => onlightbox?.(url, att.filename, att)}
                oncontextmenu={(e) =>
                  handleContextMenu(e, att, "Save Image As...")}
              >
                <img src={url} alt={att.filename} class="media-img" />
              </button>
              <button
                class="save-as-overlay"
                onclick={() => blobs.saveFileAs(att)}>Save As</button
              >
            {/snippet}
          </MediaLoader>
        </div>
      {:else if kind === "video"}
        <div class="media-item">
          <MediaLoader {att} {blobs}>
            {#snippet ready(url, refetch)}
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <video
                src={url}
                controls
                class="media-video"
                oncontextmenu={(e) =>
                  handleContextMenu(e, att, "Save Video As...")}
              >
                <track kind="captions" />
              </video>
              <div class="media-actions">
                <button class="retry-btn" onclick={refetch}>Re-download</button>
                <button
                  class="save-as-btn"
                  onclick={() => blobs.saveFileAs(att)}>Save As</button
                >
              </div>
            {/snippet}
          </MediaLoader>
        </div>
      {:else if kind === "audio"}
        <MediaLoader {att} {blobs}>
          {#snippet ready(url)}
            <div class="media-audio">
              <span class="audio-filename">{att.filename}</span>
              <audio src={url} controls preload="metadata"></audio>
              <button class="save-as-btn" onclick={() => blobs.saveFileAs(att)}
                >Save As</button
              >
            </div>
          {/snippet}
        </MediaLoader>
      {:else}
        <button class="media-file" onclick={() => blobs.saveFileAs(att)}>
          <span class="file-name">{att.filename}</span>
          <span class="file-size">{formatSize(att.size)}</span>
          <span class="download-label">Save As</span>
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .media-grid {
    margin-top: 0.75rem;
  }

  .media-grid.grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    grid-auto-rows: 1fr;
    gap: 0.5rem;
  }

  .media-item {
    position: relative;
    overflow: hidden;
    border-radius: var(--radius-lg);
    background: var(--bg-deep);
  }

  .grid .media-item {
    height: 100%;
  }

  .media-img-btn {
    background: none;
    border: none;
    padding: 0;
    cursor: zoom-in;
    display: block;
    width: 100%;
    height: 100%;
    transition: opacity var(--transition-fast);
  }

  .media-img-btn:hover {
    opacity: 0.85;
  }

  .media-img {
    width: 100%;
    max-height: 400px;
    object-fit: contain;
    background: var(--bg-deep);
    display: block;
    border-radius: var(--radius-lg);
  }

  .grid .media-img {
    height: 100%;
    max-height: none;
    object-fit: cover;
  }

  .save-as-overlay {
    position: absolute;
    top: 0.5rem;
    right: 0.5rem;
    background: var(--overlay-medium);
    border: 1px solid var(--overlay-white-subtle);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-size: var(--text-xs, 0.7rem);
    padding: 0.25rem 0.5rem;
    cursor: pointer;
    opacity: 0;
    z-index: 1;
    transition: opacity var(--transition-fast);
  }

  .media-item:hover .save-as-overlay {
    opacity: 1;
  }

  .save-as-overlay:hover {
    background: var(--overlay-dark);
    border-color: var(--accent-medium);
  }

  .media-video {
    width: 100%;
    border-radius: var(--radius-lg);
    max-height: 400px;
  }

  .media-audio {
    background: var(--bg-deep);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.5rem 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .audio-filename {
    color: var(--accent-light);
    font-size: var(--text-base);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .media-audio audio {
    width: 100%;
    height: 36px;
    border-radius: var(--radius-sm);
  }

  .media-file {
    background: var(--bg-deep);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem;
    display: flex;
    justify-content: space-between;
    align-items: center;
    color: var(--accent-light);
    font-size: var(--text-base);
    cursor: pointer;
    flex: 1;
    gap: 0.5rem;
    min-width: 0;
    transition: border-color var(--transition-normal);
  }

  .media-file:hover {
    border-color: var(--accent-medium);
  }

  .file-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-size {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }

  .download-label {
    color: var(--color-link);
    font-size: var(--text-sm);
    flex-shrink: 0;
  }

  .retry-btn {
    background: var(--accent-medium);
    color: var(--text-primary);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0.15rem 0.5rem;
    font-size: var(--text-xs, 0.7rem);
    cursor: pointer;
    opacity: 0.6;
    transition: opacity var(--transition-fast);
  }

  .retry-btn:hover {
    opacity: 0.8;
  }

  .media-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.25rem;
  }

  .save-as-btn {
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    font-size: var(--text-xs, 0.7rem);
    padding: 0.2rem 0.5rem;
    cursor: pointer;
    white-space: nowrap;
    transition:
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .save-as-btn:hover {
    border-color: var(--accent-medium);
    color: var(--accent-light);
  }
</style>
