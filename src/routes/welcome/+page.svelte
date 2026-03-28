<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import WelcomeIntro from "$lib/welcome/WelcomeIntro.svelte";
  import RecoveryForm from "$lib/welcome/RecoveryForm.svelte";
  import SeedPhraseDisplay from "$lib/welcome/SeedPhraseDisplay.svelte";
  import SeedVerification from "$lib/welcome/SeedVerification.svelte";
  import ProfileSetupForm from "$lib/welcome/ProfileSetupForm.svelte";

  let step = $state(0);
  let nodeId = $state("");
  let masterPubkey = $state("");
  let seedPhrase = $state("");

  onMount(async () => {
    try {
      [nodeId, masterPubkey] = await Promise.all([
        invoke<string>("get_node_id"),
        invoke<string>("get_pubkey"),
      ]);
      const profile = await invoke("get_my_profile");
      if (profile) {
        goto("/");
        return;
      }
      const backedUp = await invoke<boolean>("is_seed_phrase_backed_up");
      if (backedUp) {
        step = 3;
      }
    } catch {
      setTimeout(() => location.reload(), 500);
    }
  });

  async function loadSeedAndContinue() {
    seedPhrase = await invoke<string>("get_seed_phrase");
    step = 1;
  }
</script>

<div class="welcome">
  {#if step === 0}
    <WelcomeIntro
      {nodeId}
      {masterPubkey}
      oncontinue={loadSeedAndContinue}
      onrecover={() => (step = -1)}
    />
  {:else if step === -1}
    <RecoveryForm onback={() => (step = 0)} onprofile={() => (step = 3)} />
  {:else if step === 1}
    <SeedPhraseDisplay
      {seedPhrase}
      onback={() => (step = 0)}
      onverify={() => (step = 2)}
      onskip={() => (step = 3)}
    />
  {:else if step === 2}
    <SeedVerification onback={() => (step = 1)} onverified={() => (step = 3)} />
  {:else}
    <ProfileSetupForm onback={() => (step = 1)} />
  {/if}
</div>

<style>
  .welcome {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 70vh;
    padding: 2rem 1rem;
  }
</style>
