<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Icon from "#lib/Icon.svelte";
  import { useNodeInit, useToast } from "#lib/composables/index.ts";
  import ServerSelector from "#lib/discover/ServerSelector.svelte";
  import DiscoverSearchBar from "#lib/discover/DiscoverSearchBar.svelte";
  import UserResultsList from "#lib/discover/UserResultsList.svelte";
  import PostResultsList from "#lib/discover/PostResultsList.svelte";
  import TrendingList from "#lib/discover/TrendingList.svelte";
  import type {
    ServerEntry,
    ServerUser,
    ServerSearchPost,
    TrendingHashtag,
    UserSearchResponse,
    PostSearchResponse,
    SocialGraphEntry,
    FollowOutcome,
  } from "#lib/types.ts";
  import {
    dedupeBy,
    dedupeById,
    followRequestNotice,
    isPubkeyFormat,
    shortId,
  } from "#lib/utils.ts";

  type Tab = "users" | "posts" | "trending";

  let servers = $state<ServerEntry[]>([]);
  let activeServer = $state<ServerEntry | null>(null);
  let activeTab = $state<Tab>("users");
  let searchQuery = $state("");
  let searching = $state(false);

  let users = $state<ServerUser[]>([]);
  let posts = $state<ServerSearchPost[]>([]);
  let trending = $state<TrendingHashtag[]>([]);
  let loadingTrending = $state(false);
  let followedPubkeys = $state<Set<string>>(new Set());
  let togglingFollow = $state<string | null>(null);
  let userNames = $derived(
    new Map(users.map((u) => [u.pubkey, u.display_name || shortId(u.pubkey)])),
  );

  // Incremented when another server is selected. A response is applied only
  // if the value it started with is still current.
  let serverGeneration = 0;
  // Number of user and post requests in progress.
  let activeSearches = 0;

  const toast = useToast();

  const node = useNodeInit(async () => {
    const follows: SocialGraphEntry[] = await invoke("get_follows");
    followedPubkeys = new Set(follows.map((f) => f.pubkey));
    servers = await invoke("list_servers");
    if (servers.length > 0) {
      activeServer = servers[0];
      await loadTrending();
      await loadUsers();
    }
  });

  /**
   * Run `request` against the active server and pass its result to `apply`
   * unless another server was selected meanwhile. A failure is shown as a toast.
   */
  async function withServer<T>(
    failure: string,
    request: (url: string) => Promise<T>,
    apply: (result: T) => void,
  ) {
    const server = activeServer;
    if (!server) return;
    const generation = serverGeneration;
    try {
      const result = await request(server.url);
      if (generation === serverGeneration) apply(result);
    } catch (e) {
      if (generation === serverGeneration) toast.show(`${failure}: ${e}`);
      console.error(`${failure}:`, e);
    }
  }

  // Number of the latest request per result list. A response to an older
  // request is not applied, so a slow earlier search cannot replace a later one.
  const latestRequest = { users: 0, posts: 0 };

  async function searchRequest<T>(
    list: keyof typeof latestRequest,
    failure: string,
    request: (url: string) => Promise<T>,
    apply: (result: T) => void,
  ) {
    const id = ++latestRequest[list];
    activeSearches += 1;
    searching = true;
    try {
      await withServer(failure, request, (result) => {
        if (id === latestRequest[list]) apply(result);
      });
    } finally {
      activeSearches -= 1;
      searching = activeSearches > 0;
    }
  }

  function setUsers(list: ServerUser[]) {
    users = dedupeBy(list, (u) => u.pubkey);
  }

  function loadUsers() {
    return searchRequest(
      "users",
      "Failed to load users",
      (url) =>
        invoke<UserSearchResponse>("server_list_users", { url, limit: 50 }),
      (resp) => setUsers(resp.users),
    );
  }

  async function loadPosts() {
    if (!activeServer) return;
    if (users.length === 0) await loadUsers();
    await searchRequest(
      "posts",
      "Failed to load posts",
      (url) =>
        invoke<{ posts: ServerSearchPost[] }>("server_get_feed", {
          url,
          limit: 50,
        }),
      (resp) => (posts = dedupeById(resp.posts)),
    );
  }

  async function loadTrending() {
    const generation = serverGeneration;
    loadingTrending = true;
    await withServer(
      "Failed to load trending tags",
      (url) =>
        invoke<{ hashtags: TrendingHashtag[] }>("server_get_trending", {
          url,
          limit: 20,
        }),
      (resp) => (trending = dedupeBy(resp.hashtags, (t) => t.tag)),
    );
    if (generation === serverGeneration) loadingTrending = false;
  }

  async function search() {
    const query = searchQuery.trim();
    if (!activeServer || !query) return;
    if (activeTab === "users") {
      await searchRequest(
        "users",
        "Search failed",
        (url) =>
          invoke<UserSearchResponse>("server_search_users", {
            url,
            query,
            limit: 50,
          }),
        (resp) => setUsers(resp.users),
      );
    } else if (activeTab === "posts") {
      await searchRequest(
        "posts",
        "Search failed",
        (url) =>
          invoke<PostSearchResponse>("server_search_posts", {
            url,
            query,
            limit: 50,
          }),
        (resp) => (posts = dedupeById(resp.posts)),
      );
    }
  }

  async function switchServer(server: ServerEntry) {
    serverGeneration += 1;
    activeServer = server;
    users = [];
    posts = [];
    trending = [];
    searchQuery = "";
    await loadTrending();
    if (activeTab === "users") await loadUsers();
    else if (activeTab === "posts") await loadPosts();
  }

  async function switchTab(tab: Tab) {
    activeTab = tab;
    if (tab === "users" && users.length === 0 && !searchQuery.trim()) {
      await loadUsers();
    } else if (tab === "posts" && posts.length === 0 && !searchQuery.trim()) {
      await loadPosts();
    }
  }

  async function unfollow(pubkey: string) {
    if (
      !confirm(
        "Unfollow this user? Their posts will be deleted from your device.",
      )
    ) {
      return;
    }
    await invoke("unfollow_user", { pubkey });
    followedPubkeys = new Set([...followedPubkeys].filter((p) => p !== pubkey));
  }

  async function follow(pubkey: string) {
    const nodeId = users.find((u) => u.pubkey === pubkey)?.transport_node_id;
    if (!nodeId || !isPubkeyFormat(nodeId)) {
      toast.show("The server lists no Node ID for this user");
      return;
    }
    // `follow_user` fails if the node reports a master pubkey other than `pubkey`.
    const outcome: FollowOutcome = await invoke("follow_user", {
      nodeId,
      expectedPubkey: pubkey,
    });
    if (outcome !== "followed") {
      toast.show(...followRequestNotice(outcome));
      return;
    }
    followedPubkeys = new Set([...followedPubkeys, pubkey]);
  }

  async function toggleFollow(pubkey: string) {
    if (togglingFollow) return;
    togglingFollow = pubkey;
    try {
      await (followedPubkeys.has(pubkey) ? unfollow(pubkey) : follow(pubkey));
    } catch (e) {
      toast.show(`Failed to update follow: ${e}`);
      console.error("Follow toggle failed:", e);
    } finally {
      togglingFollow = null;
    }
  }

  function searchTag(tag: string) {
    searchQuery = `#${tag}`;
    activeTab = "posts";
    search();
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
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
  </div>
{:else if servers.length === 0}
  <div class="empty-state">
    <Icon name="compass" size={48} />
    <h2>No servers added</h2>
    <p>
      Add a discovery server in <a href="/preferences">Preferences</a> to discover
      users and content.
    </p>
  </div>
{:else}
  {#if servers.length > 1}
    <ServerSelector {servers} {activeServer} onselect={switchServer} />
  {/if}

  <DiscoverSearchBar
    bind:searchQuery
    {activeTab}
    {searching}
    onsearch={search}
  />

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
      <UserResultsList
        {users}
        myPubkey={node.pubkey}
        {followedPubkeys}
        {togglingFollow}
        ontogglefollow={toggleFollow}
      />
    {:else if activeTab === "posts"}
      <PostResultsList {posts} {searchQuery} {userNames} />
    {:else if activeTab === "trending"}
      <TrendingList
        {trending}
        loading={loadingTrending}
        onsearchtag={searchTag}
      />
    {/if}
  </div>
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
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

  .info-loading {
    display: flex;
    justify-content: center;
    padding: 2rem 0;
  }
</style>
