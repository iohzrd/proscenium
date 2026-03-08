<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { useNodeInit } from "$lib/composables.svelte";
  import type {
    ServerEntry,
    ServerInfo,
    ServerFeedPost,
    TrendingHashtag,
  } from "$lib/types";

  let servers = $state<ServerEntry[]>([]);
  let newServerUrl = $state("");
  let status = $state("");
  let adding = $state(false);
  let selectedServer = $state<ServerEntry | null>(null);
  let serverInfo = $state<ServerInfo | null>(null);
  let serverFeed = $state<ServerFeedPost[]>([]);
  let trending = $state<TrendingHashtag[]>([]);
  let loadingInfo = $state(false);
  let loadingFeed = $state(false);
  let registeringServer = $state<string | null>(null);
  let pendingRemoveUrl = $state<string | null>(null);

  const node = useNodeInit(async () => {
    await loadServers();
  });

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
    status = "";
    try {
      const entry: ServerEntry = await invoke("add_server", { url });
      newServerUrl = "";
      await loadServers();
      status = `Added ${entry.name || entry.url}`;
      setTimeout(() => (status = ""), 3000);
    } catch (e) {
      status = `Error: ${e}`;
    }
    adding = false;
  }

  async function removeServer() {
    if (!pendingRemoveUrl) return;
    try {
      await invoke("remove_server", { url: pendingRemoveUrl });
      if (selectedServer?.url === pendingRemoveUrl) {
        selectedServer = null;
        serverInfo = null;
        serverFeed = [];
        trending = [];
      }
      await loadServers();
    } catch (e) {
      status = `Error: ${e}`;
    }
    pendingRemoveUrl = null;
  }

  async function selectServer(server: ServerEntry) {
    selectedServer = server;
    serverInfo = null;
    serverFeed = [];
    trending = [];
    loadingInfo = true;
    loadingFeed = true;

    try {
      serverInfo = await invoke("refresh_server_info", { url: server.url });
    } catch (e) {
      console.error("Failed to fetch server info:", e);
    }
    loadingInfo = false;

    try {
      const resp: { posts: ServerFeedPost[] } = await invoke(
        "server_get_feed",
        { url: server.url, limit: 20 },
      );
      serverFeed = resp.posts;
    } catch (e) {
      console.error("Failed to fetch server feed:", e);
    }

    try {
      const resp: { hashtags: TrendingHashtag[] } = await invoke(
        "server_get_trending",
        { url: server.url, limit: 10 },
      );
      trending = resp.hashtags;
    } catch (e) {
      console.error("Failed to fetch trending:", e);
    }
    loadingFeed = false;
  }

  async function registerWithServer(url: string, visibility: string) {
    registeringServer = url;
    try {
      await invoke("register_with_server", { url, visibility });
      await loadServers();
      if (selectedServer?.url === url) {
        selectedServer = servers.find((s) => s.url === url) ?? selectedServer;
      }
      status = "Registered!";
      setTimeout(() => (status = ""), 3000);
    } catch (e) {
      status = `Error: ${e}`;
    }
    registeringServer = null;
  }

  async function unregisterFromServer(url: string) {
    registeringServer = url;
    try {
      await invoke("unregister_from_server", { url });
      await loadServers();
      if (selectedServer?.url === url) {
        selectedServer = servers.find((s) => s.url === url) ?? selectedServer;
      }
      status = "Unregistered";
      setTimeout(() => (status = ""), 3000);
    } catch (e) {
      status = `Error: ${e}`;
    }
    registeringServer = null;
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addServer();
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (pendingRemoveUrl) pendingRemoveUrl = null;
      else if (selectedServer) selectedServer = null;
    }
  }

  function shortId(id: string): string {
    return id.length > 12 ? id.slice(0, 6) + ".." + id.slice(-4) : id;
  }

  function formatTime(ts: number): string {
    return new Date(ts).toLocaleDateString();
  }

  onMount(() => {
    node.init();
    window.addEventListener("keydown", handleGlobalKey);
    return () => {
      window.removeEventListener("keydown", handleGlobalKey);
    };
  });
