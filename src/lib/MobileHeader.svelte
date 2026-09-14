<script lang="ts">
  import type { NodeStatus } from "$lib/types";

  interface Props {
    status: NodeStatus | null;
  }

  let { status }: Props = $props();
</script>

<header class="mobile-header">
  <span class="app-name">proscenium</span>
  {#if status}
    <span
      class="status-dot"
      class:connected={status.has_relay}
      class:disconnected={!status.has_relay}
      title={status.has_relay ? "Relay connected" : "No relay connection"}
    ></span>
  {/if}
</header>

<style>
  .mobile-header {
    position: sticky;
    top: 0;
    height: var(--mobile-header-height);
    padding: 0 var(--space-xl);
    padding-top: env(safe-area-inset-top);
    background: var(--bg-base);
    border-bottom: 1px solid var(--border);
    display: flex;
    align-items: center;
    justify-content: space-between;
    z-index: var(--z-mobile-header);
  }

  .app-name {
    font-family: var(--font-display);
    font-style: italic;
    font-weight: 500;
    font-size: var(--text-xl);
    letter-spacing: -0.02em;
    color: var(--text-primary);
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .status-dot.connected {
    background: var(--color-success);
  }

  .status-dot.disconnected {
    background: var(--color-error);
  }

  @media (min-width: 768px) {
    .mobile-header {
      display: none;
    }
  }
</style>
