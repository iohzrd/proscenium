<script lang="ts">
  import type { NodeStatus } from "$lib/types";
  import Icon from "$lib/Icon.svelte";

  interface Props {
    pubkey: string;
    status: NodeStatus | null;
    unreadDmCount: number;
    unreadNotificationCount: number;
    currentPath: string;
  }

  let {
    pubkey,
    status,
    unreadDmCount,
    unreadNotificationCount,
    currentPath,
  }: Props = $props();
</script>

<aside class="sidebar">
  <div class="sidebar-brand">proscenium</div>
  <nav class="sidebar-nav">
    <a href="/" class:active={currentPath === "/"}>
      <Icon name="home" />
      <span class="nav-label">Timeline</span>
    </a>
    <a href="/activity" class:active={currentPath === "/activity"}>
      <Icon name="bell" />
      <span class="nav-label">Activity</span>
      {#if unreadNotificationCount > 0}
        <span class="unread-badge">{unreadNotificationCount}</span>
      {/if}
    </a>
    <a href="/messages" class:active={currentPath.startsWith("/messages")}>
      <Icon name="message-circle" />
      <span class="nav-label">Messages</span>
      {#if unreadDmCount > 0}
        <span class="unread-badge">{unreadDmCount}</span>
      {/if}
    </a>
    <a href="/discover" class:active={currentPath === "/discover"}>
      <Icon name="compass" />
      <span class="nav-label">Discover</span>
    </a>
    <a href="/stage" class:active={currentPath === "/stage"}>
      <Icon name="radio" />
      <span class="nav-label">Stage</span>
    </a>
    <a href="/follows" class:active={currentPath === "/follows"}>
      <Icon name="users" />
      <span class="nav-label">Follows</span>
    </a>
    {#if pubkey}
      <a
        href="/profile/{pubkey}"
        class:active={currentPath === `/profile/${pubkey}`}
      >
        <Icon name="user" />
        <span class="nav-label">Profile</span>
      </a>
    {/if}
    <a href="/preferences" class:active={currentPath === "/preferences"}>
      <Icon name="settings" />
      <span class="nav-label">Preferences</span>
    </a>
  </nav>
  <div class="sidebar-footer">
    {#if status}
      <span
        class="status-row"
        title={status.has_relay
          ? `Relay connected | ${status.follow_count} following | ${status.follower_count} follower(s)`
          : "No relay connection"}
      >
        <span
          class="status-dot"
          class:connected={status.has_relay}
          class:disconnected={!status.has_relay}
        ></span>
        <span class="status-text">
          {status.has_relay ? "Connected" : "Disconnected"}
        </span>
      </span>
    {/if}
  </div>
</aside>

<style>
  .sidebar {
    position: fixed;
    left: 0;
    top: 0;
    bottom: 0;
    width: var(--sidebar-width);
    background: var(--bg-deep);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    z-index: var(--z-sidebar);
    padding: var(--space-lg) 0;
  }

  /* Pillar inner-edge glow */
  .sidebar::after {
    content: "";
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: 32px;
    background: linear-gradient(to left, rgba(124, 58, 237, 0.25), transparent);
    pointer-events: none;
  }

  .sidebar-brand {
    padding: var(--space-md) var(--space-xl);
    font-size: var(--text-xl);
    font-weight: 700;
    color: var(--text-primary);
    margin-bottom: var(--space-lg);
  }

  .sidebar-nav {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    padding: 0 var(--space-sm);
  }

  .sidebar-nav a {
    display: flex;
    align-items: center;
    gap: var(--space-md);
    padding: var(--space-md) var(--space-lg);
    color: var(--text-muted);
    text-decoration: none;
    font-weight: 600;
    font-size: var(--text-base);
    border-radius: var(--radius-lg) 0 0 var(--radius-lg);
    transition:
      color var(--transition-fast),
      background var(--transition-fast);
  }

  .sidebar-nav a:hover {
    color: var(--accent-light);
    background: transparent;
    border-right: 2px solid rgba(167, 139, 250, 0.3);
  }

  .sidebar-nav a.active {
    color: var(--accent-medium);
    background: transparent;
    border-right: 3px solid var(--accent);
  }

  .sidebar-footer {
    margin-top: auto;
    padding: var(--space-md) var(--space-xl);
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .status-dot.connected {
    background: var(--color-success);
    box-shadow: 0 0 4px var(--glow-success);
  }

  .status-dot.disconnected {
    background: var(--color-error);
    box-shadow: 0 0 4px var(--glow-error);
  }

  .status-text {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  @media (max-width: 767px) {
    .sidebar {
      display: none;
    }
  }
</style>
