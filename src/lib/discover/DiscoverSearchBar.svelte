<script lang="ts">
  import Icon from "$lib/Icon.svelte";

  let {
    searchQuery = $bindable(""),
    activeTab,
    searching,
    onsearch,
  }: {
    searchQuery: string;
    activeTab: "users" | "posts" | "trending";
    searching: boolean;
    onsearch: () => void;
  } = $props();

  function handleSearchKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      onsearch();
    }
  }
</script>

<div class="search-bar">
  <Icon name="search" size={18} />
  <input
    class="search-input"
    bind:value={searchQuery}
    placeholder={activeTab === "users"
      ? "Search users..."
      : activeTab === "posts"
        ? "Search posts..."
        : "Search..."}
    onkeydown={handleSearchKey}
  />
  <button
    class="search-btn"
    onclick={onsearch}
    disabled={!searchQuery.trim() || searching}
  >
    {searching ? "..." : "Search"}
  </button>
</div>

<style>
  .search-bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.5rem 0.75rem;
    margin-bottom: 0.75rem;
  }

  .search-bar :global(svg) {
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .search-input {
    flex: 1;
    background: none;
    border: none;
    outline: none;
    color: var(--text-primary);
    font-size: var(--text-base);
    font-family: inherit;
  }

  .search-input::placeholder {
    color: var(--text-muted);
  }

  .search-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.35rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    font-family: inherit;
  }

  .search-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .search-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
