<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import QrModal from "$lib/QrModal.svelte";
  import Avatar from "$lib/Avatar.svelte";
  import PostFeed from "$lib/PostFeed.svelte";
  import DeleteConfirmModal from "$lib/DeleteConfirmModal.svelte";
  import ProfileEditor from "$lib/ProfileEditor.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import type {
    Post,
    Profile,
    FollowEntry,
    SyncResult,
    SyncStatus,
  } from "$lib/types";
  import { shortId } from "$lib/utils";
  import {
    useToast,
    useCopyFeedback,
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useDeleteConfirm,
    useLightbox,
  } from "$lib/composables.svelte";

  let pubkey: string = $derived(page.params.pubkey ?? "");
  let profile = $state<Profile | null>(null);
  let posts = $state<Post[]>([]);
  let isFollowing = $state(false);
  let toggling = $state(false);
  let sentinel = $state<HTMLDivElement>(null!);
  let mediaFilter = $state("all");
  let syncStatus = $state<SyncStatus | null>(null);
  let remoteTotal = $state<number | null>(null);
  let fetchingRemote = $state(false);
  let peerOffline = $state(false);
  let isMuted = $state(false);
  let isBlocked = $state(false);
  let togglingMute = $state(false);
  let togglingBlock = $state(false);
  let showQr = $state(false);
  let editingProfile = $state(false);
  let transportNodeIds = $state<string[]>([]);

  const FILTERS = [
    { value: "all", label: "All" },
    { value: "images", label: "Images" },
    { value: "videos", label: "Videos" },
    { value: "audio", label: "Audio" },
    { value: "files", label: "Files" },
    { value: "text", label: "Text" },
  ] as const;

  const blobs = createBlobCache();
  setBlobContext(blobs);

  const toast = useToast();
  const copyFb = useCopyFeedback();
  const lightbox = useLightbox();
  const del = useDeleteConfirm(async (id) => {
    try {
      await invoke("delete_post", { id });
      await reloadPosts();
    } catch (e) {
      toast.show("Failed to delete post");
      console.error("Failed to delete post:", e);
    }
  });

  const node = useNodeInit(async () => {
    if (pubkey === node.pubkey) {
      const myProfile: Profile | null = await invoke("get_my_profile");
      profile = myProfile;
      const myNodeId: string = await invoke("get_node_id");
      transportNodeIds = [myNodeId];
    } else {
      profile = await invoke("get_remote_profile", { pubkey });
      try {
        transportNodeIds = await invoke("get_peer_node_ids", { pubkey });
      } catch {
        transportNodeIds = [];
      }
    }

    const allPosts: Post[] = await invoke("get_user_posts", {
      pubkey,
      limit: 20,
      before: null,
      mediaFilter: mediaFilter === "all" ? null : mediaFilter,
    });
    posts = allPosts;
    scroll.setHasMore(allPosts.length);

    const follows: FollowEntry[] = await invoke("get_follows");
    isFollowing = follows.some((f) => f.pubkey === pubkey);

    if (pubkey !== node.pubkey) {
      isMuted = await invoke("is_muted", { pubkey });
      isBlocked = await invoke("is_blocked", { pubkey });
    }

    if (pubkey !== node.pubkey) {
      try {
        syncStatus = await invoke("get_sync_status", { pubkey });
      } catch {
        // sync status is informational
      }
    }
  });

  let isSelf = $derived(pubkey === node.pubkey);
  let displayName = $derived(
    profile?.display_name || (isSelf ? "You" : shortId(pubkey)),
  );

  async function reloadPosts() {
    try {
      const newPosts: Post[] = await invoke("get_user_posts", {
        pubkey,
        limit: 20,
        before: null,
        mediaFilter: mediaFilter === "all" ? null : mediaFilter,
      });
      posts = newPosts;
      scroll.setHasMore(newPosts.length);
    } catch (e) {
      console.error("Failed to reload posts:", e);
    }
  }

  async function reloadProfile() {
    try {
      if (isSelf) {
        profile = await invoke("get_my_profile");
      } else {
        profile = await invoke("get_remote_profile", { pubkey });
      }
    } catch (e) {
      console.error("Failed to reload profile:", e);
    }
  }

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      try {
        const oldest = posts[posts.length - 1];
        const olderPosts: Post[] = await invoke("get_user_posts", {
          pubkey,
          limit: 20,
          before: oldest.timestamp,
          mediaFilter: mediaFilter === "all" ? null : mediaFilter,
        });
        if (olderPosts.length > 0) {
          posts = [...posts, ...olderPosts];
          scroll.setHasMore(olderPosts.length);
        } else if (!isSelf && !peerOffline && mediaFilter === "all") {
          await fetchFromRemote();
        } else {
          scroll.setNoMore();
        }
      } catch (e) {
        toast.show("Failed to load more posts");
        console.error("Failed to load more:", e);
      }
    },
    20,
  );

  async function fetchFromRemote() {
    fetchingRemote = true;
    try {
      const result: SyncResult = await invoke("sync_posts", {
        pubkey,
      });
      remoteTotal = result.remote_total;
      if (result.posts.length > 0) {
        posts = [...posts, ...result.posts];
        syncStatus = await invoke("get_sync_status", { pubkey });
      }
      scroll.setNoMore();
    } catch {
      peerOffline = true;
      scroll.setNoMore();
    }
    fetchingRemote = false;
  }

  async function toggleFollow() {
    toggling = true;
    try {
      if (isFollowing) {
        if (
          !confirm(
            "Unfollow this user? Their posts will be deleted from your device.",
          )
        ) {
          toggling = false;
          return;
        }
        await invoke("unfollow_user", { pubkey });
        isFollowing = false;
      } else {
        if (transportNodeIds.length === 0) {
          toast.show("No transport NodeId known for this user");
          toggling = false;
          return;
        }
        await invoke("follow_user", { nodeId: transportNodeIds[0] });
        isFollowing = true;
      }
    } catch (e) {
      toast.show(`Failed to ${isFollowing ? "unfollow" : "follow"}`);
      console.error("Toggle follow failed:", e);
    }
    toggling = false;
  }

  async function toggleMute() {
    togglingMute = true;
    try {
      if (isMuted) {
        await invoke("unmute_user", { pubkey });
        isMuted = false;
      } else {
        await invoke("mute_user", { pubkey });
        isMuted = true;
      }
    } catch (e) {
      toast.show("Failed to toggle mute");
      console.error("Toggle mute failed:", e);
    }
    togglingMute = false;
  }

  async function toggleBlock() {
    togglingBlock = true;
    try {
      if (isBlocked) {
        await invoke("unblock_user", { pubkey });
        isBlocked = false;
      } else {
        await invoke("block_user", { pubkey });
        isBlocked = true;
        isFollowing = false;
      }
    } catch (e) {
      toast.show("Failed to toggle block");
      console.error("Toggle block failed:", e);
    }
    togglingBlock = false;
  }

  async function startCall() {
    try {
      await invoke("start_call", { peerPubkey: pubkey });
    } catch (e) {
      toast.show("Failed to start call");
      console.error("Failed to start call:", e);
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (del.pendingId) del.cancel();
      else if (editingProfile) editingProfile = false;
      else if (showQr) showQr = false;
    }
  }

  $effect(() => {
    return scroll.setupEffect();
  });

  let filterInitialized = false;
  $effect(() => {
    mediaFilter; // track dependency
    if (!filterInitialized) {
      filterInitialized = true;
      return;
    }
    posts = [];
    reloadPosts();
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "feed-updated": () => {
        reloadPosts();
      },
      "profile-updated": (payload) => {
        if (payload === pubkey) {
          reloadProfile();
        }
      },
    });
    window.addEventListener("keydown", handleGlobalKey);
    return () => {
      blobs.revokeAll();
      cleanupListeners();
      window.removeEventListener("keydown", handleGlobalKey);
    };
  });
