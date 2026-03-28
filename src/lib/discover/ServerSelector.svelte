<script lang="ts">
  import type { ServerEntry } from "$lib/types";

  let {
    servers,
    activeServer,
    onselect,
  }: {
    servers: ServerEntry[];
    activeServer: ServerEntry | null;
    onselect: (server: ServerEntry) => void;
  } = $props();
</script>

<div class="server-selector">
  {#each servers as server (server.url)}
    <button
      class="server-chip"
      class:active={activeServer?.url === server.url}
      onclick={() => onselect(server)}
    >
      {server.name || server.url}
    </button>
  {/each}
</div>

<style>
  .server-selector {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
    margin-bottom: 0.75rem;
  }

  .server-chip {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    padding: 0.3rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--text-secondary);
    cursor: pointer;
    font-family: inherit;
    transition:
      background var(--transition-fast),
      color var(--transition-fast);
  }

  .server-chip.active {
    background: var(--accent);
    color: var(--text-on-accent);
    border-color: var(--accent);
  }

  .server-chip:hover:not(.active) {
    color: var(--accent-light);
  }
</style>
