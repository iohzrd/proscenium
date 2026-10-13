<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import PostCard from "$lib/PostCard.svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import Avatar from "$lib/Avatar.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import type { AppNotification, Post } from "$lib/types";
  import {
    coalesce,
    dedupeById,
    getCachedDisplayName,
    getDisplayName,
    isWindowActive,
    mergeFirstPage,
    shortId,
  } from "$lib/utils";
  import {
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useLightbox,
    useToast,
  } from "$lib/composables";

  const PAGE_SIZE = 30;
  // Largest number of rows one refresh reads again.
  const REFRESH_MAX = 90;

  let notifications = $state<AppNotification[]>([]);
  let sentinel = $state<HTMLDivElement>(null!);
  let filter = $state("all");
  let postCache = $state<Record<string, Post>>({});

  const FILTERS = [
    { value: "all", label: "All" },
    { value: "mention", label: "Mentions" },
    { value: "like", label: "Likes" },
    { value: "reply", label: "Replies" },
    { value: "quote", label: "Quotes" },
    { value: "follower", label: "Followers" },
    { value: "follow_request", label: "Requests" },
  ] as const;

  const toast = useToast();
  const blobs = createBlobCache({ onError: toast.show });
  setBlobContext(blobs);

  const lightbox = useLightbox();

  let filtered = $derived(
    filter === "all"
      ? notifications
      : notifications.filter((n) => n.kind === filter),
  );

  function actorName(pubkey: string): string {
    if (pubkey === node.pubkey) return "You";
    return getCachedDisplayName(pubkey) ?? shortId(pubkey);
  }

  /** Load the posts and actor names that `notifs` reference and that are not cached. */
  async function fetchPostsForNotifications(notifs: AppNotification[]) {
    const ids = new Set<string>();
    const actors = new Set<string>();
    for (const n of notifs) {
      if (n.post_id && !postCache[n.post_id]) ids.add(n.post_id);
      if (n.target_post_id && !postCache[n.target_post_id]) {
        ids.add(n.target_post_id);
      }
      actors.add(n.actor);
    }
    const postPromises = [...ids].map(async (id) => {
      try {
        const post: Post | null = await invoke("get_post", { id });
        if (post) postCache[id] = post;
      } catch {
        // post may have been deleted
      }
    });
    const namePromises = [...actors].map((pubkey) =>
      getDisplayName(pubkey, node.pubkey),
    );
    await Promise.all([...postPromises, ...namePromises]);
  }

  /** Clear the unread state. Returns early unless the window is active. */
  function markRead() {
    if (!isWindowActive()) return;
    invoke("mark_notifications_read").catch((e) =>
      console.error("Failed to mark notifications read:", e),
    );
  }

  const node = useNodeInit(async () => {
    await refreshNotifications();
    markRead();
  });

  /**
   * Read the first rows of the notification list again and merge them into
   * `notifications` by id. Rows loaded by pagination beyond the refreshed
   * range are kept.
   */
  const refreshNotifications = coalesce(async () => {
    const limit = Math.min(
      Math.max(PAGE_SIZE, notifications.length),
      REFRESH_MAX,
    );
    try {
      const fresh: AppNotification[] = await invoke("get_notifications", {
        limit,
      });
      const merged = mergeFirstPage(notifications, fresh, limit, "desc");
      notifications = merged.items;
      if (merged.exhausted) scroll.setNoMore();
      else if (!merged.keptExisting) scroll.reset();
      await fetchPostsForNotifications(fresh);
    } catch (e) {
      toast.show("Failed to load notifications");
      console.error("Failed to load notifications:", e);
    }
  });

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      const last = notifications[notifications.length - 1];
      if (!last) return false;
      try {
        const more: AppNotification[] = await invoke("get_notifications", {
          limit: PAGE_SIZE,
          before: last.timestamp,
          beforeId: last.id,
        });
        // The cursor row is no longer the last row: this page is outdated.
        if (notifications[notifications.length - 1]?.id !== last.id) return;
        const known = new Set(notifications.map((n) => n.id));
        const added = dedupeById(more).filter((n) => !known.has(n.id));
        if (added.length === 0) {
          scroll.setNoMore();
          return;
        }
        notifications = [...notifications, ...added];
        scroll.setHasMore(more.length);
        await fetchPostsForNotifications(added);
      } catch (e) {
        toast.show("Failed to load more notifications");
        console.error("Failed to load more notifications:", e);
        return false;
      }
    },
    PAGE_SIZE,
  );

  function selectFilter(value: string) {
    filter = value;
    // A filter with few matches leaves the sentinel in view without an
    // intersection change; request the next page explicitly.
    scroll.checkSentinel();
  }

  function postForNotification(n: AppNotification): Post | undefined {
    if (n.kind === "like") {
      return n.target_post_id ? postCache[n.target_post_id] : undefined;
    }
    return n.post_id ? postCache[n.post_id] : undefined;
  }

  function notifLabel(kind: AppNotification["kind"]): string {
    switch (kind) {
      case "mention":
        return "mentioned you";
      case "like":
        return "liked your post";
      case "reply":
        return "replied to your post";
      case "quote":
        return "quoted your post";
      case "follower":
        return "started following you";
      case "follow_request":
        return "requested to follow you";
      default:
        return "";
    }
  }

  function handleActivation() {
    if (!node.loading) markRead();
  }

  $effect(() => {
    return scroll.setupEffect();
  });

  onMount(() => {
    node.init();
    const onEvent = () => {
      refreshNotifications();
      markRead();
    };
    const cleanupListeners = useEventListeners({
      "notification-received": onEvent,
      "mentioned-in-post": onEvent,
    });
    window.addEventListener("focus", handleActivation);
    document.addEventListener("visibilitychange", handleActivation);
    return () => {
      window.removeEventListener("focus", handleActivation);
      document.removeEventListener("visibilitychange", handleActivation);
      cleanupListeners();
      blobs.revokeAll();
    };
  });
