<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { useNodeInit } from "$lib/composables.svelte";
  import type {
    ServerEntry,
    ServerUser,
    ServerSearchPost,
    TrendingHashtag,
    UserSearchResponse,
    PostSearchResponse,
    FollowEntry,
  } from "$lib/types";

  type Tab = "users" | "posts" | "trending";

  let servers = $state<ServerEntry[]>([]);
  let activeServer = $state<ServerEntry | null>(null);
  let activeTab = $state<Tab>("users");
  let searchQuery = $state("");
  let searching = $state(false);
  let nodeId = $state("");

  let users = $state<ServerUser[]>([]);
  let posts = $state<ServerSearchPost[]>([]);
  let trending = $state<TrendingHashtag[]>([]);
  let loadingTrending = $state(false);
  let followedPubkeys = $state<Set<string>>(new Set());
  let togglingFollow = $state<string | null>(null);
  let userNames = $derived(
    new Map(
      users.map((u) => [u.pubkey, u.display_name || shortId(u.pubkey)]),
    ),
  );

  const node = useNodeInit(async () => {
    nodeId = await invoke<string>("get_node_id");
    const follows: FollowEntry[] = await invoke("get_follows");
    followedPubkeys = new Set(follows.map((f) => f.pubkey));
    servers = await invoke("list_servers");
    if (servers.length > 0) {
      activeServer = servers[0];
      await loadTrending();
      await loadUsers();
    }
  });

  async function loadUsers() {
    if (!activeServer) return;
    searching = true;
    try {
      const resp: UserSearchResponse = await invoke("server_list_users", {
        url: activeServer.url,
        limit: 50,
      });
      users = resp.users;
    } catch (e) {
      console.error("Failed to load users:", e);
    }
    searching = false;
  }

  async function loadPosts() {
    if (!activeServer) return;
    // Ensure user names are loaded for display
    if (users.length === 0) await loadUsers();
    searching = true;
    try {
      const resp: { posts: ServerSearchPost[] } = await invoke(
        "server_get_feed",
        { url: activeServer.url, limit: 50 },
      );
      posts = resp.posts;
    } catch (e) {
      console.error("Failed to load feed:", e);
    }
    searching = false;
  }

  async function loadTrending() {
    if (!activeServer) return;
    loadingTrending = true;
    try {
      const resp: { hashtags: TrendingHashtag[] } = await invoke(
        "server_get_trending",
        { url: activeServer.url, limit: 20 },
      );
      trending = resp.hashtags;
    } catch (e) {
      console.error("Failed to load trending:", e);
    }
    loadingTrending = false;
  }

  async function search() {
    if (!activeServer || !searchQuery.trim()) return;
    searching = true;
    try {
      if (activeTab === "users") {
        const resp: UserSearchResponse = await invoke("server_search_users", {
          url: activeServer.url,
          query: searchQuery.trim(),
          limit: 50,
        });
        users = resp.users;
      } else if (activeTab === "posts") {
        const resp: PostSearchResponse = await invoke("server_search_posts", {
          url: activeServer.url,
          query: searchQuery.trim(),
          limit: 50,
        });
        posts = resp.posts;
      }
    } catch (e) {
      console.error("Search failed:", e);
    }
    searching = false;
  }

  async function switchServer(server: ServerEntry) {
    activeServer = server;
    users = [];
    posts = [];
    trending = [];
    searchQuery = "";
    await loadTrending();
    if (activeTab === "users") await loadUsers();
  }

  async function switchTab(tab: Tab) {
    activeTab = tab;
    if (tab === "users" && users.length === 0 && !searchQuery.trim()) {
      await loadUsers();
    } else if (tab === "posts" && posts.length === 0 && !searchQuery.trim()) {
      await loadPosts();
    }
  }

  function handleSearchKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      search();
    }
  }

  async function toggleFollow(pubkey: string) {
    togglingFollow = pubkey;
    try {
      if (followedPubkeys.has(pubkey)) {
        if (
          !confirm(
            "Unfollow this user? Their posts will be deleted from your device.",
          )
        ) {
          togglingFollow = null;
          return;
        }
        await invoke("unfollow_user", { pubkey });
        followedPubkeys = new Set(
          [...followedPubkeys].filter((p) => p !== pubkey),
        );
      } else {
        await invoke("follow_user", { pubkey });
        followedPubkeys = new Set([...followedPubkeys, pubkey]);
      }
    } catch (e) {
      console.error("Follow toggle failed:", e);
    }
    togglingFollow = null;
  }

  function searchTag(tag: string) {
    searchQuery = `#${tag}`;
    activeTab = "posts";
    search();
  }

  function shortId(id: string): string {
    return id.length > 12 ? id.slice(0, 6) + ".." + id.slice(-4) : id;
  }

  function formatTime(ts: number): string {
    return new Date(ts).toLocaleDateString();
  }

  onMount(() => {
    node.init();
  });
