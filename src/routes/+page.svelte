<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import PostCard from "$lib/PostCard.svelte";
  import ReplyComposer from "$lib/ReplyComposer.svelte";
  import QuoteComposer from "$lib/QuoteComposer.svelte";
  import DeleteConfirmModal from "$lib/DeleteConfirmModal.svelte";
  import PostComposer from "$lib/PostComposer.svelte";
  import { platform } from "@tauri-apps/plugin-os";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import { hapticImpact } from "$lib/haptics";
  import type { Post } from "$lib/types";
  import {
    shortId,
    seedOwnProfile,
    evictDisplayName,
    copyToClipboard,
    setupInfiniteScroll,
  } from "$lib/utils";

  const isMobile = platform() === "android" || platform() === "ios";

  let nodeId = $state("");
  let loading = $state(true);
  let syncing = $state(false);
  let posts = $state<Post[]>([]);
  let copyFeedback = $state("");
  let pendingDeleteId = $state<string | null>(null);
  let showScrollTop = $state(false);
  let toastMessage = $state("");
  let toastType = $state<"error" | "success">("error");
  let loadingMore = $state(false);
  let hasMore = $state(true);
  let replyingTo = $state<Post | null>(null);
  let quotingPost = $state<Post | null>(null);
  let lightboxSrc = $state("");
  let lightboxAlt = $state("");
  let sentinel = $state<HTMLDivElement>(null!);
  let syncFailures = $state<string[]>([]);
  let showSyncDetails = $state(false);
  // Pull-to-refresh
  let pullStartY = 0;
  let pullDistance = $state(0);
  let isPulling = $state(false);
  let pullTriggered = $state(false);
  const PULL_THRESHOLD = 80;

  const blobs = createBlobCache();
  setBlobContext(blobs);

  function showToast(message: string, type: "error" | "success" = "error") {
    toastMessage = message;
    toastType = type;
    setTimeout(() => (toastMessage = ""), 4000);
  }

  async function copyWithFeedback(text: string, label: string) {
    await copyToClipboard(text);
    copyFeedback = label;
    setTimeout(() => (copyFeedback = ""), 1500);
  }

  async function init() {
    try {
      nodeId = await invoke("get_node_id");
      const profile = await invoke("get_my_profile");
      if (!profile) {
        goto("/welcome");
        return;
      }
      await seedOwnProfile(nodeId);
      await loadFeed();
      loading = false;
    } catch {
      setTimeout(init, 500);
    }
  }

  async function loadFeed() {
    try {
      const newPosts: Post[] = await invoke("get_feed", { limit: 20 });
      posts = newPosts;
      hasMore = newPosts.length >= 20;
    } catch (e) {
      showToast("Failed to load feed");
      console.error("Failed to load feed:", e);
    }
  }

  async function loadMore() {
    if (loadingMore || !hasMore || posts.length === 0) return;
    loadingMore = true;
    try {
      const oldest = posts[posts.length - 1];
      const olderPosts: Post[] = await invoke("get_feed", {
        limit: 20,
        before: oldest.timestamp,
      });
      if (olderPosts.length === 0) {
        hasMore = false;
      } else {
        posts = [...posts, ...olderPosts];
        hasMore = olderPosts.length >= 20;
      }
    } catch (e) {
      showToast("Failed to load more posts");
      console.error("Failed to load more:", e);
    }
    loadingMore = false;
  }

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
        showToast(`Synced, but ${failed.length} peer(s) unreachable`);
      } else if (failed.length > 0 && failed.length === follows.length) {
        showToast("Could not reach any peers");
      }
      await loadFeed();
    } catch (e) {
      showToast("Sync failed");
      console.error("Failed to sync:", e);
    }
    syncing = false;
  }

  function confirmDelete(id: string) {
    pendingDeleteId = id;
  }

  async function executeDelete() {
    if (!pendingDeleteId) return;
    try {
      await invoke("delete_post", { id: pendingDeleteId });
      await loadFeed();
    } catch (e) {
      showToast("Failed to delete post");
      console.error("Failed to delete post:", e);
    }
    pendingDeleteId = null;
  }

  function cancelDelete() {
    pendingDeleteId = null;
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape" && pendingDeleteId) {
      cancelDelete();
    }
  }

  function scrollToTop() {
    window.scrollTo({ top: 0, behavior: "smooth" });
  }

  function handleScroll() {
    showScrollTop = window.scrollY > 400;
  }

  function handleTouchStart(e: TouchEvent) {
    if (window.scrollY === 0 && !syncing) {
      pullStartY = e.touches[0].clientY;
      isPulling = true;
    }
  }

  function handleTouchMove(e: TouchEvent) {
    if (!isPulling) return;
    const delta = e.touches[0].clientY - pullStartY;
    if (delta > 0) {
      pullDistance = Math.min(delta * 0.5, 120);
      pullTriggered = pullDistance >= PULL_THRESHOLD;
    } else {
      pullDistance = 0;
      isPulling = false;
    }
  }

  async function handleTouchEnd() {
    if (isPulling && pullTriggered && !syncing) {
      hapticImpact("medium");
      await syncAll();
    }
    pullDistance = 0;
    isPulling = false;
    pullTriggered = false;
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
    return setupInfiniteScroll(
      sentinel,
      () => hasMore,
      () => loadingMore,
      loadMore,
    );
  });

  onMount(() => {
    init();
    const unlisteners: Promise<UnlistenFn>[] = [];
    unlisteners.push(
      listen("feed-updated", () => {
        loadFeed();
      }),
    );
    unlisteners.push(
      listen("profile-updated", (event) => {
        const pubkey = event.payload as string;
        evictDisplayName(pubkey);
        loadFeed();
      }),
    );
    window.addEventListener("scroll", handleScroll);
    window.addEventListener("keydown", handleGlobalKey);
    document.addEventListener("visibilitychange", handleVisibility);
    startAutoSync();
    return () => {
      stopAutoSync();
      document.removeEventListener("visibilitychange", handleVisibility);
      window.removeEventListener("keydown", handleGlobalKey);
      window.removeEventListener("scroll", handleScroll);
      unlisteners.forEach((p) => p.then((fn) => fn()));
      blobs.revokeAll();
    };
  });
