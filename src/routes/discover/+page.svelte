<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Icon from "$lib/Icon.svelte";
  import { useNodeInit } from "$lib/composables";
  import ServerSelector from "$lib/discover/ServerSelector.svelte";
  import DiscoverSearchBar from "$lib/discover/DiscoverSearchBar.svelte";
  import UserResultsList from "$lib/discover/UserResultsList.svelte";
  import PostResultsList from "$lib/discover/PostResultsList.svelte";
  import TrendingList from "$lib/discover/TrendingList.svelte";
  import type {
    ServerEntry,
    ServerUser,
    ServerSearchPost,
    TrendingHashtag,
    UserSearchResponse,
    PostSearchResponse,
    SocialGraphEntry,
  } from "$lib/types";

  type Tab = "users" | "posts" | "trending";

  let servers = $state<ServerEntry[]>([]);
  let activeServer = $state<ServerEntry | null>(null);
  let activeTab = $state<Tab>("users");
  let searchQuery = $state("");
  let searching = $state(false);
  let myPubkey = $state("");

  let users = $state<ServerUser[]>([]);
  let posts = $state<ServerSearchPost[]>([]);
  let trending = $state<TrendingHashtag[]>([]);
  let loadingTrending = $state(false);
  let followedPubkeys = $state<Set<string>>(new Set());
  let togglingFollow = $state<string | null>(null);
  let userNames = $derived(
    new Map(users.map((u) => [u.pubkey, u.display_name || shortId(u.pubkey)])),
  );

  const node = useNodeInit(async () => {
    myPubkey = await invoke<string>("get_pubkey");
    const follows: SocialGraphEntry[] = await invoke("get_follows");
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
        const user = users.find((u) => u.pubkey === pubkey);
        const nodeId = user?.transport_node_id;
        if (!nodeId) {
          console.error("No transport node ID for user:", pubkey);
          togglingFollow = null;
          return;
        }
        await invoke("follow_user", { nodeId });
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
        {myPubkey}
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
