<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { TERMS_VERSION, type LegalDocumentId } from "#lib/legal/documents.ts";
  import LegalDocumentModal from "#lib/legal/LegalDocumentModal.svelte";

  let { onaccepted }: { onaccepted: () => void } = $props();

  let agreed = $state(false);
  let saving = $state(false);
  let error = $state("");
  let shownDocument = $state<LegalDocumentId | null>(null);

  async function accept() {
    saving = true;
    error = "";
    try {
      await invoke("accept_terms", { version: TERMS_VERSION });
      onaccepted();
    } catch (e) {
      error = `Could not save the acceptance: ${e}`;
    } finally {
      saving = false;
    }
  }
</script>

<div class="terms-screen" role="dialog" aria-modal="true" aria-label="Terms">
  <div class="terms-box">
    <h1>Before you start</h1>
    <p class="desc">
      Proscenium is used under its Terms of Use. The Privacy Policy describes
      what the app stores and where it sends data. Terms dated {TERMS_VERSION}.
    </p>
    <div class="doc-links">
      <button class="btn-elevated" onclick={() => (shownDocument = "terms")}>
        Terms of Use
      </button>
      <button class="btn-elevated" onclick={() => (shownDocument = "privacy")}>
        Privacy Policy
      </button>
    </div>
    <label class="agree">
      <input type="checkbox" bind:checked={agreed} />
      <span>I agree to the Terms of Use and have read the Privacy Policy.</span>
    </label>
    {#if error}
      <p class="error" role="alert">{error}</p>
    {/if}
    <button
      class="btn-accent continue-btn"
      onclick={accept}
      disabled={!agreed || saving}
    >
      Continue
    </button>
  </div>
</div>

{#if shownDocument}
  <LegalDocumentModal
    docId={shownDocument}
    onclose={() => (shownDocument = null)}
  />
{/if}

<style>
  .terms-screen {
    position: fixed;
    inset: 0;
    z-index: calc(var(--z-modal) - 1);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1rem;
    background: var(--bg-deep);
  }

  .terms-box {
    max-width: 420px;
    width: 100%;
    text-align: center;
  }

  h1 {
    font-size: var(--text-2xl);
    color: var(--text-primary);
    margin: 0 0 1rem;
  }

  .desc {
    color: var(--text-secondary);
    font-size: var(--text-base);
    line-height: 1.6;
    margin: 0 0 1.25rem;
  }

  .doc-links {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 0.5rem;
    margin-bottom: 1.25rem;
  }

  .agree {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    margin: 0 0 1rem;
    text-align: left;
    color: var(--text-secondary);
    font-size: var(--text-sm);
    line-height: 1.5;
    cursor: pointer;
  }

  .agree input {
    margin-top: 0.2rem;
    accent-color: var(--accent);
  }

  .error {
    color: var(--color-error);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
  }

  .continue-btn {
    width: 100%;
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
  }
</style>
