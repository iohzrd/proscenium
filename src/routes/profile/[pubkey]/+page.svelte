<script lang="ts">
  import { page } from "$app/state";
  import { isPubkeyFormat } from "#lib/utils.ts";
  import ProfileView from "./ProfileView.svelte";

  let pubkey = $derived(page.params.pubkey ?? "");
  // Node ID from an invite link; used only if no Node ID is cached for the user.
  let transportHint = $derived.by(() => {
    const transport = page.url.searchParams.get("transport");
    return transport && isPubkeyFormat(transport) ? transport : null;
  });
</script>

{#if isPubkeyFormat(pubkey)}
  <!-- SvelteKit reuses this page component across profiles. The key block
       creates a new ProfileView, with new state, for each pubkey. -->
  {#key pubkey}
    <ProfileView {pubkey} {transportHint} />
  {/key}
{:else}
  <a href="/" class="back-link">&larr; Back to feed</a>
  <div class="empty">
    <p>This link does not contain a valid public key.</p>
  </div>
{/if}
