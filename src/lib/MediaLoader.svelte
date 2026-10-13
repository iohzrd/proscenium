<script lang="ts" module>
  import { isAudio, isImage, isVideo } from "$lib/utils";

  export type MediaKind = "image" | "video" | "audio" | "file";

  /**
   * Element kind that renders `mime` inline: raster image types, `video/*`
   * and `audio/*`. "file" for every other type, including `image/svg+xml`:
   * those are offered for saving only.
   */
  export function mediaKind(mime: string): MediaKind {
    if (isImage(mime)) return "image";
    if (isVideo(mime)) return "video";
    if (isAudio(mime)) return "audio";
    return "file";
  }
</script>

<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import { requiresClickToLoad, type BlobCache } from "$lib/blobs";
  import type { MediaAttachment } from "$lib/types";
  import { formatSize } from "$lib/utils";

  let {
    att,
    blobs,
    ready,
  }: {
    att: MediaAttachment;
    blobs: BlobCache;
    /** Rendered with the object URL and a function that downloads the blob again. */
    ready: Snippet<[url: string, refetch: () => void]>;
  } = $props();

  type Load =
    | { status: "idle" }
    | { status: "loading" }
    | { status: "ready"; url: string }
    | { status: "error" };

  let load = $state<Load>({ status: "idle" });
  // Incremented per fetch and per attachment change; a fetch assigns its
  // result only while its own value is still the latest.
  let generation = 0;

  function run(fetchUrl: () => Promise<string>) {
    const current = ++generation;
    load = { status: "loading" };
    fetchUrl().then(
      (url) => {
        if (current === generation) load = { status: "ready", url };
      },
      (e) => {
        console.error("Failed to load attachment:", e);
        if (current === generation) load = { status: "error" };
      },
    );
  }

  function fetchCached() {
    run(() => blobs.getBlobUrl(att));
  }

  function refetch() {
    run(() => blobs.refetchBlobUrl(att));
  }

  // Derived primitives: the effect below reruns only when one of these values
  // changes, not when the parent passes an equal attachment object.
  const ticket = $derived(att.ticket);
  const autoFetch = $derived(!requiresClickToLoad(att));

  $effect(() => {
    void ticket;
    const start = autoFetch;
    untrack(() => {
      if (start) {
        fetchCached();
      } else {
        generation++;
        load = { status: "idle" };
      }
    });
    return () => {
      generation++;
    };
  });
</script>

{#if load.status === "ready"}
  {@render ready(load.url, refetch)}
{:else if load.status === "loading"}
  <div class="media-placeholder">Loading...</div>
{:else if load.status === "error"}
  <div class="media-placeholder">
    Failed to load
    <button class="load-btn" onclick={refetch}>Re-download</button>
  </div>
{:else}
  <div class="media-placeholder">
    <span class="load-name">{att.filename}</span>
    <button class="load-btn" onclick={fetchCached}>
      Load ({formatSize(att.size)})
    </button>
  </div>
{/if}

<style>
  .media-placeholder {
    background: var(--bg-deep);
    border-radius: var(--radius-lg);
    padding: 2rem 1rem;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    color: var(--text-tertiary);
    font-size: var(--text-base);
    min-width: 0;
  }

  .load-name {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .load-btn {
    background: var(--accent-medium);
    color: var(--text-primary);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0.3rem 0.75rem;
    font-size: var(--text-sm);
    cursor: pointer;
    transition: opacity var(--transition-fast);
  }

  .load-btn:hover {
    opacity: 0.8;
  }
</style>
