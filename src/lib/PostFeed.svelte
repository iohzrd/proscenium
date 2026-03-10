<script lang="ts">
  import PostCard from "$lib/PostCard.svelte";
  import ReplyComposer from "$lib/ReplyComposer.svelte";
  import QuoteComposer from "$lib/QuoteComposer.svelte";
  import type { Post } from "$lib/types";

  let {
    posts,
    pubkey,
    emptyMessage = "No posts yet.",
    showAuthor = true,
    showDelete = false,
    showReplyContext = true,
    onreload,
    ondelete,
    onlightbox,
  }: {
    posts: Post[];
    pubkey: string;
    emptyMessage?: string;
    showAuthor?: boolean;
    showDelete?: boolean;
    showReplyContext?: boolean;
    onreload?: () => void;
    ondelete?: (id: string) => void;
    onlightbox: (src: string, alt: string) => void;
  } = $props();

  let replyingTo = $state<Post | null>(null);
  let quotingPost = $state<Post | null>(null);
</script>

<div class="feed">
  {#each posts as post (post.id)}
    <PostCard
      {post}
      {pubkey}
      {showAuthor}
      {showDelete}
      {showReplyContext}
      onreply={(p) => {
        replyingTo = replyingTo?.id === p.id ? null : p;
        quotingPost = null;
      }}
      {ondelete}
      onquote={(p) => {
        quotingPost = quotingPost?.id === p.id ? null : p;
        replyingTo = null;
      }}
      {onlightbox}
    />
    {#if replyingTo?.id === post.id}
      <ReplyComposer
        replyToId={post.id}
        replyToAuthor={post.author}
        {pubkey}
        onsubmitted={() => {
          replyingTo = null;
          onreload?.();
        }}
        oncancel={() => (replyingTo = null)}
      />
    {/if}
    {#if quotingPost?.id === post.id}
      <QuoteComposer
        quotedPost={post}
        {pubkey}
        onsubmitted={() => {
          quotingPost = null;
          onreload?.();
        }}
        oncancel={() => (quotingPost = null)}
      />
    {/if}
  {:else}
    <p class="empty">{emptyMessage}</p>
  {/each}
</div>