</script>

{#if showQr}
  <QrModal
    {pubkey}
    transportNodeId={transportNodeIds[0]}
    onclose={() => (showQr = false)}
  />
{/if}

{#if lightbox.src}
  <Lightbox src={lightbox.src} alt={lightbox.alt} onclose={lightbox.close} />
{/if}

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Loading profile...</p>
  </div>
{:else}
  {#if !isSelf}
    <a href="/" class="back-link">&larr; Back to feed</a>
  {/if}

  {#if isSelf && editingProfile && profile}
    <ProfileEditor
      {pubkey}
      {profile}
      onsaved={async () => {
        editingProfile = false;
        await reloadProfile();
        toast.show("Profile saved", "success");
      }}
      oncancel={() => (editingProfile = false)}
    />
  {:else}
    <div class="profile-header">
      <Avatar
        {pubkey}
        name={displayName}
        {isSelf}
        ticket={profile?.avatar_ticket}
        size={56}
      />
      <div class="profile-info">
        <h2>{displayName}</h2>
        {#if profile?.visibility && profile.visibility !== "public"}
          <span class="visibility-badge"
            >{profile.visibility === "private" ? "Private" : "Listed"}</span
          >
        {/if}
        {#if profile?.bio}
          <p class="bio">{profile.bio}</p>
        {/if}
      </div>
      {#if isSelf}
        <button
          class="btn-elevated edit-btn"
          onclick={() => (editingProfile = true)}>Edit</button
        >
      {/if}
    </div>
  {/if}

  <div class="id-section">
    <div class="id-row">
      <span class="id-label">Public Key</span>
      <code>{pubkey}</code>
      <button
        class="btn-elevated copy-btn"
        onclick={() => copyFb.copy(pubkey, "pubkey")}
      >
        {copyFb.feedback === "pubkey" ? "Copied!" : "Copy"}
      </button>
      <button class="btn-elevated copy-btn" onclick={() => (showQr = true)}
        >QR</button
      >
    </div>
    {#each transportNodeIds as nid, i}
      <div class="id-row">
        <span class="id-label"
          >Node ID{transportNodeIds.length > 1 ? ` ${i + 1}` : ""}</span
        >
        <code>{nid}</code>
        <button
          class="btn-elevated copy-btn"
          onclick={() => copyFb.copy(nid, `transport-${i}`)}
        >
          {copyFb.feedback === `transport-${i}` ? "Copied!" : "Copy"}
        </button>
      </div>
    {/each}
  </div>

  {#if !isSelf}
    <div class="action-row">
      <button
        class="follow-toggle"
        class:following={isFollowing}
        onclick={toggleFollow}
        disabled={toggling || isBlocked}
      >
        {#if toggling}<span class="btn-spinner"></span>{:else}{isFollowing
            ? "Unfollow"
            : "Follow"}{/if}
      </button>
      <a href="/messages/{pubkey}" class="message-btn">Message</a>
      <button class="call-btn" onclick={startCall} disabled={isBlocked}>
        Call
      </button>
    </div>
    <div class="moderation-row">
      <button
        class="mod-btn mute"
        class:active={isMuted}
        onclick={toggleMute}
        disabled={togglingMute}
      >
        {#if togglingMute}<span class="btn-spinner"></span>{:else}{isMuted
            ? "Unmute"
            : "Mute"}{/if}
      </button>
      <button
        class="mod-btn block"
        class:active={isBlocked}
        onclick={toggleBlock}
        disabled={togglingBlock}
      >
        {#if togglingBlock}<span class="btn-spinner"></span>{:else}{isBlocked
            ? "Unblock"
            : "Block"}{/if}
      </button>
    </div>
  {/if}

  <div class="filter-bar">
    {#each FILTERS as f (f.value)}
      <button
        class="filter-chip"
        class:active={mediaFilter === f.value}
        onclick={() => (mediaFilter = f.value)}
      >
        {f.label}
      </button>
    {/each}
  </div>

  <h3 class="section-title">
    Posts{posts.length > 0
      ? ` (${posts.length}${scroll.hasMore ? "+" : ""})`
      : ""}
    {#if syncStatus && !isSelf}
      <span class="sync-info">
        {syncStatus.local_count}{remoteTotal != null ? ` / ${remoteTotal}` : ""} synced
      </span>
    {/if}
  </h3>

  {#if del.pendingId}
    <DeleteConfirmModal onconfirm={del.execute} oncancel={del.cancel} />
  {/if}

  <PostFeed
    {posts}
    pubkey={node.pubkey}
    showAuthor={false}
    showDelete={isSelf}
    emptyMessage="No posts from this user yet."
    onreload={reloadPosts}
    ondelete={del.confirm}
    onlightbox={lightbox.open}
  />

  {#if scroll.hasMore && posts.length > 0}
    <div bind:this={sentinel} class="sentinel">
      {#if scroll.loadingMore}
        <span class="btn-spinner"></span>
        {#if fetchingRemote}
          Fetching from peer...
        {:else}
          Loading...
        {/if}
      {/if}
    </div>
  {/if}

  {#if peerOffline && !scroll.hasMore && posts.length > 0}
    <p class="offline-notice">End of cached posts -- peer is offline</p>
  {/if}
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

<style>
  .profile-header {
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  .profile-info {
    flex: 1;
    min-width: 0;
  }

  .profile-info h2 {
    margin: 0;
    color: var(--accent-medium);
    font-size: var(--text-xl);
  }

  .bio {
    margin: 0.25rem 0 0;
    color: var(--text-secondary);
    font-size: var(--text-base);
  }

  .visibility-badge {
    display: inline-block;
    font-size: var(--text-sm);
    color: var(--color-warning);
    border: 1px solid var(--color-warning-border);
    border-radius: var(--radius-sm);
    padding: 0.15rem 0.5rem;
    margin-top: 0.25rem;
  }

  .id-section {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-bottom: 1rem;
  }

  .id-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .id-label {
    color: var(--text-secondary);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
    min-width: 5.5rem;
    flex-shrink: 0;
  }

  code {
    background: var(--bg-deep);
    padding: 0.5rem 0.75rem;
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    word-break: break-all;
    color: var(--color-link);
    flex: 1;
    font-family: var(--font-mono);
  }

  .action-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .follow-toggle {
    flex: 1;
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 2.2rem;
  }

  .message-btn {
    background: var(--bg-elevated);
    color: var(--accent-light);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    text-decoration: none;
    text-align: center;
    transition: background var(--transition-normal);
  }

  .message-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .call-btn {
    background: var(--color-success);
    color: white;
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-normal);
  }

  .call-btn:hover:not(:disabled) {
    background: #16a34a;
  }

  .call-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .follow-toggle:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .follow-toggle.following {
    background: transparent;
    color: var(--color-error-light);
    border: 1px solid var(--color-error-light-border);
  }

  .follow-toggle.following:hover:not(:disabled) {
    background: var(--color-error-light-bg);
  }

  .moderation-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .mod-btn {
    flex: 1;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.35rem;
    font-size: var(--text-base);
    font-weight: 500;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 1.8rem;
    transition:
      color var(--transition-fast),
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .mod-btn.mute {
    color: var(--text-secondary);
  }

  .mod-btn.mute:hover:not(:disabled) {
    color: var(--color-warning);
    border-color: var(--color-warning-border);
    background: var(--color-warning-bg-subtle);
  }

  .mod-btn.mute.active {
    color: var(--color-warning);
    border-color: var(--color-warning-border);
  }

  .mod-btn.mute.active:hover:not(:disabled) {
    background: var(--color-warning-bg-subtle);
  }

  .mod-btn.block {
    color: var(--text-secondary);
  }

  .mod-btn.block:hover:not(:disabled) {
    color: var(--color-error);
    border-color: var(--color-error-border);
    background: var(--color-error-bg-subtle);
  }

  .mod-btn.block.active {
    color: var(--color-error);
    border-color: var(--color-error-border);
  }

  .mod-btn.block.active:hover:not(:disabled) {
    background: var(--color-error-bg-subtle);
  }

  .filter-bar {
    margin-bottom: 0.75rem;
  }

  .section-title {
    margin-bottom: 0.75rem;
  }

  .sync-info {
    font-size: var(--text-sm);
    color: var(--text-tertiary);
    font-weight: 400;
    text-transform: none;
    letter-spacing: normal;
    margin-left: 0.5rem;
  }

  .offline-notice {
    text-align: center;
    color: var(--text-tertiary);
    font-size: var(--text-base);
    padding: 0.75rem;
    border-top: 1px solid var(--border);
    margin-top: 0.5rem;
  }

  .edit-btn {
    padding: 0.4rem 0.85rem;
    font-size: var(--text-base);
    font-weight: 500;
    flex-shrink: 0;
  }
</style>
