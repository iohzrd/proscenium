<script lang="ts">
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
    FollowRequestOutcome,
    Post,
    Profile,
    SocialGraphEntry,
    SyncResult,
    SyncStatus,
    RemoteSocialResult,
    RemoteFollowersResult,
  } from "$lib/types";
  import {
    coalesce,
    dedupeById,
    mergeFirstPage,
    seedOwnProfile,
    shortId,
  } from "$lib/utils";
  import {
    useToast,
    useCopyFeedback,
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useDeleteConfirm,
    useLightbox,
  } from "$lib/composables";

  // The parent creates one instance per pubkey, so `pubkey` does not change
  // during the lifetime of this component.
  let {
    pubkey,
    transportHint,
  }: {
    pubkey: string;
    /** Node ID from an invite link; used only if no Node ID is cached. */
    transportHint: string | null;
  } = $props();

  const PAGE_SIZE = 20;
  // Largest number of rows one post-list refresh reads again.
  const REFRESH_MAX = 100;

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
  // True once `sync_posts` was invoked for this profile.
  let remoteSyncTried = false;
  let isMuted = $state(false);
  let isBlocked = $state(false);
  let togglingMute = $state(false);
  let togglingBlock = $state(false);
  let showQr = $state(false);
  let editingProfile = $state(false);
  let transportNodeIds = $state<string[]>([]);
  let remoteFollows = $state<RemoteSocialResult | null>(null);
  let remoteFollowers = $state<RemoteFollowersResult | null>(null);

  const toast = useToast();
  const blobs = createBlobCache({ onError: toast.show });
  setBlobContext(blobs);

  const copyFb = useCopyFeedback();
  const lightbox = useLightbox();
  const del = useDeleteConfirm(async (id) => {
    try {
      await invoke("delete_post", { id });
      await refreshPosts();
    } catch (e) {
      toast.show("Failed to delete post");
      console.error("Failed to delete post:", e);
    }
  });

  const node = useNodeInit(async () => {
    await loadProfile();
    await refreshPosts();

    const follows: SocialGraphEntry[] = await invoke("get_follows");
    isFollowing = follows.some((f) => f.pubkey === pubkey);

    if (pubkey === node.pubkey) return;

    isMuted = await invoke("is_muted", { pubkey });
    isBlocked = await invoke("is_blocked", { pubkey });
    await loadSyncStatus();
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
  });

  let isSelf = $derived(pubkey === node.pubkey);
  let displayName = $derived(
    profile?.display_name || (isSelf ? "You" : shortId(pubkey)),
  );

  function mediaFilterArg(filter: string): string | null {
    return filter === "all" ? null : filter;
  }

  /** Load the profile and the Node IDs of the displayed user. */
  async function loadProfile() {
    if (pubkey === node.pubkey) {
      profile = await invoke("get_my_profile");
      transportNodeIds = [node.nodeId];
      return;
    }
    profile = await invoke("get_remote_profile", { pubkey });
    try {
      transportNodeIds = await invoke("get_peer_node_ids", { pubkey });
    } catch {
      transportNodeIds = [];
    }
  }

  async function reloadProfile() {
    try {
      await loadProfile();
    } catch (e) {
      console.error("Failed to reload profile:", e);
    }
  }

  async function loadSyncStatus() {
    try {
      syncStatus = await invoke("get_sync_status", { pubkey });
    } catch {
      // sync status is informational
    }
  }

  /**
   * Read the first rows of the post list again and merge them into `posts`
   * by id. Rows loaded by pagination beyond the refreshed range are kept.
   * A result for a media filter that is no longer selected is discarded.
   */
  const refreshPosts = coalesce(async () => {
    const filter = mediaFilter;
    const limit = Math.min(Math.max(PAGE_SIZE, posts.length), REFRESH_MAX);
    try {
      const fresh: Post[] = await invoke("get_user_posts", {
        pubkey,
        limit,
        before: null,
        beforeId: null,
        mediaFilter: mediaFilterArg(filter),
      });
      if (filter !== mediaFilter) return;
      const merged = mergeFirstPage(posts, fresh, limit, "desc");
      posts = merged.items;
      if (merged.exhausted) scroll.setNoMore();
      else if (!merged.keptExisting) scroll.reset();
    } catch (e) {
      toast.show("Failed to load posts");
      console.error("Failed to load posts:", e);
    }
  });

  function changeMediaFilter(value: string) {
    if (value === mediaFilter) return;
    mediaFilter = value;
    posts = [];
    scroll.reset();
    refreshPosts();
  }

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      const last = posts[posts.length - 1];
      if (!last) return false;
      const filter = mediaFilter;
      try {
        const older: Post[] = await invoke("get_user_posts", {
          pubkey,
          limit: PAGE_SIZE,
          before: last.timestamp,
          beforeId: last.id,
          mediaFilter: mediaFilterArg(filter),
        });
        // The filter or the cursor row changed: this page is outdated.
        if (filter !== mediaFilter) return;
        if (posts[posts.length - 1]?.id !== last.id) return;
        const known = new Set(posts.map((p) => p.id));
        const added = dedupeById(older).filter((p) => !known.has(p.id));
        if (added.length > 0) {
          posts = [...posts, ...added];
          scroll.setHasMore(older.length);
        } else if (
          !isSelf &&
          !peerOffline &&
          !remoteSyncTried &&
          mediaFilter === "all"
        ) {
          await fetchFromRemote();
        } else {
          scroll.setNoMore();
        }
      } catch (e) {
        toast.show("Failed to load more posts");
        console.error("Failed to load more:", e);
        return false;
      }
    },
    PAGE_SIZE,
  );

  /**
   * Sync this user's posts from the peer once, then read the list again from
   * local storage so the stored rows appear at their `(timestamp, id)` position.
   */
  async function fetchFromRemote() {
    remoteSyncTried = true;
    fetchingRemote = true;
    try {
      const result: SyncResult = await invoke("sync_posts", {
        pubkey,
      });
      remoteTotal = result.remote_total;
      if (result.posts.length > 0) {
        await loadSyncStatus();
        await refreshPosts();
      } else {
        scroll.setNoMore();
      }
    } catch {
      peerOffline = true;
      scroll.setNoMore();
    }
    fetchingRemote = false;
  }

  async function unfollow() {
    if (
      !confirm(
        "Unfollow this user? Their posts will be deleted from your device.",
      )
    ) {
      return;
    }
    try {
      await invoke("unfollow_user", { pubkey });
      isFollowing = false;
      await refreshPosts();
    } catch (e) {
      toast.show(`Failed to unfollow: ${e}`);
      console.error("Unfollow failed:", e);
    }
  }

  async function follow() {
    const nodeId = transportNodeIds[0] ?? transportHint;
    if (!nodeId) {
      toast.show("No transport Node ID known for this user");
      return;
    }
    try {
      // A profile that is not public accepts followers only on request.
      if (profile && profile.visibility !== "public") {
        const outcome: FollowRequestOutcome = await invoke(
          "send_follow_request_to_peer",
          { pubkey },
        );
        if (outcome === "pending") {
          toast.show("Follow request sent. Waiting for approval.", "success");
          return;
        }
        if (outcome === "denied") {
          toast.show("Follow request denied");
          return;
        }
      }
      // `follow_user` fails if the node reports a master pubkey other than `pubkey`.
      await invoke("follow_user", { nodeId, expectedPubkey: pubkey });
      isFollowing = true;
      await reloadProfile();
      await loadSyncStatus();
    } catch (e) {
      toast.show(`Failed to follow: ${e}`);
      console.error("Follow failed:", e);
    }
  }

  async function toggleFollow() {
    if (toggling) return;
    toggling = true;
    try {
      await (isFollowing ? unfollow() : follow());
    } finally {
      toggling = false;
    }
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

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "feed-updated": () => {
        refreshPosts();
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
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
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
        await seedOwnProfile(pubkey);
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

  <MediaFilterBar {mediaFilter} onFilterChange={changeMediaFilter} />

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
    onreload={refreshPosts}
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
