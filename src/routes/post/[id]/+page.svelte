<script lang="ts">
  import { page } from "$app/state";
  import { isPostIdFormat } from "#lib/utils.ts";
  import Thread from "./Thread.svelte";

  let postId = $derived(page.params.id ?? "");
</script>

{#if isPostIdFormat(postId)}
  <!-- SvelteKit reuses this page component across posts. The key block
       creates a new Thread, with new state, for each post id. -->
  {#key postId}
    <Thread {postId} />
  {/key}
{:else}
  <a href="/" class="back-link">&larr; Back to feed</a>
  <div class="empty">
    <p>This link does not contain a valid post id.</p>
  </div>
{/if}
