<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import type { ServerEntry } from "$lib/types";

  let nodeId = $state("");
  let servers = $state<ServerEntry[]>([]);
  let newServerUrl = $state("");
  let adding = $state(false);
  let serverStatus = $state("");
  let pendingRemoveUrl = $state<string | null>(null);

  async function loadServers() {
    try {
      servers = await invoke("list_servers");
    } catch (e) {
      console.error("Failed to load servers:", e);
    }
  }

  async function addServer() {
    const url = newServerUrl.trim();
    if (!url) return;
    adding = true;
    serverStatus = "";
    try {
      const entry: ServerEntry = await invoke("add_server", { url });
      newServerUrl = "";
      await loadServers();
      serverStatus = `Added ${entry.name || entry.url}`;
      setTimeout(() => (serverStatus = ""), 3000);
    } catch (e) {
      serverStatus = `Error: ${e}`;
    }
    adding = false;
  }

  async function removeServer() {
    if (!pendingRemoveUrl) return;
    try {
      await invoke("remove_server", { url: pendingRemoveUrl });
      await loadServers();
    } catch (e) {
      serverStatus = `Error: ${e}`;
    }
    pendingRemoveUrl = null;
  }

  function handleServerKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addServer();
    }
  }

  onMount(async () => {
    try {
      nodeId = await invoke<string>("get_node_id");
    } catch {
      // Node not ready
    }
    await loadServers();
  });
</script>

<h2>Settings</h2>

<section class="settings-section">
  <h3>Identity</h3>
  <div class="setting-row">
    <span class="setting-label">Node ID</span>
    <code class="setting-value">{nodeId || "..."}</code>
  </div>
</section>

<section class="settings-section">
  <h3>Discovery Servers</h3>
  <div class="server-add-row">
    <input
      class="input-base server-input"
      bind:value={newServerUrl}
      placeholder="Server URL (e.g. http://54.201.25.68:3000)"
      onkeydown={handleServerKey}
    />
    <button
      class="server-add-btn"
      onclick={addServer}
      disabled={!newServerUrl.trim() || adding}
    >
      {adding ? "..." : "Add"}
    </button>
  </div>
  {#if serverStatus}
    <p class="server-status">{serverStatus}</p>
  {/if}
  {#if servers.length > 0}
    <div class="server-list">
      {#each servers as server (server.url)}
        <div class="server-row">
          <Icon name="server" size={16} />
          <div class="server-row-info">
            <span class="server-row-name">{server.name || server.url}</span>
            {#if server.name}
              <span class="server-row-url">{server.url}</span>
            {/if}
          </div>
          {#if server.registered_at}
            <span class="server-badge">Registered</span>
          {/if}
          <button
            class="server-remove-btn"
            onclick={() => (pendingRemoveUrl = server.url)}
            aria-label="Remove server"
          >
            <Icon name="x" size={14} />
          </button>
        </div>
      {/each}
    </div>
  {:else}
    <p class="server-empty">No servers added.</p>
  {/if}
  <a href="/servers" class="server-manage-link">Manage servers</a>
</section>

<section class="settings-section">
  <h3>Devices</h3>
  <p class="section-desc">
    Link multiple devices to share your identity, follows, and messages.
  </p>
  <a href="/settings/devices" class="server-manage-link">Manage devices</a>
</section>

{#if pendingRemoveUrl}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="modal-overlay"
    onclick={() => (pendingRemoveUrl = null)}
    role="presentation"
  >
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="modal"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label="Confirm remove server"
    >
      <p>Remove this server? This will not unregister you from it.</p>
      <div class="modal-actions">
        <button class="modal-cancel" onclick={() => (pendingRemoveUrl = null)}
          >Cancel</button
        >
        <button class="modal-confirm" onclick={removeServer}>Remove</button>
      </div>
    </div>
  </div>
{/if}

<style>
  h2 {
    margin: 0 0 1.5rem;
    font-size: var(--text-xl);
    color: var(--text-primary);
  }

  h3 {
    margin: 0 0 0.75rem;
    font-size: var(--text-lg);
    color: var(--text-primary);
  }

  .settings-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1rem 1.25rem;
  }

  .setting-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .setting-label {
    color: var(--text-secondary);
    font-weight: 500;
    white-space: nowrap;
  }

  .setting-value {
    color: var(--text-primary);
    font-size: var(--text-sm);
    word-break: break-all;
  }

  .settings-section + .settings-section {
    margin-top: 1.25rem;
  }

  .server-add-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
  }

  .server-input {
    flex: 1;
  }

  .server-add-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 0.85rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    font-family: inherit;
  }

  .server-add-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .server-add-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .server-status {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    margin: 0 0 0.5rem;
  }

  .server-list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .server-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 0.6rem;
    background: var(--bg-elevated);
    border-radius: var(--radius-md);
    color: var(--text-muted);
  }

  .server-row-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .server-row-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .server-row-url {
    font-size: var(--text-xs);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .server-badge {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--color-success, #22c55e);
    flex-shrink: 0;
  }

  .server-remove-btn {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0.2rem;
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .server-remove-btn:hover {
    color: var(--color-error, #ef4444);
    background: var(--bg-surface);
  }

  .server-empty {
    font-size: var(--text-sm);
    color: var(--text-muted);
    margin: 0;
  }

  .server-manage-link {
    display: inline-block;
    margin-top: 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent-light);
    text-decoration: none;
  }

  .server-manage-link:hover {
    text-decoration: underline;
  }

  .section-desc {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
    line-height: 1.5;
  }
</style>