</script>

{#if lightboxSrc}
  <Lightbox
    src={lightboxSrc}
    alt={lightboxAlt}
    onclose={() => {
      lightboxSrc = "";
    }}
  />
{/if}

{#if loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Starting node...</p>
  </div>
{:else}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    ontouchstart={handleTouchStart}
    ontouchmove={handleTouchMove}
    ontouchend={handleTouchEnd}
    style="transform: translateY({pullDistance}px); transition: {isPulling
      ? 'none'
      : 'transform 0.3s ease-out'};"
  >
    {#if pullDistance > 0}
      <div class="pull-indicator" style="height: {pullDistance}px;">
        <div
          class="pull-arrow"
          class:ready={pullTriggered}
          style="transform: rotate({pullDistance * 3}deg);"
        >
          &#8635;
        </div>
        <span class="pull-text">
          {pullTriggered ? "Release to refresh" : "Pull to refresh"}
        </span>
      </div>
    {/if}
    <div class="node-id">
      <span class="label">You</span>
      <code>{shortId(nodeId)}</code>
      <button
        class="btn-elevated copy-btn"
        onclick={() => copyWithFeedback(nodeId, "node-id")}
      >
        {copyFeedback === "node-id" ? "Copied!" : "Copy ID"}
      </button>
    </div>

    <PostComposer {nodeId} onsubmitted={loadFeed} />

    {#if pendingDeleteId}
      <DeleteConfirmModal onconfirm={executeDelete} oncancel={cancelDelete} />
    {/if}

    <hr class="divider" />

    <div class="feed">
      {#each posts as post (post.id)}
        <PostCard
          {post}
          {nodeId}
          showDelete={true}
          onreply={(p) => {
            replyingTo = replyingTo?.id === p.id ? null : p;
            quotingPost = null;
          }}
          ondelete={confirmDelete}
          onquote={(p) => {
            quotingPost = quotingPost?.id === p.id ? null : p;
            replyingTo = null;
          }}
          onlightbox={(src, alt) => {
            lightboxSrc = src;
            lightboxAlt = alt;
          }}
        />
        {#if replyingTo?.id === post.id}
          <ReplyComposer
            replyToId={post.id}
            replyToAuthor={post.author}
            {nodeId}
            onsubmitted={() => {
              replyingTo = null;
              loadFeed();
            }}
            oncancel={() => (replyingTo = null)}
          />
        {/if}
        {#if quotingPost?.id === post.id}
          <QuoteComposer
            quotedPost={post}
            {nodeId}
            onsubmitted={() => {
              quotingPost = null;
              loadFeed();
            }}
            oncancel={() => (quotingPost = null)}
          />
        {/if}
      {:else}
        <p class="empty">No posts yet. Write something or follow someone!</p>
      {/each}
    </div>

    {#if hasMore && posts.length > 0}
      <div bind:this={sentinel} class="sentinel">
        {#if loadingMore}
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

{#if toastMessage}
  <div class="toast" class:error={toastType === "error"}>
    {toastMessage}
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

  .node-id {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.6rem 0.85rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    margin-bottom: 1rem;
  }

  .node-id .label {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
  }

  .node-id code {
    color: var(--color-link);
    font-size: var(--text-base);
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
