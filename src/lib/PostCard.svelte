<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "$lib/Avatar.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import PostActions from "$lib/PostActions.svelte";
  import MediaGrid from "$lib/MediaGrid.svelte";
  import ReplyContextBlock from "$lib/ReplyContextBlock.svelte";
  import QuotedPostEmbed from "$lib/QuotedPostEmbed.svelte";
  import { useDisplayName } from "$lib/name.svelte";
  import type { MediaAttachment, Post } from "$lib/types";
  import { getCachedAvatarTicket, renderContent } from "$lib/utils";

  let {
    post,
    pubkey,
    showAuthor = true,
    showDelete = false,
    showReplyContext = true,
    onreply,
    ondelete,
    onquote,
    onlightbox,
  }: {
    post: Post;
    pubkey: string;
    showAuthor?: boolean;
    showDelete?: boolean;
    showReplyContext?: boolean;
    onreply?: (post: Post) => void;
    ondelete?: (id: string) => void;
    onquote?: (post: Post) => void;
    onlightbox?: (src: string, alt: string, att: MediaAttachment) => void;
  } = $props();

  // Repost-only: a quote with no original content
  let quotedPost = $state<Post | null>(null);
  let isRepostOnly = $derived(
    post.quote_of && !post.content && post.media.length === 0,
  );
  let displayPost = $derived(isRepostOnly && quotedPost ? quotedPost : post);

  // Only fetch quoted post when repost-only (QuotedPostEmbed handles the normal case)
  $effect(() => {
    if (isRepostOnly && post.quote_of) {
      invoke("get_post", { id: post.quote_of })
        .then((qp) => {
          quotedPost = qp as Post | null;
        })
        .catch(() => {});
    }
  });

  // Reactive name resolution (replaces 4 separate $effect/$state blocks)
  const author = useDisplayName(
    () => displayPost.author,
    () => pubkey,
  );
  const repostAuthor = useDisplayName(
    () => post.author,
    () => pubkey,
  );
</script>

<article class="post">
  {#if isRepostOnly && showAuthor}
    <div class="repost-label">
      <a href="/profile/{post.author}" class="repost-author"
        >{repostAuthor.name}</a
      >
      <span>reposted</span>
    </div>
  {/if}

  <div class="post-header">
    {#if showAuthor}
      <a href="/profile/{displayPost.author}" class="author-link">
        <Avatar
          pubkey={displayPost.author}
          name={author.name}
          isSelf={displayPost.author === pubkey}
          ticket={getCachedAvatarTicket(displayPost.author)}
        />
        <span class="author" class:self={displayPost.author === pubkey}>
          {author.name}
        </span>
      </a>
    {/if}
    <div class="post-header-right">
      <a href="/post/{displayPost.id}" class="time-link">
        <Timeago timestamp={displayPost.timestamp} />
      </a>
      {#if showDelete && post.author === pubkey && ondelete}
        <button
          class="delete-btn"
          onclick={() => ondelete(post.id)}
          aria-label="Delete post"
        >
          &times;
        </button>
      {/if}
    </div>
  </div>

  {#if showReplyContext && post.reply_to}
    <ReplyContextBlock replyToId={post.reply_to} {pubkey} />
  {/if}

  {#if isRepostOnly && quotedPost}
    {#if quotedPost.content}
      <p class="post-content">
        {@html renderContent(quotedPost.content, pubkey)}
      </p>
    {/if}
    <MediaGrid media={quotedPost.media} {onlightbox} />
  {:else}
    {#if post.content}
      <p class="post-content">{@html renderContent(post.content, pubkey)}</p>
    {/if}
    <MediaGrid media={post.media} {onlightbox} />
    {#if post.quote_of}
      <QuotedPostEmbed quoteOfId={post.quote_of} {pubkey} />
    {/if}
  {/if}

  <PostActions
    postId={post.id}
    postAuthor={post.author}
    onreply={() => onreply?.(post)}
    onquote={() => onquote?.(post)}
  />
</article>

<style>
  .post {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-2xl);
    padding: 0.875rem 1rem;
    margin-bottom: 0.4rem;
    transition: border-color var(--transition-normal);
    animation: fadeIn var(--transition-slow) ease-out;
  }

  .post:hover {
    border-color: var(--border-hover);
  }

  .repost-label {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    margin-bottom: 0.4rem;
    font-size: var(--text-sm);
    color: var(--text-tertiary);
  }

  .repost-author {
    color: var(--accent-light);
    text-decoration: none;
    font-weight: 600;
  }

  .repost-author:hover {
    text-decoration: underline;
  }

  .post-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.4rem;
  }

  .post-header-right {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-left: auto;
    flex-shrink: 0;
  }

  .author-link {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    text-decoration: none;
    color: inherit;
    min-width: 0;
  }

  .author-link:hover .author {
    text-decoration: underline;
  }

  .author {
    font-weight: 600;
    font-size: var(--text-base);
    color: var(--accent-light);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .author.self {
    color: var(--accent-medium);
  }

  .delete-btn {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: var(--text-xl);
    cursor: pointer;
    padding: 0.25rem;
    min-width: 44px;
    min-height: 44px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
    border-radius: var(--radius-sm);
    transition:
      color var(--transition-fast),
      background var(--transition-fast);
  }

  .delete-btn:hover {
    color: var(--color-error);
    background: var(--color-error-bg-hover);
  }

  .time-link {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    white-space: nowrap;
    text-decoration: none;
  }

  .time-link:hover {
    color: var(--text-secondary);
    text-decoration: underline;
  }

  .post-content {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    font-size: var(--text-lg);
    line-height: 1.55;
    color: var(--text-post);
  }

  .post-content :global(a) {
    color: var(--color-link);
    text-decoration: none;
  }

  .post-content :global(a:hover) {
    text-decoration: underline;
  }

  .post-content :global(a.mention) {
    color: var(--accent-light);
    font-weight: 600;
  }
</style>
