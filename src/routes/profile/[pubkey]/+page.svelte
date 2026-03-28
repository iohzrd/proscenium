<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import QrModal from "$lib/QrModal.svelte";
  import PostFeed from "$lib/PostFeed.svelte";
  import DeleteConfirmModal from "$lib/DeleteConfirmModal.svelte";
  import ProfileEditor from "$lib/ProfileEditor.svelte";
  import ProfileHeaderCard from "$lib/profile/ProfileHeaderCard.svelte";
  import ProfileIdSection from "$lib/profile/ProfileIdSection.svelte";
  import ProfileActions from "$lib/profile/ProfileActions.svelte";
  import SocialGraphAccordion from "$lib/profile/SocialGraphAccordion.svelte";
  import MediaFilterBar from "$lib/profile/MediaFilterBar.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import type {
    Post,
    Profile,
    SocialGraphEntry,
    SyncResult,
    SyncStatus,
    RemoteSocialResult,
    RemoteFollowersResult,
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
  } from "$lib/composables";

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
  let remoteFollows = $state<RemoteSocialResult | null>(null);
  let remoteFollowers = $state<RemoteFollowersResult | null>(null);

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

    const follows: SocialGraphEntry[] = await invoke("get_follows");
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
      // Fetch remote social graph (non-blocking).
      Promise.all([
        invoke<RemoteSocialResult>("get_remote_follows", { pubkey }).catch(
          () => null,
        ),
        invoke<RemoteFollowersResult>("get_remote_followers", {
          pubkey,
        }).catch(() => null),
      ]).then(([follows, followers]) => {
        remoteFollows = follows;
        remoteFollowers = followers;
      });
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
  <Lightbox
    src={lightbox.src}
    alt={lightbox.alt}
    attachment={lightbox.attachment}
    onclose={lightbox.close}
  />
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
    <ProfileHeaderCard
      {pubkey}
      {displayName}
      {profile}
      {isSelf}
      onEdit={() => (editingProfile = true)}
    />
  {/if}

  <ProfileIdSection
    {pubkey}
    {transportNodeIds}
    copyFeedback={copyFb.feedback}
    onCopy={copyFb.copy}
    onShowQr={() => (showQr = true)}
  />

  {#if !isSelf}
    <ProfileActions
      {pubkey}
      {isFollowing}
      {toggling}
      {isBlocked}
      {isMuted}
      {togglingMute}
      {togglingBlock}
      onToggleFollow={toggleFollow}
      onToggleMute={toggleMute}
      onToggleBlock={toggleBlock}
      onStartCall={startCall}
    />
  {/if}

  {#if !isSelf}
    <SocialGraphAccordion {remoteFollows} {remoteFollowers} />
  {/if}

  <MediaFilterBar
    {mediaFilter}
    onFilterChange={(value) => (mediaFilter = value)}
  />

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
</style>
