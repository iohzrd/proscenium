<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import DeleteConfirmModal from "$lib/DeleteConfirmModal.svelte";
  import Icon from "$lib/Icon.svelte";
  import { useNodeInit, useToast } from "$lib/composables";
  import type {
    ServerEntry,
    ServerInfo,
    ServerFeedPost,
    TrendingHashtag,
  } from "$lib/types";
  import { dedupeBy, dedupeById, shortId } from "$lib/utils";

  let servers = $state<ServerEntry[]>([]);
  let newServerUrl = $state("");
  let adding = $state(false);
  let selectedServer = $state<ServerEntry | null>(null);
  let serverInfo = $state<ServerInfo | null>(null);
  let serverFeed = $state<ServerFeedPost[]>([]);
  let trending = $state<TrendingHashtag[]>([]);
  let loadingInfo = $state(false);
  let loadingFeed = $state(false);
  let registeringServer = $state<string | null>(null);
  let pendingRemoveUrl = $state<string | null>(null);
  // Incremented when the detail view changes server or closes. A response is
  // applied only if the value it started with is still current.
  let selection = 0;

  const toast = useToast();

  const node = useNodeInit(async () => {
    await loadServers();
  });

  /** True if requests to `url` are sent without TLS. */
  function isUnencrypted(url: string): boolean {
    return url.trim().toLowerCase().startsWith("http://");
  }

  async function loadServers() {
    try {
      const list: ServerEntry[] = await invoke("list_servers");
      servers = dedupeBy(list, (server) => server.url);
    } catch (e) {
      toast.show("Failed to load servers");
      console.error("Failed to load servers:", e);
    }
  }

  async function addServer() {
    const url = newServerUrl.trim();
    if (!url || adding) return;
    let parsed: URL | null = null;
    try {
      parsed = new URL(url);
    } catch {
      // handled below
    }
    if (
      !parsed ||
      (parsed.protocol !== "http:" && parsed.protocol !== "https:")
    ) {
      toast.show("Enter a server URL that starts with https:// or http://");
      return;
    }
    adding = true;
    try {
      const entry: ServerEntry = await invoke("add_server", { url });
      newServerUrl = "";
      await loadServers();
      toast.show(`Added ${entry.name || entry.url}`, "success");
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
    adding = false;
  }

  function closeDetail() {
    selection += 1;
    selectedServer = null;
    serverInfo = null;
    serverFeed = [];
    trending = [];
    loadingInfo = false;
    loadingFeed = false;
  }

  async function removeServer() {
    if (!pendingRemoveUrl) return;
    const url = pendingRemoveUrl;
    pendingRemoveUrl = null;
    try {
      await invoke("remove_server", { url });
      if (selectedServer?.url === url) closeDetail();
      await loadServers();
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
  }

  async function selectServer(server: ServerEntry) {
    const current = ++selection;
    selectedServer = server;
    serverInfo = null;
    serverFeed = [];
    trending = [];
    loadingInfo = true;
    loadingFeed = true;

    try {
      const info: ServerInfo = await invoke("refresh_server_info", {
        url: server.url,
      });
      if (current !== selection) return;
      serverInfo = info;
    } catch (e) {
      if (current !== selection) return;
      toast.show(`Failed to fetch server info: ${e}`);
      console.error("Failed to fetch server info:", e);
    }
    loadingInfo = false;

    try {
      const resp: { posts: ServerFeedPost[] } = await invoke(
        "server_get_feed",
        { url: server.url, limit: 20 },
      );
      if (current !== selection) return;
      serverFeed = dedupeById(resp.posts);
    } catch (e) {
      if (current !== selection) return;
      toast.show(`Failed to fetch server feed: ${e}`);
      console.error("Failed to fetch server feed:", e);
    }

    try {
      const resp: { hashtags: TrendingHashtag[] } = await invoke(
        "server_get_trending",
        { url: server.url, limit: 10 },
      );
      if (current !== selection) return;
      trending = dedupeBy(resp.hashtags, (t) => t.tag);
    } catch (e) {
      if (current !== selection) return;
      console.error("Failed to fetch trending:", e);
    }
    loadingFeed = false;
  }

  /** Invoke a registration command for `url`, then reload the server list. */
  async function changeRegistration(
    url: string,
    command: "register_with_server" | "unregister_from_server",
    args: Record<string, unknown>,
    success: string,
  ) {
    if (registeringServer) return;
    registeringServer = url;
    try {
      await invoke(command, { url, ...args });
      await loadServers();
      if (selectedServer?.url === url) {
        selectedServer = servers.find((s) => s.url === url) ?? selectedServer;
      }
      toast.show(success, "success");
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
    registeringServer = null;
  }

  function registerWithServer(url: string, visibility: string) {
    return changeRegistration(
      url,
      "register_with_server",
      { visibility },
      "Registered",
    );
  }

  function unregisterFromServer(url: string) {
    return changeRegistration(
      url,
      "unregister_from_server",
      {},
      "Unregistered",
    );
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addServer();
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    // The remove confirmation handles Escape itself.
    if (e.key === "Escape" && !pendingRemoveUrl && selectedServer) {
      closeDetail();
    }
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
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
  </div>
{:else if selectedServer}
  <div class="server-detail">
    <button class="back-btn" onclick={closeDetail}>Back</button>

    <h2 class="server-name">
      {selectedServer.name || selectedServer.url}
    </h2>
    {#if selectedServer.description}
      <p class="server-desc">{selectedServer.description}</p>
    {/if}
    {#if isUnencrypted(selectedServer.url)}
      <p class="unencrypted-notice">
        Unencrypted connection (http): requests to this server and its responses
        can be read and changed in transit.
      </p>
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
        <div class="stat">
          <span class="stat-value">
            {serverInfo.retention_days > 0
              ? `${serverInfo.retention_days}d`
              : "Forever"}
          </span>
          <span class="stat-label">Retention</span>
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
              <a
                href="/profile/{encodeURIComponent(post.author)}"
                class="post-author"
              >
                {shortId(post.author)}
              </a>
              {#if !post.verified}
                <span
                  class="unverified-badge"
                  title="The signature of this post was not verified on this device"
                  >Unverified</span
                >
              {/if}
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
      aria-label="Server URL"
      autocomplete="off"
      autocapitalize="none"
      spellcheck="false"
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
  {#if isUnencrypted(newServerUrl)}
    <p class="unencrypted-notice">
      This URL uses http: requests to the server and its responses are not
      encrypted.
    </p>
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
            {#if isUnencrypted(server.url)}
              <span class="server-card-badge unencrypted">Unencrypted</span>
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
        No servers added yet. Add a discovery server URL above to get started.
      </p>
    {/each}
  </div>
{/if}

{#if pendingRemoveUrl}
  <DeleteConfirmModal
    message="Remove this server? This will not unregister you from it."
    confirmLabel="Remove"
    onconfirm={removeServer}
    oncancel={() => (pendingRemoveUrl = null)}
  />
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
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

  .unencrypted-notice {
    color: var(--color-warning);
    font-size: var(--text-sm);
    margin: 0 0 0.75rem;
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
    background: var(--color-success);
    color: var(--text-on-accent);
  }

  .server-card-badge.unencrypted {
    background: var(--color-warning-bg);
    color: var(--color-warning);
    border: 1px solid var(--color-warning-border);
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
    background: var(--color-error);
    color: var(--text-on-accent);
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

  .unverified-badge {
    color: var(--color-warning);
    font-size: var(--text-xs);
    font-weight: 600;
    margin: 0 auto 0 0.5rem;
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
