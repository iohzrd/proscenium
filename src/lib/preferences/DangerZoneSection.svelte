<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Modal from "$lib/Modal.svelte";

  let showWipeConfirm = $state(false);
  let wiping = $state(false);
  let wipeStatus = $state("");

  async function wipeAllData() {
    wiping = true;
    wipeStatus = "";
    try {
      // `wipe_all_data` shows a native confirmation dialog, deletes the data,
      // then restarts the app. The promise resolves only if the restart has
      // not happened yet.
      await invoke("wipe_all_data");
      wipeStatus = "All data deleted. The app is restarting.";
    } catch (e) {
      // "cancelled": the native dialog was declined. Nothing was deleted.
      if (String(e) !== "cancelled") wipeStatus = `Error: ${e}`;
    }
    showWipeConfirm = false;
    wiping = false;
  }
</script>

<section class="settings-section danger-section">
  <h3>Danger Zone</h3>
  <p class="section-desc">
    Delete all posts, messages, follows, notifications, bookmarks, preferences
    and downloaded media stored on this device, then restart the app. Your
    identity keys are kept. Copies that other peers or servers already hold are
    not deleted.
  </p>
  {#if wipeStatus}
    <p class="wipe-status" role="status">{wipeStatus}</p>
  {/if}
  <button
    class="wipe-btn"
    onclick={() => (showWipeConfirm = true)}
    disabled={wiping}
  >
    {wiping ? "Deleting..." : "Delete all data"}
  </button>
</section>

{#if showWipeConfirm}
  <Modal
    label="Confirm delete all data"
    onclose={() => {
      if (!wiping) showWipeConfirm = false;
    }}
  >
    <p>
      Delete all posts, messages, follows, notifications, bookmarks, preferences
      and downloaded media on this device? This cannot be undone. A system
      dialog asks once more, then the app restarts.
    </p>
    <div class="modal-actions">
      <button
        class="modal-cancel"
        onclick={() => (showWipeConfirm = false)}
        disabled={wiping}>Cancel</button
      >
      <button
        class="modal-confirm modal-confirm-danger"
        onclick={wipeAllData}
        disabled={wiping}>{wiping ? "Deleting..." : "Delete everything"}</button
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

  .section-desc {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
    line-height: 1.5;
  }

  .danger-section {
    border-color: var(--color-error);
  }

  .wipe-btn {
    background: var(--color-error);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    font-family: inherit;
  }

  .wipe-btn:hover:not(:disabled) {
    opacity: 0.9;
  }

  .wipe-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .wipe-status {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    margin: 0 0 0.5rem;
  }

  .modal-confirm-danger {
    background: var(--color-error);
    border-color: var(--color-error);
  }

  .modal-confirm-danger:hover:not(:disabled) {
    opacity: 0.9;
  }
</style>
