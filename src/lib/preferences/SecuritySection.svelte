<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let rotating = $state(false);
  let rotateStatus = $state("");
  let showRotateConfirm = $state(false);

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
  <p class="section-desc">
    Rotate your signing key if you suspect a device has been compromised. This
    derives a new signing key, notifies peers, and re-registers with servers.
    The app will need to restart after rotation.
  </p>
  {#if rotateStatus}
    <p class="rotate-status">{rotateStatus}</p>
  {/if}
  <button
    class="rotate-btn"
    onclick={() => (showRotateConfirm = true)}
    disabled={rotating}
  >
    {rotating ? "Rotating..." : "Rotate signing key"}
  </button>
</section>

{#if showRotateConfirm}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="modal-overlay"
    onclick={() => (showRotateConfirm = false)}
    role="presentation"
  >
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="modal"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label="Confirm key rotation"
    >
      <p>
        Rotate your signing key? This will invalidate the current signing key
        and derive a new one. All peers will be notified and DM sessions will
        need to be re-established. The app must restart after rotation.
      </p>
      <div class="modal-actions">
        <button class="modal-cancel" onclick={() => (showRotateConfirm = false)}
          >Cancel</button
        >
        <button class="modal-confirm" onclick={rotateKey} disabled={rotating}
          >{rotating ? "Rotating..." : "Rotate"}</button
        >
      </div>
    </div>
  </div>
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

  .section-desc {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
    line-height: 1.5;
  }

  .rotate-btn {
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

  .rotate-btn:hover:not(:disabled) {
    border-color: var(--color-error, #ef4444);
    color: var(--color-error, #ef4444);
  }

  .rotate-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .rotate-status {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    margin: 0 0 0.5rem;
  }
</style>
