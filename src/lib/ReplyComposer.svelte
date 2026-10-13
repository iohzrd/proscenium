<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import InlineComposer from "$lib/InlineComposer.svelte";

  let {
    replyToId,
    replyToAuthor,
    pubkey,
    onsubmitted,
    oncancel,
  }: {
    replyToId: string;
    replyToAuthor: string;
    pubkey: string;
    onsubmitted?: () => void;
    oncancel?: () => void;
  } = $props();
</script>

<InlineComposer
  {pubkey}
  placeholder="Write a reply..."
  submitLabel="Reply"
  create={(content) =>
    invoke("create_post", {
      content,
      media: null,
      replyTo: replyToId,
      replyToAuthor,
    })}
  {onsubmitted}
  {oncancel}
/>
