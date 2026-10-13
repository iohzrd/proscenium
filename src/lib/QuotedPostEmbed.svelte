<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "$lib/Avatar.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import { useDisplayName } from "$lib/name.svelte";
  import { postHref } from "$lib/profile/links";
  import type { Post } from "$lib/types";
  import { shortId, getCachedAvatarTicket, truncate } from "$lib/utils";

  let {
    quoteOfId,
    pubkey,
  }: {
    quoteOfId: string;
    pubkey: string;
  } = $props();

  const PREVIEW_CHARS = 200;

  let quotedPost = $state<Post | null>(null);

  const author = useDisplayName(
    () => quotedPost?.author ?? "",
    () => pubkey,
  );

  let preview = $derived.by(() => {
    const content = quotedPost?.content ?? "";
    const cut = truncate(content, PREVIEW_CHARS);
    return cut === content ? cut : cut + "...";
  });

  $effect(() => {
    const id = quoteOfId;
    quotedPost = null;
    let stale = false;
    invoke<Post | null>("get_post", { id })
      .then((loaded) => {
        if (!stale && loaded?.id === id) quotedPost = loaded;
      })
      .catch(() => {
        // The quoted post is not stored locally.
      });
    return () => {
      stale = true;
    };
  });
</script>

{#if quotedPost}
  <a href={postHref(quotedPost.id)} class="quoted-post">
    <div class="quoted-header">
      <Avatar
        pubkey={quotedPost.author}
        name={author.name || shortId(quotedPost.author)}
        isSelf={quotedPost.author === pubkey}
        ticket={getCachedAvatarTicket(quotedPost.author)}
        size={20}
      />
      <span class="quoted-author">
        {author.name || shortId(quotedPost.author)}
      </span>
      <Timeago timestamp={quotedPost.timestamp} />
    </div>
    {#if preview}
      <p class="quoted-content">{preview}</p>
    {/if}
  </a>
{:else}
  <a href={postHref(quoteOfId)} class="quoted-post unavailable">
    Quoted post unavailable
  </a>
{/if}

<style>
  .quoted-post {
    display: block;
    margin-top: 0.6rem;
    padding: 0.4rem 0 0.4rem 0.85rem;
    border-left: 2px solid var(--border-hover);
    text-decoration: none;
    color: inherit;
    transition: border-color var(--transition-normal);
  }

  .quoted-post:hover {
    border-color: var(--accent-medium);
  }

  .quoted-post.unavailable {
    color: var(--text-muted);
    font-size: var(--text-base);
    font-style: italic;
  }

  .quoted-header {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    margin-bottom: 0.3rem;
    font-size: var(--text-sm);
    color: var(--text-secondary);
  }

  .quoted-author {
    color: var(--accent-light);
    font-weight: 600;
  }

  .quoted-content {
    margin: 0;
    font-size: var(--text-base);
    line-height: 1.4;
    color: var(--text-quoted);
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
