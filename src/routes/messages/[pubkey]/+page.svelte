<script lang="ts">
  import { page } from "$app/state";
  import { isPubkeyFormat } from "#lib/utils.ts";
  import Conversation from "./Conversation.svelte";

  let pubkey = $derived(page.params.pubkey ?? "");
</script>

{#if isPubkeyFormat(pubkey)}
  <!-- SvelteKit reuses this page component across conversations. The key
       block creates a new Conversation, with new state, for each peer. -->
  {#key pubkey}
    <Conversation {pubkey} />
  {/key}
{:else}
  <a href="/messages" class="back-link">&larr; Back to messages</a>
  <div class="empty">
    <p>This link does not contain a valid public key.</p>
  </div>
{/if}
