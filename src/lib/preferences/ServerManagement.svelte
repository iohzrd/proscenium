<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import Icon from "#lib/Icon.svelte";
  import Modal from "#lib/Modal.svelte";
  import type { ServerEntry } from "#lib/types.ts";

  let servers = $state<ServerEntry[]>([]);
  let newServerUrl = $state("");
  let adding = $state(false);
  let serverStatus = $state("");
  let statusTimer: ReturnType<typeof setTimeout> | undefined;
  let pendingRemoveUrl = $state<string | null>(null);

  /** Parses `input` as an absolute http or https URL; null otherwise. */
  function parseServerUrl(input: string): URL | null {
    try {
      const url = new URL(input);
      const isHttp = url.protocol === "https:" || url.protocol === "http:";
      return isHttp && url.hostname ? url : null;
    } catch {
      return null;
    }
  }

  function isUnencrypted(url: string): boolean {
    return parseServerUrl(url)?.protocol === "http:";
  }

  let newUrlUnencrypted = $derived(isUnencrypted(newServerUrl.trim()));

  function setStatus(message: string, clearAfterMs?: number) {
    clearTimeout(statusTimer);
    serverStatus = message;
    if (clearAfterMs !== undefined) {
      statusTimer = setTimeout(() => (serverStatus = ""), clearAfterMs);
    }
  }

  async function loadServers() {
    try {
      servers = await invoke("list_servers");
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
  }

  async function addServer() {
    const url = newServerUrl.trim();
    if (!url || adding) return;
    if (!parseServerUrl(url)) {
      setStatus("Enter a URL that starts with https:// or http://.");
      return;
    }
    adding = true;
    setStatus("");
    try {
      const entry: ServerEntry = await invoke("add_server", { url });
      newServerUrl = "";
      await loadServers();
      setStatus(`Added ${entry.name || entry.url}`, 3000);
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
    adding = false;
  }

  async function removeServer() {
    const url = pendingRemoveUrl;
    if (!url) return;
    pendingRemoveUrl = null;
    try {
      await invoke("remove_server", { url });
      await loadServers();
    } catch (e) {
      setStatus(`Error: ${e}`);
    }
  }

  function handleServerKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.isComposing) {
      e.preventDefault();
      addServer();
    }
  }

  onMount(() => {
    loadServers();
  });

  onDestroy(() => clearTimeout(statusTimer));
</script>

<section class="settings-section">
  <h3>Discovery Servers</h3>
  <div class="server-add-row">
    <input
      class="input-base server-input"
      bind:value={newServerUrl}
      placeholder="Server URL (e.g. https://discovery.example.org)"
      aria-label="Server URL"
      autocapitalize="none"
      autocomplete="off"
      spellcheck="false"
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
  {#if newUrlUnencrypted}
    <p class="server-warning">
      http:// is not encrypted. Registration, searches and results sent to this
      server can be read and changed in transit.
    </p>
  {/if}
  {#if serverStatus}
    <p class="server-status" role="status">{serverStatus}</p>
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
          {#if isUnencrypted(server.url)}
            <span
              class="server-badge unencrypted"
              title="Traffic to this server is not encrypted">Unencrypted</span
            >
          {/if}
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

{#if pendingRemoveUrl}
  <Modal
    label="Confirm remove server"
    onclose={() => (pendingRemoveUrl = null)}
  >
    <p>Remove this server? This will not unregister you from it.</p>
    <div class="modal-actions">
      <button class="modal-cancel" onclick={() => (pendingRemoveUrl = null)}
        >Cancel</button
      >
      <button class="modal-confirm" onclick={removeServer}>Remove</button>
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

  .server-badge.unencrypted {
    color: var(--color-warning);
  }

  .server-warning {
    font-size: var(--text-sm);
    color: var(--color-warning);
    margin: 0 0 0.5rem;
    line-height: 1.4;
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
</style>
