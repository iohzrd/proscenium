<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "$lib/Avatar.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import PostActions from "$lib/PostActions.svelte";
  import MediaGrid from "$lib/MediaGrid.svelte";
  import ReplyContextBlock from "$lib/ReplyContextBlock.svelte";
  import QuotedPostEmbed from "$lib/QuotedPostEmbed.svelte";
  import { useDisplayName } from "$lib/name.svelte";
  import { postHref, profileHref } from "$lib/profile/links";
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
    /** Called with the displayed post: the quoted post for a repost-only card. */
    onreply?: (post: Post) => void;
    ondelete?: (id: string) => void;
    /** Called with the displayed post: the quoted post for a repost-only card. */
    onquote?: (post: Post) => void;
    onlightbox?: (src: string, alt: string, att: MediaAttachment) => void;
  } = $props();

  // Repost-only: a quote with no content and no media of its own.
  let isRepostOnly = $derived(
    !!post.quote_of && !post.content && post.media.length === 0,
  );
  let repostTargetId = $derived(isRepostOnly ? post.quote_of : null);
  let quotedPost = $state<Post | null>(null);
  // True once the lookup of the reposted post ended without a post.
  let repostMissing = $state(false);
  let displayPost = $derived(isRepostOnly && quotedPost ? quotedPost : post);
  // Like, reply, repost and quote act on the displayed post. A repost-only
  // card whose quoted post is not stored locally has no target.
  let actionTarget = $derived(isRepostOnly ? quotedPost : post);
  // A repost-only card shows the quoted post as its content, so only the
  // quote of that post is embedded below it.
  let embeddedQuoteId = $derived(
    isRepostOnly ? (quotedPost?.quote_of ?? null) : post.quote_of,
  );

  // QuotedPostEmbed loads the quoted post of a quote that has content.
  $effect(() => {
    const id = repostTargetId;
    quotedPost = null;
    repostMissing = false;
    if (!id) return;
    let stale = false;
    invoke<Post | null>("get_post", { id })
      .then((loaded) => {
        if (stale) return;
        if (loaded?.id === id) quotedPost = loaded;
        else repostMissing = true;
      })
      .catch((e) => {
        console.error("Failed to load reposted post:", e);
        if (!stale) repostMissing = true;
      });
    return () => {
      stale = true;
    };
  });

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
  {#if isRepostOnly}
    <div class="repost-label">
      <a href={profileHref(post.author)} class="repost-author"
        >{repostAuthor.name}</a
      >
      <span>reposted</span>
    </div>
  {/if}

  <div class="post-header">
    {#if showAuthor || displayPost.author !== post.author}
      <a href={profileHref(displayPost.author)} class="author-link">
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
      <a href={postHref(displayPost.id)} class="time-link">
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

  {#if showReplyContext && displayPost.reply_to}
    <ReplyContextBlock replyToId={displayPost.reply_to} {pubkey} />
  {/if}

  {#if displayPost.content}
    <p class="post-content">
      {@html renderContent(displayPost.content, pubkey)}
    </p>
  {/if}
  <MediaGrid media={displayPost.media} {onlightbox} />
  {#if embeddedQuoteId}
    <QuotedPostEmbed quoteOfId={embeddedQuoteId} {pubkey} />
  {/if}
  {#if repostMissing}
    <p class="repost-missing">Reposted post unavailable</p>
  {/if}

  {#if actionTarget}
    {@const target = actionTarget}
    <PostActions
      postId={target.id}
      postAuthor={target.author}
      onreply={onreply ? () => onreply?.(target) : undefined}
      onquote={onquote ? () => onquote?.(target) : undefined}
    />
  {/if}
</article>

<style>
  .post {
    padding: 1.1rem 0.25rem;
    border-bottom: 1px solid var(--border);
    animation: fadeIn var(--transition-slow) ease-out;
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

  .repost-missing {
    margin: 0;
    color: var(--text-muted);
    font-size: var(--text-base);
    font-style: italic;
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
    font-family: var(--font-display);
    font-optical-sizing: auto;
    font-weight: 600;
    font-size: var(--text-lg);
    letter-spacing: -0.005em;
    color: var(--text-primary);
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
    font-variant-numeric: tabular-nums;
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
    line-height: 1.6;
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
