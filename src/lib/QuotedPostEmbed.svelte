<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "$lib/Avatar.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import { useDisplayName } from "$lib/name.svelte";
  import type { Post } from "$lib/types";
  import { shortId, getCachedAvatarTicket } from "$lib/utils";

  let {
    quoteOfId,
    pubkey,
  }: {
    quoteOfId: string;
    pubkey: string;
  } = $props();

  let quotedPost = $state<Post | null>(null);

  const author = useDisplayName(
    () => quotedPost?.author ?? "",
    () => pubkey,
  );

  $effect(() => {
    invoke("get_post", { id: quoteOfId })
      .then((qp) => {
        quotedPost = qp as Post | null;
      })
      .catch(() => {});
  });
</script>

{#if quotedPost}
  <a href="/post/{quotedPost.id}" class="quoted-post">
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
    {#if quotedPost.content}
      <p class="quoted-content">
        {quotedPost.content.length > 200
          ? quotedPost.content.slice(0, 200) + "..."
          : quotedPost.content}
      </p>
    {/if}
  </a>
{:else}
  <a href="/post/{quoteOfId}" class="quoted-post unavailable">
    Quoted post unavailable
  </a>
{/if}

<style>
  .quoted-post {
    display: block;
    margin-top: 0.6rem;
    padding: 0.6rem 0.75rem;
    background: var(--bg-deep);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    text-decoration: none;
    color: inherit;
    transition: border-color var(--transition-normal);
  }

  .quoted-post:hover {
    border-color: var(--border-hover);
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
