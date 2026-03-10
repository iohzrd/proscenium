<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import type { Post } from "$lib/types";
  import { getDisplayName } from "$lib/utils";

  let {
    replyToId,
    pubkey,
  }: {
    replyToId: string;
    pubkey: string;
  } = $props();

  let context = $state<{ author: string; preview: string } | null>(null);

  $effect(() => {
    loadReplyContext(replyToId);
  });

  async function loadReplyContext(parentId: string) {
    try {
      const parent: Post | null = await invoke("get_post", { id: parentId });
      if (parent) {
        const name = await getDisplayName(parent.author, pubkey);
        const preview =
          parent.content.length > 100
            ? parent.content.slice(0, 100) + "..."
            : parent.content;
        context = { author: name, preview };
      }
    } catch {
      // parent not available locally
    }
  }
</script>

{#if context}
  <a href="/post/{replyToId}" class="reply-context-block">
    <span class="reply-icon">{"\u21A9"}</span>
    <span class="reply-author">{context.author}</span>
    {#if context.preview}
      <span class="reply-preview">{context.preview}</span>
    {/if}
  </a>
{:else}
  <a href="/post/{replyToId}" class="reply-context">
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
