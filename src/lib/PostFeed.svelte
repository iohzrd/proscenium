<script lang="ts">
  import PostCard from "#lib/PostCard.svelte";
  import ReplyComposer from "#lib/ReplyComposer.svelte";
  import QuoteComposer from "#lib/QuoteComposer.svelte";
  import type { MediaAttachment, Post } from "#lib/types.ts";

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
    onlightbox: (src: string, alt: string, att: MediaAttachment) => void;
  } = $props();

  // The open inline composer: `cardId` is the feed entry it is shown under,
  // `target` the post it replies to or quotes (the quoted post for a
  // repost-only entry).
  type Composing = { kind: "reply" | "quote"; cardId: string; target: Post };
  let composing = $state.raw<Composing | null>(null);

  function toggleComposer(
    kind: Composing["kind"],
    cardId: string,
    target: Post,
  ) {
    composing =
      composing?.kind === kind && composing.cardId === cardId
        ? null
        : { kind, cardId, target };
  }

  // Rendering a list with a repeated key throws; keep the first of each id.
  let uniquePosts = $derived.by(() => {
    const seen = new Set<string>();
    return posts.filter((p) => {
      if (seen.has(p.id)) return false;
      seen.add(p.id);
      return true;
    });
  });
</script>

<div class="feed">
  {#each uniquePosts as post (post.id)}
    <PostCard
      {post}
      {pubkey}
      {showAuthor}
      {showDelete}
      {showReplyContext}
      onreply={(target) => toggleComposer("reply", post.id, target)}
      {ondelete}
      onquote={(target) => toggleComposer("quote", post.id, target)}
      {onlightbox}
    />
    {#if composing?.cardId === post.id}
      {#if composing.kind === "reply"}
        <ReplyComposer
          replyToId={composing.target.id}
          replyToAuthor={composing.target.author}
          {pubkey}
          onsubmitted={() => {
            composing = null;
            onreload?.();
          }}
          oncancel={() => (composing = null)}
        />
      {:else}
        <QuoteComposer
          quotedPost={composing.target}
          {pubkey}
          onsubmitted={() => {
            composing = null;
            onreload?.();
          }}
          oncancel={() => (composing = null)}
        />
      {/if}
    {/if}
  {:else}
    <p class="empty">{emptyMessage}</p>
  {/each}
</div>
