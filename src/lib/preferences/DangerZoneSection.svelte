<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let showWipeConfirm = $state(false);
  let wiping = $state(false);
  let wipeStatus = $state("");

  async function wipeAllData() {
    wiping = true;
    wipeStatus = "";
    try {
      await invoke("wipe_all_data");
      wipeStatus = "All data deleted. Restart the app to start fresh.";
      showWipeConfirm = false;
    } catch (e) {
      wipeStatus = `Error: ${e}`;
    }
    wiping = false;
  }
</script>

<section class="settings-section danger-section">
  <h3>Danger Zone</h3>
  <p class="section-desc">
    Permanently delete all local data including posts, messages, follows,
    notifications, and preferences. Your identity keys are preserved but all
    content is gone forever.
  </p>
  {#if wipeStatus}
    <p class="wipe-status">{wipeStatus}</p>
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
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="modal-overlay"
    onclick={() => (showWipeConfirm = false)}
    role="presentation"
  >
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="modal"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label="Confirm delete all data"
    >
      <p>
        Are you sure you want to delete all data? This will permanently remove
        all posts, messages, follows, notifications, bookmarks, and preferences.
        This cannot be undone.
      </p>
      <div class="modal-actions">
        <button class="modal-cancel" onclick={() => (showWipeConfirm = false)}
          >Cancel</button
        >
        <button
          class="modal-confirm modal-confirm-danger"
          onclick={wipeAllData}
          disabled={wiping}
          >{wiping ? "Deleting..." : "Delete everything"}</button
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
