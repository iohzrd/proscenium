<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let {
    onback,
    onverified,
  }: {
    onback: () => void;
    onverified: () => void;
  } = $props();

  let verifyIndices = $state<number[]>([]);
  let verifyInputs = $state<string[]>(["", "", ""]);
  let verifyError = $state("");
  let verifying = $state(false);

  function pickVerifyIndices() {
    const indices: number[] = [];
    while (indices.length < 3) {
      const i = Math.floor(Math.random() * 24);
      if (!indices.includes(i)) indices.push(i);
    }
    indices.sort((a, b) => a - b);
    verifyIndices = indices;
    verifyInputs = ["", "", ""];
    verifyError = "";
  }

  // Pick indices on first render
  pickVerifyIndices();

  async function verifySeedPhrase() {
    verifying = true;
    verifyError = "";
    try {
      const checks: [number, string][] = verifyIndices.map((idx, i) => [
        idx,
        verifyInputs[i].trim().toLowerCase(),
      ]);
      const valid = await invoke<boolean>("verify_seed_phrase_words", {
        checks,
      });
      if (valid) {
        await invoke("mark_seed_phrase_backed_up");
        onverified();
      } else {
        verifyError = "One or more words are incorrect. Please try again.";
      }
    } catch (e) {
      verifyError = `Verification failed: ${e}`;
    }
    verifying = false;
  }
</script>

<div class="step">
  <h2>Verify Your Phrase</h2>
  <p class="desc">
    Enter the following words from your recovery phrase to confirm you've saved
    it correctly.
  </p>

  <div class="verify-fields">
    {#each verifyIndices as wordIdx, i}
      <label class="verify-field">
        <span class="verify-label">Word #{wordIdx + 1}</span>
        <input
          class="input-base"
          type="text"
          bind:value={verifyInputs[i]}
          placeholder="Enter word"
          autocapitalize="none"
          autocomplete="off"
        />
      </label>
    {/each}
  </div>

  {#if verifyError}
    <p class="error">{verifyError}</p>
  {/if}

  <div class="actions">
    <button class="secondary-btn" onclick={onback}>Back</button>
    <button
      class="btn-accent primary-btn"
      onclick={verifySeedPhrase}
      disabled={verifyInputs.some((w) => !w.trim()) || verifying}
    >
      {verifying ? "Verifying..." : "Verify"}
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

  .verify-fields {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    margin-bottom: 1rem;
    text-align: left;
  }

  .verify-field {
    display: block;
  }

  .verify-label {
    display: block;
    color: var(--text-secondary);
    font-size: var(--text-sm);
    font-weight: 600;
    margin-bottom: 0.25rem;
  }

  .error {
    color: var(--color-danger);
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
