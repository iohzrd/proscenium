<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import QrModal from "$lib/QrModal.svelte";
  import Avatar from "$lib/Avatar.svelte";
  import PostCard from "$lib/PostCard.svelte";
  import ReplyComposer from "$lib/ReplyComposer.svelte";
  import QuoteComposer from "$lib/QuoteComposer.svelte";
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
  import { shortId, copyToClipboard, setupInfiniteScroll } from "$lib/utils";

  let pubkey: string = $derived(page.params.pubkey ?? "");
  let nodeId = $state("");
  let profile = $state<Profile | null>(null);
  let posts = $state<Post[]>([]);
  let loading = $state(true);
  let isFollowing = $state(false);
  let toggling = $state(false);
  let copyFeedback = $state(false);
  let hasMore = $state(true);
  let loadingMore = $state(false);
  let replyingTo = $state<Post | null>(null);
  let quotingPost = $state<Post | null>(null);
  let lightboxSrc = $state("");
  let lightboxAlt = $state("");
  let toastMessage = $state("");
  let toastType = $state<"error" | "success">("error");
  let sentinel = $state<HTMLDivElement>(null!);
  let mediaFilter = $state("all");
  let syncStatus = $state<SyncStatus | null>(null);
  let remoteTotal = $state<number | null>(null);
  let fetchingRemote = $state(false);
  let peerOffline = $state(false);
  let pendingDeleteId = $state<string | null>(null);
  let isMuted = $state(false);
  let isBlocked = $state(false);
  let togglingMute = $state(false);
  let togglingBlock = $state(false);
  let showQr = $state(false);
  let editingProfile = $state(false);

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

  function showToast(message: string, type: "error" | "success" = "error") {
    toastMessage = message;
    toastType = type;
    setTimeout(() => (toastMessage = ""), 4000);
  }

  let isSelf = $derived(pubkey === nodeId);
  let displayName = $derived(
    profile?.display_name || (isSelf ? "You" : shortId(pubkey)),
  );

  async function init() {
    try {
      nodeId = await invoke("get_node_id");
      if (isSelf) {
        const myProfile: Profile | null = await invoke("get_my_profile");
        profile = myProfile;
      } else {
        profile = await invoke("get_remote_profile", { pubkey });
      }

      const allPosts: Post[] = await invoke("get_user_posts", {
        pubkey,
        limit: 20,
        before: null,
        mediaFilter: mediaFilter === "all" ? null : mediaFilter,
      });
      posts = allPosts;
      hasMore = allPosts.length >= 20;

      const follows: FollowEntry[] = await invoke("get_follows");
      isFollowing = follows.some((f) => f.pubkey === pubkey);

      if (!isSelf) {
        isMuted = await invoke("is_muted", { pubkey });
        isBlocked = await invoke("is_blocked", { pubkey });
      }

      if (!isSelf) {
        try {
          syncStatus = await invoke("get_sync_status", { pubkey });
        } catch {
          // sync status is informational
        }
      }

      loading = false;
    } catch {
      setTimeout(init, 500);
    }
  }

  async function reloadPosts() {
    try {
      const newPosts: Post[] = await invoke("get_user_posts", {
        pubkey,
        limit: 20,
        before: null,
        mediaFilter: mediaFilter === "all" ? null : mediaFilter,
      });
      posts = newPosts;
      hasMore = newPosts.length >= 20;
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

  async function loadMore() {
    if (loadingMore || !hasMore || posts.length === 0) return;
    loadingMore = true;
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
        hasMore = olderPosts.length >= 20;
      } else if (!isSelf && !peerOffline && mediaFilter === "all") {
        await fetchFromRemote();
      } else {
        hasMore = false;
      }
    } catch (e) {
      showToast("Failed to load more posts");
      console.error("Failed to load more:", e);
    }
    loadingMore = false;
  }

  async function fetchFromRemote() {
    fetchingRemote = true;
    try {
      const result: SyncResult = await invoke("fetch_older_posts", {
        pubkey,
      });
      remoteTotal = result.remote_total;
      if (result.posts.length > 0) {
        posts = [...posts, ...result.posts];
        syncStatus = await invoke("get_sync_status", { pubkey });
      }
      hasMore = false;
    } catch {
      peerOffline = true;
      hasMore = false;
    }
    fetchingRemote = false;
  }

  async function toggleFollow() {
    toggling = true;
    try {
      if (isFollowing) {
        await invoke("unfollow_user", { pubkey });
        isFollowing = false;
      } else {
        await invoke("follow_user", { pubkey });
        isFollowing = true;
      }
    } catch (e) {
      showToast(`Failed to ${isFollowing ? "unfollow" : "follow"}`);
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
      showToast("Failed to toggle mute");
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
      showToast("Failed to toggle block");
      console.error("Toggle block failed:", e);
    }
    togglingBlock = false;
  }

  async function copyNodeId() {
    await copyToClipboard(pubkey);
    copyFeedback = true;
    setTimeout(() => (copyFeedback = false), 1500);
  }

  function confirmDelete(id: string) {
    pendingDeleteId = id;
  }

  async function executeDelete() {
    if (!pendingDeleteId) return;
    try {
      await invoke("delete_post", { id: pendingDeleteId });
      await reloadPosts();
    } catch (e) {
      showToast("Failed to delete post");
      console.error("Failed to delete post:", e);
    }
    pendingDeleteId = null;
  }

  function cancelDelete() {
    pendingDeleteId = null;
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (pendingDeleteId) cancelDelete();
      else if (editingProfile) editingProfile = false;
      else if (showQr) showQr = false;
    }
  }

  $effect(() => {
    return setupInfiniteScroll(
      sentinel,
      () => hasMore,
      () => loadingMore,
      loadMore,
    );
  });

  let filterInitialized = false;
  $effect(() => {
    mediaFilter; // track dependency
    if (!filterInitialized) {
      filterInitialized = true;
      return;
    }
    posts = [];
    hasMore = true;
    reloadPosts();
  });

  onMount(() => {
    init();
    const unlisteners: Promise<UnlistenFn>[] = [];
    unlisteners.push(
      listen("feed-updated", () => {
        reloadPosts();
      }),
    );
    unlisteners.push(
      listen("profile-updated", (event) => {
        if (event.payload === pubkey) {
          reloadProfile();
        }
      }),
    );
    window.addEventListener("keydown", handleGlobalKey);
    return () => {
      blobs.revokeAll();
      unlisteners.forEach((p) => p.then((fn) => fn()));
      window.removeEventListener("keydown", handleGlobalKey);
    };
  });
