<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { postHref } from "#lib/profile/links.ts";
  import type { Post } from "#lib/types.ts";
  import { getDisplayName, truncate } from "#lib/utils.ts";

  let {
    replyToId,
    pubkey,
  }: {
    replyToId: string;
    pubkey: string;
  } = $props();

  const PREVIEW_CHARS = 100;

  let context = $state<{ author: string; preview: string } | null>(null);

  $effect(() => {
    const parentId = replyToId;
    const selfId = pubkey;
    context = null;
    let stale = false;
    (async () => {
      try {
        const parent = await invoke<Post | null>("get_post", { id: parentId });
        if (!parent || stale) return;
        const author = await getDisplayName(parent.author, selfId);
        if (stale) return;
        const cut = truncate(parent.content, PREVIEW_CHARS);
        const preview = cut === parent.content ? cut : cut + "...";
        context = { author, preview };
      } catch {
        // The parent post is not stored locally.
      }
    })();
    return () => {
      stale = true;
    };
  });
</script>

{#if context}
  <a href={postHref(replyToId)} class="reply-context-block">
    <span class="reply-icon">{"\u21A9"}</span>
    <span class="reply-author">{context.author}</span>
    {#if context.preview}
      <span class="reply-preview">{context.preview}</span>
    {/if}
  </a>
{:else}
  <a href={postHref(replyToId)} class="reply-context">
    {"\u21A9"} in reply to a post
  </a>
{/if}

<style>
  .reply-context {
    display: block;
    margin-bottom: 0.35rem;
    font-size: var(--text-sm);
    color: var(--text-tertiary);
    text-decoration: none;
  }

  .reply-context:hover {
    color: var(--accent-medium);
    text-decoration: underline;
  }

  .reply-context-block {
    display: flex;
    align-items: baseline;
    gap: 0.3rem;
    margin-bottom: 0.5rem;
    padding: 0.35rem 0.6rem;
    background: var(--bg-deep);
    border-left: 2px solid var(--border-hover);
    border-radius: 0 var(--radius-md) var(--radius-md) 0;
    font-size: var(--text-sm);
    color: var(--text-secondary);
    text-decoration: none;
    overflow: hidden;
  }

  .reply-context-block:hover {
    border-left-color: var(--accent-medium);
    color: var(--accent-medium);
  }

  .reply-icon {
    flex-shrink: 0;
    color: var(--text-tertiary);
  }

  .reply-author {
    color: var(--accent-light);
    font-weight: 600;
    flex-shrink: 0;
  }

  .reply-preview {
    color: var(--text-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
