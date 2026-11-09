<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { useNodeInit } from "#lib/composables/index.ts";
  import WelcomeIntro from "#lib/welcome/WelcomeIntro.svelte";
  import RecoveryForm from "#lib/welcome/RecoveryForm.svelte";
  import SeedPhraseDisplay from "#lib/welcome/SeedPhraseDisplay.svelte";
  import SeedVerification from "#lib/welcome/SeedVerification.svelte";
  import ProfileSetupForm from "#lib/welcome/ProfileSetupForm.svelte";

  const RECOVERY = -1;
  const INTRO = 0;
  const SEED_DISPLAY = 1;
  const SEED_VERIFICATION = 2;
  const PROFILE_SETUP = 3;

  let step = $state(INTRO);
  // Held only while the SEED_DISPLAY step is shown.
  let seedPhrase = $state("");
  let error = $state("");
  let loadingPhrase = false;

  const node = useNodeInit(async () => {
    const profile = await invoke("get_my_profile");
    if (profile) {
      goto("/");
      return;
    }
    if (await invoke<boolean>("is_seed_phrase_backed_up")) {
      step = PROFILE_SETUP;
    }
  });

  /** Show `next`. Clears the phrase: no step entered through here displays it. */
  function goToStep(next: number) {
    seedPhrase = "";
    error = "";
    step = next;
  }

  /** Load the recovery phrase and show it. The step is unchanged if loading fails. */
  async function showSeedPhrase() {
    if (loadingPhrase) return;
    loadingPhrase = true;
    error = "";
    try {
      seedPhrase = await invoke<string>("get_seed_phrase");
      step = SEED_DISPLAY;
    } catch (e) {
      // "cancelled": the native confirmation was declined.
      if (String(e) !== "cancelled") {
        error = `Could not load the recovery phrase: ${e}`;
      }
    } finally {
      loadingPhrase = false;
    }
  }

  /** From the intro: profile setup if the phrase is backed up, else the phrase. */
  async function continueFromIntro() {
    let backedUp = false;
    try {
      backedUp = await invoke<boolean>("is_seed_phrase_backed_up");
    } catch {
      // treated as not backed up
    }
    if (backedUp) goToStep(PROFILE_SETUP);
    else await showSeedPhrase();
  }

  onMount(() => {
    node.init();
  });
</script>

<div class="welcome">
  {#if node.loading}
    <div class="loading">
      <div class="spinner"></div>
      <p>Starting node...</p>
    </div>
  {:else if node.error}
    <div class="empty">
      <p>{node.error}</p>
      <button class="btn-elevated" onclick={node.init}>Retry</button>
    </div>
  {:else}
    {#if error}
      <p class="welcome-error" role="alert">{error}</p>
    {/if}
    {#if step === INTRO}
      <WelcomeIntro
        nodeId={node.nodeId}
        masterPubkey={node.pubkey}
        oncontinue={continueFromIntro}
        onrecover={() => goToStep(RECOVERY)}
      />
    {:else if step === RECOVERY}
      <!-- RecoveryForm reloads the webview after a successful recovery. -->
      <RecoveryForm onback={() => goToStep(INTRO)} />
    {:else if step === SEED_DISPLAY}
      <SeedPhraseDisplay
        {seedPhrase}
        onback={() => goToStep(INTRO)}
        onverify={() => goToStep(SEED_VERIFICATION)}
        onskip={() => goToStep(PROFILE_SETUP)}
      />
    {:else if step === SEED_VERIFICATION}
      <SeedVerification
        onback={showSeedPhrase}
        onverified={() => goToStep(PROFILE_SETUP)}
      />
    {:else}
      <ProfileSetupForm onback={() => goToStep(INTRO)} />
    {/if}
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

  .welcome-error {
    color: var(--color-error-light);
    font-size: var(--text-base);
    margin: 0 0 1rem;
    text-align: center;
  }
</style>