</script>

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Loading...</p>
  </div>
{:else if selectedServer}
  <div class="server-detail">
    <button class="back-btn" onclick={() => (selectedServer = null)}>
      Back
    </button>

    <h2 class="server-name">
      {selectedServer.name || selectedServer.url}
    </h2>
    {#if selectedServer.description}
      <p class="server-desc">{selectedServer.description}</p>
    {/if}

    {#if loadingInfo}
      <div class="info-loading">
        <div class="spinner small"></div>
      </div>
    {:else if serverInfo}
      <div class="server-stats">
        <div class="stat">
          <span class="stat-value">{serverInfo.registered_users}</span>
          <span class="stat-label">Users</span>
        </div>
        <div class="stat">
          <span class="stat-value">{serverInfo.total_posts}</span>
          <span class="stat-label">Posts</span>
        </div>
        <div class="stat">
          <span class="stat-value">v{serverInfo.version}</span>
          <span class="stat-label">Version</span>
        </div>
      </div>

      <div class="registration-section">
        {#if selectedServer.registered_at}
          <p class="registered-status">
            Registered ({selectedServer.visibility})
          </p>
          <button
            class="btn-danger"
            onclick={() => unregisterFromServer(selectedServer!.url)}
            disabled={registeringServer === selectedServer.url}
          >
            {registeringServer === selectedServer.url ? "..." : "Unregister"}
          </button>
        {:else if serverInfo.registration_open}
          <div class="register-actions">
            <button
              class="btn-primary"
              onclick={() => registerWithServer(selectedServer!.url, "public")}
              disabled={registeringServer === selectedServer.url}
            >
              {registeringServer === selectedServer.url ? "..." : "Public"}
            </button>
            <button
              class="btn-secondary"
              onclick={() => registerWithServer(selectedServer!.url, "listed")}
              disabled={registeringServer === selectedServer.url}
            >
              Listed
            </button>
            <button
              class="btn-secondary"
              onclick={() => registerWithServer(selectedServer!.url, "private")}
              disabled={registeringServer === selectedServer.url}
            >
              Private
            </button>
          </div>
          <p class="visibility-hint">
            Public: profile and posts visible to all. Listed: profile visible,
            posts only to followers. Private: invisible on server.
          </p>
        {:else}
          <p class="registration-closed">Registration closed</p>
        {/if}
      </div>
    {/if}

    {#if trending.length > 0}
      <div class="trending-section">
        <h3>Trending</h3>
        <div class="trending-tags">
          {#each trending as t (t.tag)}
            <span class="trending-tag">#{t.tag} ({t.post_count})</span>
          {/each}
        </div>
      </div>
    {/if}

    {#if loadingFeed}
      <div class="info-loading">
        <div class="spinner small"></div>
      </div>
    {:else if serverFeed.length > 0}
      <h3>Recent Posts</h3>
      <div class="server-feed">
        {#each serverFeed as post (post.id)}
          <div class="feed-post">
            <div class="post-meta">
              <a href="/profile/{post.author}" class="post-author">
                {shortId(post.author)}
              </a>
              <span class="post-time">{formatTime(post.timestamp)}</span>
            </div>
            <p class="post-content">{post.content}</p>
          </div>
        {/each}
      </div>
    {:else}
      <p class="empty">No posts on this server yet.</p>
    {/if}
  </div>
{:else}
  <div class="add-server">
    <input
      class="input-base"
      bind:value={newServerUrl}
      placeholder="Server URL (e.g. https://community.example.com)"
      onkeydown={handleKey}
    />
    <button
      class="add-btn"
      onclick={addServer}
      disabled={!newServerUrl.trim() || adding}
    >
      {#if adding}
        <span class="btn-spinner"></span>
      {:else}
        Add
      {/if}
    </button>
  </div>

  {#if status}
    <p class="status">{status}</p>
  {/if}

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

  <div class="server-list">
    {#each servers as server (server.url)}
      <div class="server-card">
        <button class="server-card-main" onclick={() => selectServer(server)}>
          <Icon name="server" size={24} />
          <div class="server-card-info">
            <span class="server-card-name">
              {server.name || server.url}
            </span>
            {#if server.name}
              <span class="server-card-url">{server.url}</span>
            {/if}
            {#if server.registered_at}
              <span class="server-card-badge registered"
                >Registered ({server.visibility})</span
              >
            {/if}
          </div>
        </button>
        <button
          class="btn-moderation danger"
          onclick={() => (pendingRemoveUrl = server.url)}
        >
          Remove
        </button>
      </div>
    {:else}
      <p class="empty">
        No servers added yet. Add a community server URL above to get started.
      </p>
    {/each}
  </div>
{/if}

<style>
  .add-server {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .add-server input {
    flex: 1;
  }

  .add-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.6rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    min-width: 60px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .add-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .status {
    text-align: center;
    color: var(--text-secondary);
    font-size: var(--text-base);
    margin: 0.5rem 0;
  }

  .server-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .server-card {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
  }

  .server-card-main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: none;
    border: none;
    cursor: pointer;
    color: inherit;
    text-align: left;
    font-family: inherit;
    padding: 0;
  }

  .server-card-main:hover {
    color: var(--accent-light);
  }

  .server-card-info {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }

  .server-card-name {
    font-weight: 600;
    font-size: var(--text-base);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .server-card-url {
    font-size: var(--text-sm);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .server-card-badge {
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
    width: fit-content;
  }

  .server-card-badge.registered {
    background: var(--color-success, #22c55e);
    color: #fff;
  }

  /* Detail view */

  .server-detail {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .back-btn {
    align-self: flex-start;
    background: var(--bg-elevated);
    color: var(--text-secondary);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.4rem 0.8rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
  }

  .back-btn:hover {
    color: var(--accent-light);
    background: var(--bg-elevated-hover, var(--bg-surface));
  }

  .server-name {
    font-size: var(--text-xl);
    font-weight: 700;
    margin: 0;
  }

  .server-desc {
    color: var(--text-secondary);
    margin: 0;
  }

  .server-stats {
    display: flex;
    gap: 1.5rem;
  }

  .stat {
    display: flex;
    flex-direction: column;
    align-items: center;
  }

  .stat-value {
    font-size: var(--text-lg);
    font-weight: 700;
    color: var(--accent-light);
  }

  .stat-label {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .registration-section {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
  }

  .registered-status {
    font-weight: 600;
    color: var(--color-success, #22c55e);
    margin: 0;
  }

  .registration-closed {
    color: var(--text-muted);
    font-style: italic;
    margin: 0;
  }

  .register-actions {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .btn-primary {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
  }

  .btn-primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .btn-secondary {
    background: var(--bg-elevated);
    color: var(--text-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
  }

  .btn-secondary:hover:not(:disabled) {
    color: var(--accent-light);
  }

  .visibility-hint {
    font-size: var(--text-xs);
    color: var(--text-muted);
    margin: 0;
    width: 100%;
  }

  .btn-danger {
    background: var(--color-error, #ef4444);
    color: #fff;
    border: none;
    border-radius: var(--radius-md);
    padding: 0.4rem 0.8rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
  }

  .btn-danger:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  .trending-section h3 {
    margin: 0 0 0.5rem;
    font-size: var(--text-base);
    font-weight: 600;
  }

  .trending-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .trending-tag {
    background: var(--bg-elevated);
    color: var(--accent-light);
    padding: 0.25rem 0.6rem;
    border-radius: var(--radius-full);
    font-size: var(--text-sm);
    font-weight: 500;
  }

  .server-feed {
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

  .info-loading {
    display: flex;
    justify-content: center;
    padding: 1rem 0;
  }

  h3 {
    margin: 0;
    font-size: var(--text-base);
    font-weight: 600;
  }
</style>
