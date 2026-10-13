<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Modal from "$lib/Modal.svelte";
  import SeedVerification from "$lib/welcome/SeedVerification.svelte";
  import SeedWordGrid from "$lib/welcome/SeedWordGrid.svelte";

  let rotating = $state(false);
  let rotateStatus = $state("");
  let showRotateConfirm = $state(false);

  // null until `is_seed_phrase_backed_up` returned.
  let backedUp = $state<boolean | null>(null);
  let loadingPhrase = $state(false);
  let phraseError = $state("");
  // The recovery phrase is held only while its dialog is open.
  let phraseWords = $state.raw<string[]>([]);
  let phraseDialog = $state<"closed" | "words" | "verify">("closed");

  onMount(async () => {
    try {
      backedUp = await invoke<boolean>("is_seed_phrase_backed_up");
    } catch (e) {
      console.error("Failed to read recovery phrase backup state:", e);
    }
  });

  async function showPhrase() {
    loadingPhrase = true;
    phraseError = "";
    try {
      // `get_seed_phrase` shows a native confirmation dialog once the backup
      // marker file exists.
      const phrase = await invoke<string>("get_seed_phrase");
      phraseWords = phrase.split(" ");
      phraseDialog = "words";
    } catch (e) {
      // "cancelled": the native dialog was declined.
      if (String(e) !== "cancelled") phraseError = `Error: ${e}`;
    }
    loadingPhrase = false;
  }

  function closePhrase() {
    phraseWords = [];
    phraseDialog = "closed";
  }

  function startVerification() {
    phraseWords = [];
    phraseDialog = "verify";
  }

  async function rotateKey() {
    rotating = true;
    rotateStatus = "";
    try {
      const msg: string = await invoke("rotate_signing_key");
      rotateStatus = msg;
      showRotateConfirm = false;
    } catch (e) {
      rotateStatus = `Error: ${e}`;
    }
    rotating = false;
  }
</script>

<section class="settings-section">
  <h3>Security</h3>

  <h4>Recovery phrase</h4>
  {#if backedUp === false}
    <p class="backup-reminder" role="status">
      The recovery phrase is not backed up. If this device is lost or its data
      is deleted, the identity cannot be recovered. Show the phrase, write it
      down, then verify it.
    </p>
  {:else}
    <p class="section-desc">
      The 24-word recovery phrase restores this identity on another device.
      Anyone who reads it controls the identity.
    </p>
  {/if}
  {#if phraseError}
    <p class="section-status" role="alert">{phraseError}</p>
  {/if}
  <button class="section-btn" onclick={showPhrase} disabled={loadingPhrase}>
    {loadingPhrase ? "Waiting for confirmation..." : "Show recovery phrase"}
  </button>

  <h4>Signing key</h4>
  <p class="section-desc">
    Rotate your signing key if you suspect a device has been compromised. This
    derives a new signing key, notifies peers, and re-registers with servers.
    The app will need to restart after rotation.
  </p>
  {#if rotateStatus}
    <p class="section-status" role="status">{rotateStatus}</p>
  {/if}
  <button
    class="section-btn danger"
    onclick={() => (showRotateConfirm = true)}
    disabled={rotating}
  >
    {rotating ? "Rotating..." : "Rotate signing key"}
  </button>
</section>

{#if phraseDialog === "words"}
  <Modal label="Recovery phrase" onclose={closePhrase} maxWidth="460px">
    <p>
      Write these 24 words down in order. Do not copy them to the clipboard, a
      screenshot or a file.
    </p>
    <SeedWordGrid words={phraseWords} />
    <div class="modal-actions">
      <button class="modal-cancel" onclick={closePhrase}>Close</button>
      {#if backedUp === false}
        <button class="modal-confirm save" onclick={startVerification}>
          Verify backup
        </button>
      {/if}
    </div>
  </Modal>
{:else if phraseDialog === "verify"}
  <Modal label="Verify recovery phrase" onclose={closePhrase} maxWidth="460px">
    <SeedVerification
      onback={closePhrase}
      onverified={() => {
        backedUp = true;
        closePhrase();
      }}
    />
  </Modal>
{/if}

{#if showRotateConfirm}
  <Modal
    label="Confirm key rotation"
    onclose={() => {
      if (!rotating) showRotateConfirm = false;
    }}
  >
    <p>
      Rotate your signing key? This will invalidate the current signing key and
      derive a new one. All peers will be notified and DM sessions will need to
      be re-established. The app must restart after rotation.
    </p>
    <div class="modal-actions">
      <button
        class="modal-cancel"
        onclick={() => (showRotateConfirm = false)}
        disabled={rotating}>Cancel</button
      >
      <button class="modal-confirm" onclick={rotateKey} disabled={rotating}
        >{rotating ? "Rotating..." : "Rotate"}</button
      >
    </div>
  </Modal>
{/if}

<style>
  .settings-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1rem 1.25rem;
  }

  h3 {
    margin: 0 0 0.75rem;
    font-size: var(--text-lg);
    color: var(--text-primary);
  }

  h4 {
    margin: 0 0 0.4rem;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-primary);
  }

  .section-btn + h4 {
    margin-top: 1.25rem;
  }

  .section-desc {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
    line-height: 1.5;
  }

  .backup-reminder {
    color: var(--color-warning);
    background: var(--color-warning-bg-subtle);
    border: 1px solid var(--color-warning-border);
    border-radius: var(--radius-md);
    padding: 0.5rem 0.75rem;
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
    line-height: 1.5;
  }

  .section-btn {
    background: var(--bg-elevated);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    font-family: inherit;
  }

  .section-btn:hover:not(:disabled) {
    border-color: var(--accent-medium);
    color: var(--accent-light);
  }

  .section-btn.danger:hover:not(:disabled) {
    border-color: var(--color-error);
    color: var(--color-error);
  }

  .section-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .section-status {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    margin: 0 0 0.5rem;
  }
</style>
