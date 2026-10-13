<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import PostCard from "$lib/PostCard.svelte";
  import ReplyComposer from "$lib/ReplyComposer.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import type { Post } from "$lib/types";
  import { coalesce, dedupeById, mergeFirstPage } from "$lib/utils";
  import {
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useLightbox,
    useToast,
  } from "$lib/composables";

  // The parent creates one instance per post id, so `postId` does not change
  // during the lifetime of this component.
  let { postId }: { postId: string } = $props();

  const PAGE_SIZE = 50;
  // Largest number of rows one reply-list refresh reads again.
  const REFRESH_MAX = 200;

  let post = $state<Post | null>(null);
  let replies = $state<Post[]>([]);
  let sentinel = $state<HTMLDivElement>(null!);
  let replySection = $state<HTMLDivElement>(null!);

  const toast = useToast();
  const blobs = createBlobCache({ onError: toast.show });
  setBlobContext(blobs);

  const lightbox = useLightbox();

  const node = useNodeInit(async () => {
    await loadPost();
    await refreshReplies();
  });

  async function loadPost() {
    post = await invoke("get_post", { id: postId });
  }

  /**
   * Read the first rows of the reply list (ascending) again and merge them
   * into `replies` by id. Rows loaded by pagination beyond the refreshed
   * range are kept.
   */
  const refreshReplies = coalesce(async () => {
    const limit = Math.min(Math.max(PAGE_SIZE, replies.length), REFRESH_MAX);
    try {
      const fresh: Post[] = await invoke("get_replies", {
        targetPostId: postId,
        limit,
        after: null,
        afterId: null,
      });
      const merged = mergeFirstPage(replies, fresh, limit, "asc");
      replies = merged.items;
      if (merged.exhausted) scroll.setNoMore();
      else if (!merged.keptExisting) scroll.reset();
    } catch (e) {
      toast.show("Failed to load replies");
      console.error("Failed to load replies:", e);
    }
  });

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      const last = replies[replies.length - 1];
      if (!last) return false;
      try {
        const more: Post[] = await invoke("get_replies", {
          targetPostId: postId,
          limit: PAGE_SIZE,
          after: last.timestamp,
          afterId: last.id,
        });
        // The cursor row is no longer the last row: this page is outdated.
        if (replies[replies.length - 1]?.id !== last.id) return;
        const known = new Set(replies.map((r) => r.id));
        const added = dedupeById(more).filter((r) => !known.has(r.id));
        if (added.length === 0) {
          scroll.setNoMore();
          return;
        }
        replies = [...replies, ...added];
        scroll.setHasMore(more.length);
      } catch (e) {
        toast.show("Failed to load more replies");
        console.error("Failed to load more replies:", e);
        return false;
      }
    },
    PAGE_SIZE,
  );

  $effect(() => {
    return scroll.setupEffect();
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "feed-updated": () => {
        // The event has no payload: the post may have been deleted, or a reply added.
        loadPost().catch((e) => console.error("Failed to reload post:", e));
        refreshReplies();
      },
    });
    return () => {
      blobs.revokeAll();
      cleanupListeners();
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
    <p>Loading thread...</p>
  </div>
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
  </div>
{:else}
  <a href="/" class="back-link">&larr; Back to feed</a>

  {#if post}
    <div class="parent-post">
      <PostCard
        {post}
        pubkey={node.pubkey}
        showReplyContext={true}
        onreply={() => {
          replySection?.scrollIntoView({ behavior: "smooth" });
        }}
        onlightbox={lightbox.open}
      />
    </div>

    <div class="reply-section" bind:this={replySection}>
      <h3 class="section-title">
        Replies{replies.length > 0
          ? ` (${replies.length}${scroll.hasMore ? "+" : ""})`
          : ""}
      </h3>

      <ReplyComposer
        replyToId={post.id}
        replyToAuthor={post.author}
        pubkey={node.pubkey}
        onsubmitted={refreshReplies}
      />
    </div>
  {:else}
    <div class="not-found">
      <p>Post not found in local cache.</p>
      <p class="hint">
        The post may not have been synced yet. Try viewing it from the author's
        profile.
      </p>
    </div>
  {/if}

  <div class="replies">
    {#each replies as reply (reply.id)}
      <PostCard
        post={reply}
        pubkey={node.pubkey}
        showReplyContext={false}
        onlightbox={lightbox.open}
      />
    {:else}
      {#if post}
        <p class="empty">No replies yet.</p>
      {/if}
    {/each}
  </div>

  {#if scroll.hasMore && replies.length > 0}
    <div bind:this={sentinel} class="sentinel">
      {#if scroll.loadingMore}
        <span class="btn-spinner"></span> Loading...
      {/if}
    </div>
  {/if}
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

<style>
  .parent-post :global(.post) {
    border-color: var(--border-hover);
    margin-bottom: 1rem;
  }

  .reply-section {
    margin-bottom: 1rem;
  }

  .empty {
    padding: 1rem;
    font-size: var(--text-base);
  }

  .not-found {
    text-align: center;
    padding: 2rem;
    color: var(--text-secondary);
  }

  .not-found .hint {
    font-size: var(--text-base);
    color: var(--text-tertiary);
  }
</style>
