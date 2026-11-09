<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Lightbox from "#lib/Lightbox.svelte";
  import PostFeed from "#lib/PostFeed.svelte";
  import StageCard from "#lib/StageCard.svelte";
  import DeleteConfirmModal from "#lib/DeleteConfirmModal.svelte";
  import PostComposer from "#lib/PostComposer.svelte";
  import { createBlobCache, setBlobContext } from "#lib/blobs.ts";
  import { hapticImpact } from "#lib/haptics.ts";
  import { liveStages } from "#lib/stage.svelte.ts";
  import type { Post } from "#lib/types.ts";
  import { coalesce, dedupeById, mergeFirstPage, shortId } from "#lib/utils.ts";
  import {
    useToast,
    useCopyFeedback,
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useDeleteConfirm,
    useLightbox,
    usePullToRefresh,
  } from "#lib/composables/index.ts";

  const PAGE_SIZE = 20;
  // Largest number of rows one feed refresh reads again.
  const REFRESH_MAX = 100;
  // Interval between automatic sync requests while the window is visible: 10 minutes.
  const AUTO_SYNC_INTERVAL_MS = 600_000;
  // A change to visible requests a sync only if the last request is at least this old.
  const VISIBILITY_SYNC_MIN_INTERVAL_MS = 60_000;
  // Shortest time between two sync requests from any trigger.
  const SYNC_REQUEST_MIN_INTERVAL_MS = 10_000;

  let syncing = $state(false);
  let lastSyncRequestedAt = 0;
  let posts = $state<Post[]>([]);
  let showScrollTop = $state(false);
  let sentinel = $state<HTMLDivElement>(null!);

  const toast = useToast();
  const blobs = createBlobCache({ onError: toast.show });
  setBlobContext(blobs);

  const copyFb = useCopyFeedback();
  const lightbox = useLightbox();
  const del = useDeleteConfirm(async (id) => {
    try {
      await invoke("delete_post", { id });
      await refreshFeed();
    } catch (e) {
      toast.show("Failed to delete post");
      console.error("Failed to delete post:", e);
    }
  });

  const node = useNodeInit(async () => {
    const profile = await invoke("get_my_profile");
    if (!profile) {
      goto("/welcome");
      return;
    }
    await refreshFeed();
  });

  /**
   * Read the first rows of the feed again and merge them into `posts` by id.
   * Rows loaded by pagination beyond the refreshed range are kept.
   */
  const refreshFeed = coalesce(async () => {
    const limit = Math.min(Math.max(PAGE_SIZE, posts.length), REFRESH_MAX);
    try {
      const fresh: Post[] = await invoke("get_feed", { limit });
      const merged = mergeFirstPage(posts, fresh, limit, "desc");
      posts = merged.items;
      if (merged.exhausted) scroll.setNoMore();
      else if (!merged.keptExisting) scroll.reset();
    } catch (e) {
      toast.show("Failed to load feed");
      console.error("Failed to load feed:", e);
    }
  });

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      const last = posts[posts.length - 1];
      if (!last) return false;
      try {
        const older: Post[] = await invoke("get_feed", {
          limit: PAGE_SIZE,
          before: last.timestamp,
          beforeId: last.id,
        });
        // The cursor row is no longer the last row: this page is outdated.
        if (posts[posts.length - 1]?.id !== last.id) return;
        const known = new Set(posts.map((p) => p.id));
        const added = dedupeById(older).filter((p) => !known.has(p.id));
        if (added.length === 0) {
          scroll.setNoMore();
          return;
        }
        posts = [...posts, ...added];
        scroll.setHasMore(older.length);
      } catch (e) {
        toast.show("Failed to load more posts");
        console.error("Failed to load more:", e);
        return false;
      }
    },
    PAGE_SIZE,
  );

  /**
   * Ask the backend to sync every followed peer, then read the feed again.
   * `sync_all_peers` returns when the request is queued, so the backend can
   * still be syncing afterwards; posts it stores later reach the list through
   * `feed-updated`. A call made while a request is pending, or less than
   * `SYNC_REQUEST_MIN_INTERVAL_MS` after the last one, only reads the feed.
   */
  async function requestSync() {
    if (syncing) return;
    if (Date.now() - lastSyncRequestedAt < SYNC_REQUEST_MIN_INTERVAL_MS) {
      await refreshFeed();
      return;
    }
    syncing = true;
    lastSyncRequestedAt = Date.now();
    try {
      await invoke("sync_all_peers");
      await refreshFeed();
    } catch (e) {
      toast.show("Sync failed");
      console.error("Failed to sync:", e);
    } finally {
      syncing = false;
    }
  }

  const pull = usePullToRefresh(async () => {
    hapticImpact("medium");
    await requestSync();
  });

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape" && del.pendingId) {
      del.cancel();
    }
  }

  function scrollToTop() {
    window.scrollTo({ top: 0, behavior: "smooth" });
  }

  function handleScroll() {
    showScrollTop = window.scrollY > 400;
  }

  // Visibility-aware auto-sync
  let syncInterval: ReturnType<typeof setInterval> | null = null;

  function startAutoSync() {
    if (syncInterval) return;
    syncInterval = setInterval(requestSync, AUTO_SYNC_INTERVAL_MS);
  }

  function stopAutoSync() {
    if (syncInterval) {
      clearInterval(syncInterval);
      syncInterval = null;
    }
  }

  function handleVisibility() {
    if (document.hidden) {
      stopAutoSync();
      return;
    }
    startAutoSync();
    if (Date.now() - lastSyncRequestedAt >= VISIBILITY_SYNC_MIN_INTERVAL_MS) {
      requestSync();
    }
  }

  $effect(() => {
    return scroll.setupEffect();
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "feed-updated": () => {
        refreshFeed();
      },
    });
    window.addEventListener("scroll", handleScroll);
    window.addEventListener("keydown", handleGlobalKey);
    document.addEventListener("visibilitychange", handleVisibility);
    startAutoSync();
    return () => {
      stopAutoSync();
      document.removeEventListener("visibilitychange", handleVisibility);
      window.removeEventListener("keydown", handleGlobalKey);
      window.removeEventListener("scroll", handleScroll);
      cleanupListeners();
      blobs.revokeAll();
    };
  });
