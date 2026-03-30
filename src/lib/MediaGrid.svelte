<script lang="ts">
  import { getBlobContext } from "$lib/blobs";
  import { showContextMenu } from "$lib/context-menu";
  import type { MediaAttachment } from "$lib/types";
  import { isImage, isVideo, isAudio, formatSize } from "$lib/utils";

  let {
    media,
    onlightbox,
  }: {
    media: MediaAttachment[];
    onlightbox?: (src: string, alt: string, att: MediaAttachment) => void;
  } = $props();

  const { getBlobUrl, refetchBlobUrl, saveFileAs } = getBlobContext();

  let refetching = $state<Record<string, Promise<string> | undefined>>({});

  function handleContextMenu(e: MouseEvent, att: MediaAttachment) {
    showContextMenu(e, [
      { text: "Save Image As...", action: () => saveFileAs(att) },
    ]);
  }
</script>

{#if media.length > 0}
  <div class="media-grid" class:grid={media.length > 1}>
    {#each media as att (att.hash)}
      {#if isImage(att.mime_type)}
        <div class="media-item">
          {#await refetching[att.hash] ?? getBlobUrl(att)}
            <div class="media-placeholder">Loading...</div>
          {:then url}
            <button
              class="media-img-btn"
              onclick={() => onlightbox?.(url, att.filename, att)}
              oncontextmenu={(e) => handleContextMenu(e, att)}
            >
              <img src={url} alt={att.filename} class="media-img" />
            </button>
            <button class="save-as-overlay" onclick={() => saveFileAs(att)}
              >Save As</button
            >
          {:catch}
            <div class="media-placeholder">
              Failed to load
              <button
                class="retry-btn"
                onclick={() => {
                  refetching[att.hash] = refetchBlobUrl(att);
                }}>Re-download</button
              >
            </div>
          {/await}
        </div>
      {:else if isVideo(att.mime_type)}
        <div class="media-item">
          {#await refetching[att.hash] ?? getBlobUrl(att)}
            <div class="media-placeholder">Loading...</div>
          {:then url}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <video
              src={url}
              controls
              class="media-video"
              oncontextmenu={(e) => handleContextMenu(e, att)}
            >
              <track kind="captions" />
            </video>
            <div class="media-actions">
              <button
                class="retry-btn retry-btn-inline"
                onclick={() => {
                  refetching[att.hash] = refetchBlobUrl(att);
                }}>Re-download</button
              >
              <button class="save-as-btn" onclick={() => saveFileAs(att)}
                >Save As</button
              >
            </div>
          {:catch}
            <div class="media-placeholder">
              Failed to load
              <button
                class="retry-btn"
                onclick={() => {
                  refetching[att.hash] = refetchBlobUrl(att);
                }}>Re-download</button
              >
            </div>
          {/await}
        </div>
      {:else if isAudio(att.mime_type)}
        {#await refetching[att.hash] ?? getBlobUrl(att)}
          <div class="media-placeholder">Loading...</div>
        {:then url}
          <div class="media-audio">
            <span class="audio-filename">{att.filename}</span>
            <audio src={url} controls preload="metadata"></audio>
            <button class="save-as-btn" onclick={() => saveFileAs(att)}
              >Save As</button
            >
          </div>
        {:catch}
          <div class="media-placeholder">
            Failed to load
            <button
              class="retry-btn"
              onclick={() => {
                refetching[att.hash] = refetchBlobUrl(att);
              }}>Re-download</button
            >
          </div>
        {/await}
      {:else}
        <button class="media-file" onclick={() => saveFileAs(att)}>
          <span>{att.filename}</span>
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

  .media-placeholder {
    background: var(--bg-deep);
    border-radius: var(--radius-lg);
    padding: 2rem;
    text-align: center;
    color: var(--text-tertiary);
    font-size: var(--text-base);
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
    min-width: 0;
    transition: border-color var(--transition-normal);
  }

  .media-file:hover {
    border-color: var(--accent-medium);
  }

  .file-size {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
  }

  .download-label {
    color: var(--color-link);
    font-size: var(--text-sm);
  }

  .retry-btn {
    background: var(--accent-medium);
    color: var(--text-primary);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0.3rem 0.75rem;
    font-size: var(--text-sm);
    cursor: pointer;
    margin-top: 0.5rem;
    transition: opacity var(--transition-fast);
  }

  .retry-btn:hover {
    opacity: 0.8;
  }

  .retry-btn-inline {
    margin-top: 0;
    font-size: var(--text-xs, 0.7rem);
    padding: 0.15rem 0.5rem;
    opacity: 0.6;
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
