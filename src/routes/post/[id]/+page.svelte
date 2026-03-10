<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import PostCard from "$lib/PostCard.svelte";
  import ReplyComposer from "$lib/ReplyComposer.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import type { Post } from "$lib/types";
  import {
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useLightbox,
  } from "$lib/composables.svelte";

  let postId: string = $derived(page.params.id ?? "");
  let post = $state<Post | null>(null);
  let replies = $state<Post[]>([]);
  let sentinel = $state<HTMLDivElement>(null!);
  let replySection = $state<HTMLDivElement>(null!);

  const blobs = createBlobCache();
  setBlobContext(blobs);

  const lightbox = useLightbox();

  const node = useNodeInit(async () => {
    await loadPost();
    await loadReplies();
  });

  async function loadPost() {
    post = await invoke("get_post", { id: postId });
  }

  async function loadReplies() {
    try {
      const result: Post[] = await invoke("get_replies", {
        targetPostId: postId,
        limit: 50,
        before: null,
      });
      replies = result;
      scroll.setHasMore(result.length);
    } catch (e) {
      console.error("Failed to load replies:", e);
    }
  }

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      try {
        const oldest = replies[replies.length - 1];
        const more: Post[] = await invoke("get_replies", {
          targetPostId: postId,
          limit: 50,
          before: oldest.timestamp,
        });
        if (more.length === 0) {
          scroll.setNoMore();
        } else {
          replies = [...replies, ...more];
          scroll.setHasMore(more.length);
        }
      } catch (e) {
        console.error("Failed to load more replies:", e);
      }
    },
    50,
  );

  $effect(() => {
    return scroll.setupEffect();
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "feed-updated": () => {
        loadReplies();
      },
    });
    return () => {
      blobs.revokeAll();
      cleanupListeners();
    };
  });
</script>

{#if lightbox.src}
  <Lightbox src={lightbox.src} alt={lightbox.alt} onclose={lightbox.close} />
{/if}

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Loading thread...</p>
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
        Replies{replies.length > 0 ? ` (${replies.length})` : ""}
      </h3>

      <ReplyComposer
        replyToId={post.id}
        replyToAuthor={post.author}
        pubkey={node.pubkey}
        onsubmitted={loadReplies}
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