</script>

{#if lightbox.src}
  <Lightbox
    src={lightbox.src}
    alt={lightbox.alt}
    attachment={lightbox.attachment}
    onclose={lightbox.close}
  />
{/if}

<h2 class="page-title">Notifications</h2>

<div class="filter-bar">
  {#each FILTERS as f (f.value)}
    <button
      class="filter-chip"
      class:active={filter === f.value}
      onclick={() => selectFilter(f.value)}
    >
      {f.label}
    </button>
  {/each}
</div>

{#if node.loading}
  <div class="loading">
    <div class="spinner"></div>
    <p>Loading notifications...</p>
  </div>
{:else if node.error}
  <div class="empty">
    <p>{node.error}</p>
    <button class="btn-elevated" onclick={node.init}>Retry</button>
  </div>
{:else}
  {#if filtered.length === 0}
    <div class="empty">
      {#if filter === "all"}
        <p>No notifications yet.</p>
        <p class="hint">
          Mentions, likes, replies, quotes, new followers, and follow requests
          will appear here.
        </p>
      {:else if scroll.hasMore && notifications.length > 0}
        <p>No notifications of this type in the loaded range.</p>
      {:else}
        <p>No notifications of this type.</p>
      {/if}
    </div>
  {:else}
    <div class="notifications">
      {#each filtered as notif (notif.id)}
        {@const post = postForNotification(notif)}
        <div class="notif" class:unread={!notif.read}>
          <div class="notif-header">
            <a href="/profile/{notif.actor}" class="notif-actor">
              <Avatar
                pubkey={notif.actor}
                name={actorName(notif.actor)}
                size={28}
              />
              <span class="actor-name">{actorName(notif.actor)}</span>
            </a>
            <span class="notif-label">{notifLabel(notif.kind)}</span>
            <Timeago timestamp={notif.timestamp} />
          </div>

          {#if notif.kind === "follower"}
            <a href="/profile/{notif.actor}" class="follower-link"
              >View profile</a
            >
          {:else if notif.kind === "follow_request"}
            <a href="/follows?tab=requests" class="follower-link"
              >Review in Requests</a
            >
          {:else if post}
            <div class="notif-post">
              <PostCard
                {post}
                pubkey={node.pubkey}
                onlightbox={lightbox.open}
              />
            </div>
          {:else}
            <p class="notif-deleted">Post no longer available</p>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  <!-- Outside the branch above: a filter without matches must not remove the sentinel. -->
  {#if scroll.hasMore && notifications.length > 0}
    <div bind:this={sentinel} class="sentinel">
      {#if scroll.loadingMore}
        <span class="btn-spinner"></span> Loading...
      {/if}
    </div>
  {/if}
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

<style>
  .notif {
    border-bottom: 1px solid var(--border);
    padding: 0.75rem 0;
    transition: background var(--transition-fast);
  }

  .notif:hover {
    background: var(--bg-surface);
  }

  .notif.unread {
    border-left: 3px solid var(--accent);
    padding-left: 0.75rem;
  }

  .notif-header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-base);
    color: var(--text-secondary);
    margin-bottom: 0.5rem;
    flex-wrap: wrap;
  }

  .notif-actor {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    text-decoration: none;
    color: inherit;
  }

  .notif-actor:hover .actor-name {
    text-decoration: underline;
  }

  .actor-name {
    color: var(--accent-light);
    font-weight: 600;
  }

  .notif-label {
    color: var(--text-secondary);
  }

  .notif-post {
    margin-top: 0.25rem;
  }

  .notif-deleted {
    color: var(--text-muted);
    font-size: var(--text-base);
    font-style: italic;
    margin: 0.25rem 0 0;
  }

  .follower-link {
    display: inline-block;
    color: var(--accent-medium);
    font-size: var(--text-base);
    text-decoration: none;
    margin-top: 0.25rem;
  }

  .follower-link:hover {
    text-decoration: underline;
  }
</style>
