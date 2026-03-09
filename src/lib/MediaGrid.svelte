<script lang="ts">
  import { getBlobContext } from "$lib/blobs";
  import type { MediaAttachment } from "$lib/types";
  import { isImage, isVideo, isAudio, formatSize } from "$lib/utils";

  let {
    media,
    onlightbox,
  }: {
    media: MediaAttachment[];
    onlightbox?: (src: string, alt: string) => void;
  } = $props();

  const { getBlobUrl, refetchBlobUrl, downloadFile } = getBlobContext();

  let refetching = $state<Record<string, Promise<string> | undefined>>({});
</script>

{#if media.length > 0}
  <div class="media-grid" class:grid={media.length > 1}>
    {#each media as att (att.hash)}
      {#if isImage(att.mime_type)}
        {#await refetching[att.hash] ?? getBlobUrl(att)}
          <div class="media-placeholder">Loading...</div>
        {:then url}
          <button
            class="media-img-btn"
            onclick={() => onlightbox?.(url, att.filename)}
          >
            <img src={url} alt={att.filename} class="media-img" />
          </button>
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
      {:else if isVideo(att.mime_type)}
        {#await refetching[att.hash] ?? getBlobUrl(att)}
          <div class="media-placeholder">Loading...</div>
        {:then url}
          <video src={url} controls class="media-video">
            <track kind="captions" />
          </video>
          <button
            class="retry-btn retry-btn-inline"
            onclick={() => {
              refetching[att.hash] = refetchBlobUrl(att);
            }}>Re-download</button
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
      {:else if isAudio(att.mime_type)}
        {#await refetching[att.hash] ?? getBlobUrl(att)}
          <div class="media-placeholder">Loading...</div>
        {:then url}
          <div class="media-audio">
            <span class="audio-filename">{att.filename}</span>
            <audio src={url} controls preload="metadata"></audio>
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
        <button class="media-file" onclick={() => downloadFile(att)}>
          <span>{att.filename}</span>
          <span class="file-size">{formatSize(att.size)}</span>
          <span class="download-label">Download</span>
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
    gap: 0.5rem;
  }

  .media-img-btn {
    background: none;
    border: none;
    padding: 0;
    cursor: zoom-in;
    display: block;
    width: 100%;
    transition: opacity var(--transition-fast);
  }

  .media-img-btn:hover {
    opacity: 0.85;
  }

  .media-img {
    width: 100%;
    border-radius: var(--radius-lg);
    max-height: 400px;
    object-fit: contain;
    background: var(--bg-deep);
    display: block;
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
    width: 100%;
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
    margin-top: 0.25rem;
    font-size: var(--text-xs, 0.7rem);
    padding: 0.15rem 0.5rem;
    opacity: 0.6;
  }
</style>
