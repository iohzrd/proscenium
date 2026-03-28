<script lang="ts">
  import type { MediaAttachment } from "$lib/types";
  import type { BlobCache } from "$lib/blobs";
  import { isImage, isVideo, isAudio, formatSize } from "$lib/utils";

  let {
    media,
    blobs,
  }: {
    media: MediaAttachment[];
    blobs: BlobCache;
  } = $props();
</script>

<div class="message-media">
  {#each media as att}
    {#if isImage(att.mime_type)}
      {#await blobs.getBlobUrl(att) then url}
        <img src={url} alt={att.filename} class="media-img" />
      {/await}
      <button class="dm-save-as-btn" onclick={() => blobs.saveFileAs(att)}
        >Save As</button
      >
    {:else if isVideo(att.mime_type)}
      {#await blobs.getBlobUrl(att) then url}
        <video src={url} controls class="media-video" preload="metadata"
        ></video>
      {/await}
      <button class="dm-save-as-btn" onclick={() => blobs.saveFileAs(att)}
        >Save As</button
      >
    {:else if isAudio(att.mime_type)}
      {#await blobs.getBlobUrl(att) then url}
        <div class="audio-attachment">
          <span class="audio-filename">{att.filename}</span>
          <audio src={url} controls preload="metadata"></audio>
          <button class="dm-save-as-btn" onclick={() => blobs.saveFileAs(att)}
            >Save As</button
          >
        </div>
      {/await}
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
