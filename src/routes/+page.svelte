<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import PostFeed from "$lib/PostFeed.svelte";
  import StageCard from "$lib/StageCard.svelte";
  import DeleteConfirmModal from "$lib/DeleteConfirmModal.svelte";
  import PostComposer from "$lib/PostComposer.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import { hapticImpact } from "$lib/haptics";
  import type { Post, StageAnnouncement } from "$lib/types";
  import { shortId, seedOwnProfile, evictDisplayName } from "$lib/utils";
  import {
    useToast,
    useCopyFeedback,
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useDeleteConfirm,
    useLightbox,
    usePullToRefresh,
  } from "$lib/composables.svelte";

  let syncing = $state(false);
  let posts = $state<Post[]>([]);
  let liveStages = $state<Map<string, StageAnnouncement>>(new Map());
  let showScrollTop = $state(false);
  let sentinel = $state<HTMLDivElement>(null!);
  let syncFailures = $state<string[]>([]);
  let showSyncDetails = $state(false);

  const blobs = createBlobCache();
  setBlobContext(blobs);

  const toast = useToast();
  const copyFb = useCopyFeedback();
  const lightbox = useLightbox();
  const del = useDeleteConfirm(async (id) => {
    try {
      await invoke("delete_post", { id });
      await loadFeed();
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
    await seedOwnProfile(node.pubkey);
    await loadFeed();
  });

  async function loadFeed() {
    try {
      const newPosts: Post[] = await invoke("get_feed", { limit: 20 });
      posts = newPosts;
      scroll.setHasMore(newPosts.length);
    } catch (e) {
      toast.show("Failed to load feed");
      console.error("Failed to load feed:", e);
    }
  }

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      try {
        const oldest = posts[posts.length - 1];
        const olderPosts: Post[] = await invoke("get_feed", {
          limit: 20,
          before: oldest.timestamp,
        });
        if (olderPosts.length === 0) {
          scroll.setNoMore();
        } else {
          posts = [...posts, ...olderPosts];
          scroll.setHasMore(olderPosts.length);
        }
      } catch (e) {
        toast.show("Failed to load more posts");
        console.error("Failed to load more:", e);
      }
    },
    20,
  );

  async function syncAll() {
    syncing = true;
    syncFailures = [];
    try {
      const follows: { pubkey: string }[] = await invoke("get_follows");
      const results = await Promise.allSettled(
        follows.map(async (f) => {
          await invoke("sync_posts", { pubkey: f.pubkey });
          return f.pubkey;
        }),
      );
      const failed: string[] = [];
      for (let i = 0; i < results.length; i++) {
        if (results[i].status === "rejected") {
          failed.push(follows[i].pubkey);
        }
      }
      syncFailures = failed;
      if (failed.length > 0 && failed.length < follows.length) {
        toast.show(`Synced, but ${failed.length} peer(s) unreachable`);
      } else if (failed.length > 0 && failed.length === follows.length) {
        toast.show("Could not reach any peers");
      }
      await loadFeed();
    } catch (e) {
      toast.show("Sync failed");
      console.error("Failed to sync:", e);
    }
    syncing = false;
  }

  const pull = usePullToRefresh(async () => {
    hapticImpact("medium");
    await syncAll();
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
    syncInterval = setInterval(() => syncAll(), 600000);
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
    } else {
      syncAll();
      startAutoSync();
    }
  }

  $effect(() => {
    return scroll.setupEffect();
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "feed-updated": () => {
        loadFeed();
      },
      "profile-updated": (payload) => {
        const pubkey = payload as string;
        evictDisplayName(pubkey);
        loadFeed();
      },
      "stage-announced": (payload) => {
        const ann = payload as StageAnnouncement;
        liveStages = new Map(liveStages).set(ann.stage_id, ann);
      },
      "stage-ended-remote": (payload) => {
        const stageId = payload as string;
        const next = new Map(liveStages);
        next.delete(stageId);
        liveStages = next;
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
  <Lightbox src={lightbox.src} alt={lightbox.alt} onclose={lightbox.close} />
{/if}

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Starting node...</p>
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

    <PostComposer pubkey={node.pubkey} onsubmitted={loadFeed} />

    {#if del.pendingId}
      <DeleteConfirmModal onconfirm={del.execute} oncancel={del.cancel} />
    {/if}

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
      onreload={loadFeed}
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

    <button class="refresh" onclick={syncAll} disabled={syncing}>
      {#if syncing}
        <span class="btn-spinner"></span> Syncing...
      {:else}
        Refresh
      {/if}
    </button>

    {#if syncFailures.length > 0}
      <div class="sync-failures">
        <button
          class="sync-failures-toggle"
          onclick={() => (showSyncDetails = !showSyncDetails)}
        >
          {syncFailures.length} peer(s) unreachable
          <span class="toggle-arrow">{showSyncDetails ? "v" : ">"}</span>
        </button>
        {#if showSyncDetails}
          <ul class="sync-failures-list">
            {#each syncFailures as peer}
              <li><code>{shortId(peer)}</code></li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}
  </div>
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
    margin: 0.25rem 0 1rem;
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
    padding: 0.6rem 0.85rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
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

  .sync-failures {
    margin: 0.5rem 0;
    border: 1px solid var(--danger-bg);
    border-radius: var(--radius-lg);
    background: var(--danger-bg-subtle);
    overflow: hidden;
  }

  .sync-failures-toggle {
    width: 100%;
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: none;
    border: none;
    color: var(--danger-text);
    padding: 0.5rem 0.75rem;
    font-size: var(--text-base);
    cursor: pointer;
  }

  .sync-failures-toggle:hover {
    background: var(--danger-bg-subtle-hover);
  }

  .toggle-arrow {
    font-size: var(--text-sm);
    color: var(--text-secondary);
  }

  .sync-failures-list {
    list-style: none;
    padding: 0 0.75rem 0.5rem;
    margin: 0;
  }

  .sync-failures-list li {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    padding: 0.15rem 0;
  }

  .sync-failures-list code {
    color: var(--color-error-light);
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