</script>

{#if lightbox.src}
  <Lightbox
    src={lightbox.src}
    alt={lightbox.alt}
    attachment={lightbox.attachment}
    onclose={lightbox.close}
  />
{/if}

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Starting node...</p>
  </div>
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
  </div>
{:else}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    ontouchstart={pull.handleTouchStart}
    ontouchmove={pull.handleTouchMove}
    ontouchend={pull.handleTouchEnd}
    style="transform: translateY({pull.pullDistance}px); transition: {pull.isPulling
      ? 'none'
      : 'transform 0.3s ease-out'};"
  >
    {#if pull.pullDistance > 0}
      <div class="pull-indicator" style="height: {pull.pullDistance}px;">
        <div
          class="pull-arrow"
          class:ready={pull.pullTriggered}
          style="transform: rotate({pull.pullDistance * 3}deg);"
        >
          &#8635;
        </div>
        <span class="pull-text">
          {pull.pullTriggered ? "Release to refresh" : "Pull to refresh"}
        </span>
      </div>
    {/if}
    <div class="identity-card">
      <div class="key-row">
        <span class="key-label">Node ID</span>
        <code class="key-value">{shortId(node.nodeId)}</code>
        <button
          class="btn-elevated copy-btn"
          onclick={() => copyFb.copy(node.nodeId, "node-id")}
        >
          {copyFb.feedback === "node-id" ? "Copied!" : "Copy"}
        </button>
      </div>
    </div>

    <PostComposer pubkey={node.pubkey} onsubmitted={refreshFeed} />

    <hr class="divider" />

    {#if liveStages.size > 0}
      <div class="live-stages">
        {#each [...liveStages.values()] as ann (ann.stage_id)}
          <StageCard announcement={ann} />
        {/each}
      </div>
    {/if}

    <PostFeed
      {posts}
      pubkey={node.pubkey}
      showDelete={true}
      emptyMessage="No posts yet. Write something or follow someone!"
      onreload={refreshFeed}
      ondelete={del.confirm}
      onlightbox={lightbox.open}
    />

    {#if scroll.hasMore && posts.length > 0}
      <div bind:this={sentinel} class="sentinel">
        {#if scroll.loadingMore}
          <span class="btn-spinner"></span> Loading...
        {/if}
      </div>
    {/if}

    <button class="refresh" onclick={requestSync} disabled={syncing}>
      {#if syncing}
        <span class="btn-spinner"></span> Syncing...
      {:else}
        Refresh
      {/if}
    </button>
  </div>
{/if}

{#if del.pendingId}
  <DeleteConfirmModal onconfirm={del.execute} oncancel={del.cancel} />
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

{#if showScrollTop}
  <button class="scroll-top" onclick={scrollToTop} aria-label="Scroll to top">
    &#8593;
  </button>
{/if}

<style>
  .divider {
    border: none;
    border-top: 1px solid var(--border);
    margin: 0.25rem 0 0;
  }

  .live-stages {
    margin-bottom: 0.75rem;
  }

  /* Right sidebar handles live stages on wide screens. */
  @media (min-width: 1150px) {
    .live-stages {
      display: none;
    }
  }

  .identity-card {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.6rem 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 1rem;
  }

  .key-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .key-label {
    color: var(--text-secondary);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
    min-width: 5.5rem;
  }

  .key-value {
    color: var(--color-link);
    font-size: var(--text-sm);
    flex: 1;
    font-family: var(--font-mono);
  }

  .refresh {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.4rem;
    margin: 1rem auto;
    background: var(--bg-elevated);
    color: var(--accent-light);
    border: none;
    border-radius: var(--radius-lg);
    padding: 0.5rem 1.5rem;
    font-size: var(--text-base);
    font-weight: 500;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      color var(--transition-fast);
  }

  .refresh:hover:not(:disabled) {
    background: var(--bg-elevated-hover);
    color: var(--accent-light-hover);
  }

  .scroll-top {
    position: fixed;
    bottom: calc(var(--bottom-nav-height) + env(safe-area-inset-bottom) + 1rem);
    right: 1.5rem;
    width: 44px;
    height: 44px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    font-size: var(--text-icon-lg);
    cursor: pointer;
    z-index: var(--z-fab);
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: var(--shadow-sm);
    transition:
      background var(--transition-normal),
      transform var(--transition-normal);
    animation: fadeIn var(--transition-normal) ease-out;
  }

  .scroll-top:hover {
    background: var(--accent-hover);
    transform: scale(1.1);
  }

  @media (min-width: 768px) {
    .scroll-top {
      bottom: 1.5rem;
    }
  }

  .pull-indicator {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: flex-end;
    padding-bottom: 0.5rem;
    overflow: hidden;
    color: var(--text-secondary);
    font-size: var(--text-base);
  }

  .pull-arrow {
    font-size: var(--text-icon-xl);
    color: var(--text-muted);
    transition: color var(--transition-fast);
  }

  .pull-arrow.ready {
    color: var(--accent-medium);
  }

  .pull-text {
    margin-top: 0.25rem;
  }
</style>
