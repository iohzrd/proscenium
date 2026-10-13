<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import InlineComposer from "$lib/InlineComposer.svelte";
  import { useDisplayName } from "$lib/name.svelte";
  import type { Post } from "$lib/types";
  import { truncate } from "$lib/utils";

  let {
    quotedPost,
    pubkey,
    onsubmitted,
    oncancel,
  }: {
    quotedPost: Post;
    pubkey: string;
    onsubmitted?: () => void;
    oncancel?: () => void;
  } = $props();

  const PREVIEW_CHARS = 120;

  const author = useDisplayName(
    () => quotedPost.author,
    () => pubkey,
  );

  let preview = $derived.by(() => {
    const cut = truncate(quotedPost.content, PREVIEW_CHARS);
    return cut === quotedPost.content ? cut : cut + "...";
  });
</script>

<!-- An empty text creates a repost of the quoted post. -->
<InlineComposer
  {pubkey}
  placeholder="Add your commentary (optional)..."
  submitLabel="Quote"
  allowEmpty
  create={(content) =>
    invoke("create_post", {
      content,
      media: null,
      replyTo: null,
      replyToAuthor: null,
      quoteOf: quotedPost.id,
      quoteOfAuthor: quotedPost.author,
    })}
  {onsubmitted}
  {oncancel}
>
  {#snippet header()}
    <div class="quoted-preview">
      <span class="quote-author">{author.name}</span>
      {#if preview}
        <span class="quote-text">{preview}</span>
      {:else}
        <span class="quote-text empty">[no text]</span>
      {/if}
    </div>
  {/snippet}
</InlineComposer>

<style>
  .quoted-preview {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.5rem 0.7rem;
    margin-bottom: 0.5rem;
    background: var(--bg-deep);
    border-left: 2px solid var(--accent);
    border-radius: 0 var(--radius-md) var(--radius-md) 0;
    font-size: var(--text-base);
  }

  .quote-author {
    color: var(--accent-light);
    font-weight: 600;
    font-size: var(--text-sm);
  }

  .quote-text {
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .quote-text.empty {
    font-style: italic;
    color: var(--text-muted);
  }
</style>