</script>

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Loading...</p>
  </div>
{:else if servers.length === 0}
  <div class="empty-state">
    <Icon name="compass" size={48} />
    <h2>No servers added</h2>
    <p>
      Add a discovery server in <a href="/settings">Settings</a> to discover users
      and content.
    </p>
  </div>
{:else}
  {#if servers.length > 1}
    <div class="server-selector">
      {#each servers as server (server.url)}
        <button
          class="server-chip"
          class:active={activeServer?.url === server.url}
          onclick={() => switchServer(server)}
        >
          {server.name || server.url}
        </button>
      {/each}
    </div>
  {/if}

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
      onclick={search}
      disabled={!searchQuery.trim() || searching}
    >
      {searching ? "..." : "Search"}
    </button>
  </div>

  <div class="tabs">
    <button
      class="tab"
      class:active={activeTab === "users"}
      onclick={() => switchTab("users")}
    >
      <Icon name="users" size={16} />
      Users
    </button>
    <button
      class="tab"
      class:active={activeTab === "posts"}
      onclick={() => switchTab("posts")}
    >
      <Icon name="message-circle" size={16} />
      Posts
    </button>
    <button
      class="tab"
      class:active={activeTab === "trending"}
      onclick={() => switchTab("trending")}
    >
      <Icon name="compass" size={16} />
      Trending
    </button>
  </div>

  <div class="results">
    {#if searching}
      <div class="info-loading">
        <div class="spinner small"></div>
      </div>
    {:else if activeTab === "users"}
      {#if users.length > 0}
        <div class="user-list">
          {#each users as user (user.pubkey)}
            <div class="user-card">
              <a href="/profile/{user.pubkey}" class="user-card-link">
                <div class="user-avatar">
                  <Icon name="user" size={24} />
                </div>
                <div class="user-info">
                  <span class="user-name">
                    {user.display_name || shortId(user.pubkey)}
                  </span>
                  <span class="user-pubkey">{shortId(user.pubkey)}</span>
                  {#if user.bio}
                    <span class="user-bio">{user.bio}</span>
                  {/if}
                </div>
                <div class="user-stats">
                  <span class="user-stat">{user.post_count} posts</span>
                </div>
              </a>
              {#if user.pubkey !== nodeId}
                <button
                  class="follow-btn"
                  class:following={followedPubkeys.has(user.pubkey)}
                  onclick={() => toggleFollow(user.pubkey)}
                  disabled={togglingFollow === user.pubkey}
                >
                  {#if togglingFollow === user.pubkey}
                    ...
                  {:else if followedPubkeys.has(user.pubkey)}
                    Following
                  {:else}
                    Follow
                  {/if}
                </button>
              {/if}
            </div>
          {/each}
        </div>
      {:else}
        <p class="empty">No users found.</p>
      {/if}
    {:else if activeTab === "posts"}
      {#if posts.length > 0}
        <div class="post-list">
          {#each posts as post (post.id)}
            <div class="feed-post">
              <div class="post-meta">
                <a href="/profile/{post.author}" class="post-author">
                  {userNames.get(post.author) || shortId(post.author)}
                </a>
                <span class="post-time">{formatTime(post.timestamp)}</span>
              </div>
              <p class="post-content">{post.content}</p>
            </div>
          {/each}
        </div>
      {:else}
        <p class="empty">
          {searchQuery.trim() ? "No posts found." : "Search for posts above."}
        </p>
      {/if}
    {:else if activeTab === "trending"}
      {#if loadingTrending}
        <div class="info-loading">
          <div class="spinner small"></div>
        </div>
      {:else if trending.length > 0}
        <div class="trending-list">
          {#each trending as t, i (t.tag)}
            <button class="trending-item" onclick={() => searchTag(t.tag)}>
              <span class="trending-rank">{i + 1}</span>
              <div class="trending-info">
                <span class="trending-tag">#{t.tag}</span>
                <span class="trending-count">{t.post_count} posts</span>
              </div>
            </button>
          {/each}
        </div>
      {:else}
        <p class="empty">No trending topics yet.</p>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.75rem;
    padding: 3rem 1rem;
    text-align: center;
    color: var(--text-muted);
  }

  .empty-state h2 {
    margin: 0;
    color: var(--text-primary);
  }

  .empty-state p {
    margin: 0;
  }

  .empty-state a {
    color: var(--accent-light);
  }

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

  .tabs {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 0.75rem;
  }

  .tab {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0.6rem 1rem;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-muted);
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    font-family: inherit;
    transition:
      color var(--transition-fast),
      border-color var(--transition-fast);
  }

  .tab.active {
    color: var(--accent-medium);
    border-bottom-color: var(--accent-medium);
  }

  .tab:hover:not(.active) {
    color: var(--text-primary);
  }

  .user-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .user-card {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
    transition: border-color var(--transition-fast);
  }

  .user-card:hover {
    border-color: var(--accent-medium);
  }

  .user-card-link {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    text-decoration: none;
    color: inherit;
    min-width: 0;
  }

  .follow-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.35rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    font-family: inherit;
    flex-shrink: 0;
  }

  .follow-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .follow-btn.following {
    background: transparent;
    color: var(--text-secondary);
    border: 1px solid var(--border);
  }

  .follow-btn.following:hover:not(:disabled) {
    color: var(--color-error, #ef4444);
    border-color: var(--color-error, #ef4444);
  }

  .follow-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .user-avatar {
    width: 40px;
    height: 40px;
    border-radius: 50%;
    background: var(--bg-elevated);
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .user-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 0;
  }

  .user-name {
    font-weight: 600;
    font-size: var(--text-base);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .user-pubkey {
    font-size: var(--text-xs);
    color: var(--text-muted);
    font-family: monospace;
  }

  .user-bio {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .user-stats {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    flex-shrink: 0;
  }

  .user-stat {
    font-size: var(--text-xs);
    color: var(--text-muted);
  }

  .post-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .feed-post {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
  }

  .post-meta {
    display: flex;
    justify-content: space-between;
    margin-bottom: 0.35rem;
    font-size: var(--text-sm);
  }

  .post-author {
    color: var(--accent-light);
    text-decoration: none;
    font-weight: 600;
  }

  .post-author:hover {
    text-decoration: underline;
  }

  .post-time {
    color: var(--text-muted);
  }

  .post-content {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .trending-list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .trending-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
    cursor: pointer;
    font-family: inherit;
    text-align: left;
    width: 100%;
    transition: border-color var(--transition-fast);
  }

  .trending-item:hover {
    border-color: var(--accent-medium);
  }

  .trending-rank {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--bg-elevated);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--text-sm);
    font-weight: 700;
    color: var(--accent-light);
    flex-shrink: 0;
  }

  .trending-info {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }

  .trending-tag {
    font-weight: 600;
    color: var(--accent-light);
  }

  .trending-count {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .info-loading {
    display: flex;
    justify-content: center;
    padding: 2rem 0;
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2rem 0;
  }
</style>
