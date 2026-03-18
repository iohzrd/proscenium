<script lang="ts">
  import Icon from "$lib/Icon.svelte";

  interface Props {
    pubkey: string;
    unreadDmCount: number;
    unreadNotificationCount: number;
    currentPath: string;
  }

  let { pubkey, unreadDmCount, unreadNotificationCount, currentPath }: Props =
    $props();

  let moreOpen = $state(false);

  let moreActive = $derived(
    currentPath === "/follows" ||
      currentPath === "/preferences" ||
      (!!pubkey && currentPath === `/profile/${pubkey}`),
  );

  function toggleMore(e: Event) {
    e.preventDefault();
    moreOpen = !moreOpen;
  }

  function closeMore() {
    moreOpen = false;
  }
</script>

{#if moreOpen}
  <button class="more-backdrop" onclick={closeMore} aria-label="Close menu"
  ></button>
{/if}

<nav class="bottom-nav">
  <a href="/" class:active={currentPath === "/"} onclick={closeMore}>
    <Icon name="home" size={22} />
    <span class="tab-label">Arc</span>
  </a>
  <a
    href="/activity"
    class:active={currentPath === "/activity"}
    onclick={closeMore}
  >
    <span class="tab-icon-wrap">
      <Icon name="bell" size={22} />
      {#if unreadNotificationCount > 0}
        <span class="badge">{unreadNotificationCount}</span>
      {/if}
    </span>
    <span class="tab-label">Activity</span>
  </a>
  <a
    href="/messages"
    class:active={currentPath.startsWith("/messages")}
    onclick={closeMore}
  >
    <span class="tab-icon-wrap">
      <Icon name="message-circle" size={22} />
      {#if unreadDmCount > 0}
        <span class="badge">{unreadDmCount}</span>
      {/if}
    </span>
    <span class="tab-label">Messages</span>
  </a>
  <a
    href="/discover"
    class:active={currentPath === "/discover"}
    onclick={closeMore}
  >
    <Icon name="compass" size={22} />
    <span class="tab-label">Discover</span>
  </a>
  <a href="/stage" class:active={currentPath === "/stage"} onclick={closeMore}>
    <Icon name="radio" size={22} />
    <span class="tab-label">Stage</span>
  </a>
  <button
    class="tab-btn"
    class:active={moreActive}
    onclick={toggleMore}
    aria-label="More"
  >
    <Icon name="more-horizontal" size={22} />
    <span class="tab-label">More</span>
  </button>

  {#if moreOpen}
    <div class="more-menu">
      {#if pubkey}
        <a
          href="/profile/{pubkey}"
          class:active={currentPath === `/profile/${pubkey}`}
          onclick={closeMore}
        >
          <Icon name="user" size={18} />
          <span>Profile</span>
        </a>
      {/if}
      <a
        href="/follows"
        class:active={currentPath === "/follows"}
        onclick={closeMore}
      >
        <Icon name="users" size={18} />
        <span>Follows</span>
      </a>
      <a
        href="/preferences"
        class:active={currentPath === "/preferences"}
        onclick={closeMore}
      >
        <Icon name="settings" size={18} />
        <span>Preferences</span>
      </a>
    </div>
  {/if}
</nav>

<style>
  .bottom-nav {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    height: var(--bottom-nav-height);
    padding-bottom: env(safe-area-inset-bottom);
    background: var(--bg-deep);
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    justify-content: space-around;
    z-index: var(--z-bottom-nav);
  }

  .bottom-nav a {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: var(--space-xs) var(--space-sm);
    color: var(--text-muted);
    text-decoration: none;
    transition: color var(--transition-fast);
    min-width: 48px;
  }

  .bottom-nav a.active {
    color: var(--accent-medium);
  }

  .tab-btn {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: var(--space-xs) var(--space-sm);
    color: var(--text-muted);
    background: none;
    border: none;
    cursor: pointer;
    transition: color var(--transition-fast);
    min-width: 48px;
    font-family: inherit;
  }

  .tab-btn.active {
    color: var(--accent-medium);
  }

  .tab-label {
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .tab-icon-wrap {
    position: relative;
    display: inline-flex;
  }

  .badge {
    position: absolute;
    top: -6px;
    right: -10px;
    background: var(--accent);
    color: var(--text-on-accent);
    font-weight: 700;
    font-size: 0.55rem;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    border-radius: var(--radius-full);
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .more-backdrop {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: calc(var(--z-bottom-nav) - 1);
    cursor: default;
  }

  .more-menu {
    position: absolute;
    bottom: 100%;
    right: var(--space-sm);
    margin-bottom: var(--space-xs);
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-md);
    min-width: 160px;
    overflow: hidden;
  }

  .more-menu a {
    display: flex;
    align-items: center;
    gap: var(--space-md);
    padding: var(--space-md) var(--space-lg);
    color: var(--text-secondary);
    text-decoration: none;
    font-size: var(--text-base);
    font-weight: 500;
    transition:
      background var(--transition-fast),
      color var(--transition-fast);
  }

  .more-menu a:hover {
    background: var(--bg-elevated);
    color: var(--accent-light);
  }

  .more-menu a.active {
    color: var(--accent-medium);
  }

  @media (min-width: 768px) {
    .bottom-nav {
      display: none;
    }

    .more-backdrop {
      display: none;
    }
  }
</style>
