<script lang="ts" module>
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import type { Interaction } from "$lib/types";

  /** Delay between an event that names no post and the counts reload. */
  const RELOAD_DEBOUNCE_MS = 500;

  interface Card {
    postId: () => string;
    reload: () => Promise<void>;
  }

  // One set of event listeners serves every mounted instance.
  const cards = new Set<Card>();
  let unlisteners: Promise<UnlistenFn>[] = [];
  let reloadAllTimer: ReturnType<typeof setTimeout> | undefined;

  /** Reloads the counts of every mounted card that shows `postId`. */
  async function reloadPost(postId: string): Promise<void> {
    await Promise.all(
      [...cards]
        .filter((card) => card.postId() === postId)
        .map((card) => card.reload()),
    );
  }

  function reloadAllDebounced() {
    clearTimeout(reloadAllTimer);
    reloadAllTimer = setTimeout(() => {
      for (const card of cards) card.reload();
    }, RELOAD_DEBOUNCE_MS);
  }

  function register(card: Card): () => void {
    cards.add(card);
    if (cards.size === 1) {
      unlisteners = [
        listen<Interaction>("interaction-received", (event) => {
          reloadPost(event.payload.target_post_id);
        }),
        listen<{ id: string; author: string; target_post_id?: string }>(
          "interaction-deleted",
          (event) => {
            const target = event.payload.target_post_id;
            if (target) reloadPost(target);
            else reloadAllDebounced();
          },
        ),
        listen("feed-updated", reloadAllDebounced),
      ];
    }
    return () => {
      cards.delete(card);
      if (cards.size === 0) {
        clearTimeout(reloadAllTimer);
        for (const unlisten of unlisteners) unlisten.then((fn) => fn());
        unlisteners = [];
      }
    };
  }
</script>

<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import type { PostCounts } from "$lib/types";

  let {
    postId,
    postAuthor,
    onreply,
    onquote,
  }: {
    postId: string;
    postAuthor: string;
    onreply?: () => void;
    onquote?: () => void;
  } = $props();

  const NO_COUNTS: PostCounts = {
    likes: 0,
    replies: 0,
    reposts: 0,
    liked_by_me: false,
    reposted_by_me: false,
  };

  let counts = $state.raw<PostCounts>(NO_COUNTS);
  let liking = $state(false);
  let reposting = $state(false);
  let bookmarked = $state(false);
  let bookmarking = $state(false);
  let errorMessage = $state("");
  let errorTimer: ReturnType<typeof setTimeout> | undefined;

  // Derived: the effect below reruns when the id string changes, not when the
  // parent passes an equal post object.
  const currentId = $derived(postId);

  function showError(action: string, e: unknown) {
    errorMessage = `${action} failed: ${e}`;
    clearTimeout(errorTimer);
    errorTimer = setTimeout(() => (errorMessage = ""), 4000);
  }

  async function loadCounts(id: string) {
    try {
      const loaded = await invoke<PostCounts>("get_post_counts", {
        targetPostId: id,
      });
      if (id === currentId) counts = loaded;
    } catch (e) {
      console.error("Failed to load post counts:", e);
    }
  }

  async function loadBookmark(id: string) {
    try {
      const loaded = await invoke<boolean>("is_bookmarked", { postId: id });
      if (id === currentId) bookmarked = loaded;
    } catch (e) {
      console.error("Failed to load bookmark state:", e);
    }
  }

  $effect(() => {
    const id = currentId;
    counts = NO_COUNTS;
    bookmarked = false;
    loadCounts(id);
    loadBookmark(id);
  });

  onMount(() => {
    const unregister = register({
      postId: () => currentId,
      reload: () => loadCounts(currentId),
    });
    return () => {
      unregister();
      clearTimeout(errorTimer);
    };
  });

  async function toggleLike() {
    if (liking) return;
    liking = true;
    const id = currentId;
    try {
      if (counts.liked_by_me) {
        await invoke("unlike_post", { targetPostId: id });
      } else {
        await invoke("like_post", {
          targetPostId: id,
          targetAuthor: postAuthor,
        });
      }
    } catch (e) {
      showError(counts.liked_by_me ? "Unlike" : "Like", e);
    }
    // The counts come from storage again; no local arithmetic.
    await reloadPost(id);
    liking = false;
  }

  async function toggleRepost() {
    if (reposting) return;
    reposting = true;
    const id = currentId;
    try {
      if (counts.reposted_by_me) {
        await invoke("unrepost", { targetPostId: id });
      } else {
        await invoke("repost", {
          targetPostId: id,
          targetAuthor: postAuthor,
        });
      }
    } catch (e) {
      showError(counts.reposted_by_me ? "Undo repost" : "Repost", e);
    }
    await reloadPost(id);
    reposting = false;
  }

  async function toggleBookmark() {
    if (bookmarking) return;
    bookmarking = true;
    const id = currentId;
    try {
      const next = await invoke<boolean>("toggle_bookmark", { postId: id });
      if (id === currentId) bookmarked = next;
    } catch (e) {
      showError("Bookmark", e);
    }
    bookmarking = false;
  }
