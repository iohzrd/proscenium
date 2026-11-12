<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { PHRASE_INPUT_ATTRS } from "./phrase-input";

  let {
    onback,
    onprofile,
  }: {
    onback: () => void;
    /** Called after a successful recovery, before the webview reloads. */
    onprofile?: () => void;
  } = $props();

  let recoveryPhrase = $state("");
  let recovering = $state(false);
  let recoveryError = $state("");

  async function recoverFromPhrase() {
    const words = recoveryPhrase.trim().toLowerCase().split(/\s+/);
    if (words.length !== 24) {
      recoveryError = "Recovery phrase must be exactly 24 words.";
      return;
    }
    recovering = true;
    recoveryError = "";
    try {
      // `recover_from_seed_phrase` shows a native confirmation dialog, then
      // replaces the identity.
      await invoke("recover_from_seed_phrase", { phrase: words.join(" ") });
    } catch (e) {
      // "cancelled": the native dialog was declined. Nothing was replaced.
      if (String(e) !== "cancelled") recoveryError = `Recovery failed: ${e}`;
      recovering = false;
      return;
    }
    recoveryPhrase = "";
    onprofile?.();
    // The layout and every page hold the previous pubkey and node id.
    location.reload();
  }
</script>

<div class="step">
  <h2>Recover Your Identity</h2>
  <p class="desc">
    Enter your 24-word recovery phrase to restore your identity on this device.
    This replaces the identity and deletes all data stored on this device.
  </p>
  <p class="desc">
    Use recovery when your other devices are lost. The recovered device
    announces itself as your only device, so others stop reaching your other
    devices. To add a device while you still have one, link it from Linked
    Devices instead.
  </p>

  <label class="field">
    <span class="field-label">Recovery Phrase</span>
    <textarea
      class="input-base recovery-textarea"
      bind:value={recoveryPhrase}
      placeholder="Enter your 24 words separated by spaces"
      rows="4"
      {...PHRASE_INPUT_ATTRS}
      disabled={recovering}></textarea>
  </label>

  {#if recoveryError}
    <p class="error" role="alert">{recoveryError}</p>
  {/if}

  <div class="actions">
    <button class="secondary-btn" onclick={onback} disabled={recovering}
      >Back</button
    >
    <button
      class="btn-accent primary-btn"
      onclick={recoverFromPhrase}
      disabled={!recoveryPhrase.trim() || recovering}
    >
      {recovering ? "Recovering..." : "Recover Identity"}
    </button>
  </div>
</div>

<style>
  .step {
    max-width: 420px;
    width: 100%;
    text-align: center;
  }

  h2 {
    font-size: var(--text-2xl);
    color: var(--text-primary);
    margin: 0 0 1.5rem;
  }

  .desc {
    color: var(--text-secondary);
    font-size: var(--text-base);
    line-height: 1.6;
    margin: 0 0 1.5rem;
  }

  .field {
    display: block;
    text-align: left;
  }

  .recovery-textarea {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    resize: vertical;
    min-height: 80px;
  }

  .error {
    color: var(--color-error);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
  }

  .actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.5rem;
  }

  .primary-btn {
    flex: 1;
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
  }

  .secondary-btn {
    background: none;
    border: 1px solid var(--border-hover);
    color: var(--text-secondary);
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
    cursor: pointer;
    transition:
      color var(--transition-fast),
      border-color var(--transition-fast);
  }

  .secondary-btn:hover {
    color: var(--accent-light);
    border-color: var(--accent);
  }
</style>