</script>

{#if showQr}
  <QrModal nodeId={pubkey} onclose={() => (showQr = false)} />
{/if}

{#if lightboxSrc}
  <Lightbox
    src={lightboxSrc}
    alt={lightboxAlt}
    onclose={() => {
      lightboxSrc = "";
    }}
  />
{/if}

{#if loading}
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
        showToast("Profile saved", "success");
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
        {#if profile?.is_private}
          <span class="private-badge">Private profile</span>
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

  <div class="id-row">
    <code>{pubkey}</code>
    <button class="btn-elevated copy-btn" onclick={copyNodeId}>
      {copyFeedback ? "Copied!" : "Copy ID"}
    </button>
    <button class="btn-elevated copy-btn" onclick={() => (showQr = true)}
      >QR</button
    >
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
    Posts{posts.length > 0 ? ` (${posts.length}${hasMore ? "+" : ""})` : ""}
    {#if syncStatus && !isSelf}
      <span class="sync-info">
        {syncStatus.local_count}{remoteTotal != null ? ` / ${remoteTotal}` : ""} synced
      </span>
    {/if}
  </h3>

  {#if pendingDeleteId}
    <DeleteConfirmModal onconfirm={executeDelete} oncancel={cancelDelete} />
  {/if}

  <div class="feed">
    {#each posts as post (post.id)}
      <PostCard
        {post}
        {nodeId}
        showAuthor={false}
        showDelete={isSelf}
        onreply={(p) => {
          replyingTo = replyingTo?.id === p.id ? null : p;
          quotingPost = null;
        }}
        ondelete={confirmDelete}
        onquote={(p) => {
          quotingPost = quotingPost?.id === p.id ? null : p;
          replyingTo = null;
        }}
        onlightbox={(src, alt) => {
          lightboxSrc = src;
          lightboxAlt = alt;
        }}
      />
      {#if replyingTo?.id === post.id}
        <ReplyComposer
          replyToId={post.id}
          replyToAuthor={post.author}
          {nodeId}
          onsubmitted={() => {
            replyingTo = null;
            reloadPosts();
          }}
          oncancel={() => (replyingTo = null)}
        />
      {/if}
      {#if quotingPost?.id === post.id}
        <QuoteComposer
          quotedPost={post}
          {nodeId}
          onsubmitted={() => {
            quotingPost = null;
            reloadPosts();
          }}
          oncancel={() => (quotingPost = null)}
        />
      {/if}
    {:else}
      <p class="empty">No posts from this user yet.</p>
    {/each}
  </div>

  {#if hasMore && posts.length > 0}
    <div bind:this={sentinel} class="sentinel">
      {#if loadingMore}
        <span class="btn-spinner"></span>
        {#if fetchingRemote}
          Fetching from peer...
        {:else}
          Loading...
        {/if}
      {/if}
    </div>
  {/if}

  {#if peerOffline && !hasMore && posts.length > 0}
    <p class="offline-notice">End of cached posts -- peer is offline</p>
  {/if}
{/if}

{#if toastMessage}
  <div class="toast" class:error={toastType === "error"}>
    {toastMessage}
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

  .private-badge {
    display: inline-block;
    font-size: var(--text-sm);
    color: var(--color-warning);
    border: 1px solid var(--color-warning-border);
    border-radius: var(--radius-sm);
    padding: 0.15rem 0.5rem;
    margin-top: 0.25rem;
  }

  .id-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 1rem;
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