</script>

<div class="post-actions">
  <button
    class="action-btn"
    class:active={counts.liked_by_me}
    onclick={toggleLike}
    disabled={liking}
    aria-label={counts.liked_by_me ? "Unlike" : "Like"}
  >
    <span class="icon">{counts.liked_by_me ? "\u2665" : "\u2661"}</span>
    {#if counts.likes > 0}<span class="count">{counts.likes}</span>{/if}
  </button>

  {#if onreply}
    <button class="action-btn" onclick={onreply} aria-label="Reply">
      <span class="icon">{"\u21A9"}</span>
      {#if counts.replies > 0}<span class="count">{counts.replies}</span>{/if}
    </button>
  {:else if counts.replies > 0}
    <span class="action-btn static" title="Replies">
      <span class="icon">{"\u21A9"}</span>
      <span class="count">{counts.replies}</span>
    </span>
  {/if}

  <button
    class="action-btn"
    class:active={counts.reposted_by_me}
    onclick={toggleRepost}
    disabled={reposting}
    title="Repost"
    aria-label={counts.reposted_by_me ? "Undo repost" : "Repost"}
  >
    <span class="icon">{"\u21BB"}</span>
    {#if counts.reposts > 0}<span class="count">{counts.reposts}</span>{/if}
  </button>

  {#if onquote}
    <button
      class="action-btn"
      onclick={onquote}
      title="Quote"
      aria-label="Quote"
    >
      <span class="icon">{"\u275D"}</span>
    </button>
  {/if}

  <button
    class="action-btn bookmark"
    class:active={bookmarked}
    onclick={toggleBookmark}
    disabled={bookmarking}
    title={bookmarked ? "Remove bookmark" : "Bookmark"}
    aria-label={bookmarked ? "Remove bookmark" : "Bookmark"}
  >
    <span class="icon">{bookmarked ? "\u2605" : "\u2606"}</span>
  </button>
</div>
{#if errorMessage}
  <p class="action-error" role="alert">{errorMessage}</p>
{/if}

<style>
  .post-actions {
    display: flex;
    gap: 0.25rem;
    margin-top: 0.6rem;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border-faintest);
  }

  .action-btn {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: var(--text-base);
    padding: 0.4rem 0.6rem;
    min-height: 36px;
    border-radius: var(--radius-md);
    transition:
      color var(--transition-fast),
      background var(--transition-fast);
  }

  .action-btn:hover:not(:disabled) {
    color: var(--accent-light);
    background: var(--accent-light-hover-bg);
  }

  .action-btn.static,
  .action-btn.static:hover {
    cursor: default;
    color: var(--text-muted);
    background: none;
  }

  .action-btn.active {
    color: var(--color-error-light);
  }

  .action-btn.active:hover:not(:disabled) {
    color: var(--color-error-light);
    background: var(--color-error-light-hover-bg);
  }

  .action-btn.bookmark {
    margin-left: auto;
  }

  .action-btn.bookmark.active {
    color: var(--color-bookmark);
  }

  .action-btn.bookmark.active:hover:not(:disabled) {
    color: var(--color-bookmark);
    background: var(--color-bookmark-hover-bg);
  }

  .icon {
    font-size: var(--text-icon);
    line-height: 1;
  }

  .count {
    font-size: var(--text-sm);
  }

  .action-error {
    color: var(--color-error-light);
    font-size: var(--text-sm);
    margin: 0.25rem 0 0;
  }
</style>
