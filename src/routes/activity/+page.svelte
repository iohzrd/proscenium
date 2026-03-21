<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import PostCard from "$lib/PostCard.svelte";
  import Lightbox from "$lib/Lightbox.svelte";
  import Avatar from "$lib/Avatar.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import { createBlobCache, setBlobContext } from "$lib/blobs";
  import type { AppNotification, Post } from "$lib/types";
  import { getDisplayName, shortId } from "$lib/utils";
  import {
    useNodeInit,
    useEventListeners,
    useInfiniteScroll,
    useLightbox,
  } from "$lib/composables.svelte";

  let notifications = $state<AppNotification[]>([]);
  let sentinel = $state<HTMLDivElement>(null!);
  let filter = $state("all");
  let postCache = $state<Record<string, Post>>({});
  let nameCache = $state<Record<string, string>>({});

  const FILTERS = [
    { value: "all", label: "All" },
    { value: "mention", label: "Mentions" },
    { value: "like", label: "Likes" },
    { value: "reply", label: "Replies" },
    { value: "quote", label: "Quotes" },
    { value: "follower", label: "Followers" },
  ] as const;

  const blobs = createBlobCache();
  setBlobContext(blobs);

  const lightbox = useLightbox();

  let filtered = $derived(
    filter === "all"
      ? notifications
      : notifications.filter((n) => n.kind === filter),
  );

  async function resolveName(pubkey: string): Promise<string> {
    if (nameCache[pubkey]) return nameCache[pubkey];
    const name = await getDisplayName(pubkey, node.pubkey);
    nameCache = { ...nameCache, [pubkey]: name };
    return name;
  }

  async function fetchPostsForNotifications(notifs: AppNotification[]) {
    const ids = new Set<string>();
    for (const n of notifs) {
      if (n.post_id && !postCache[n.post_id]) ids.add(n.post_id);
      if (n.target_post_id && !postCache[n.target_post_id])
        ids.add(n.target_post_id);
    }
    const actors = new Set<string>();
    for (const n of notifs) {
      if (!nameCache[n.actor]) actors.add(n.actor);
    }
    const postPromises = [...ids].map(async (id) => {
      try {
        const post: Post | null = await invoke("get_post", { id });
        if (post) {
          postCache = { ...postCache, [id]: post };
        }
      } catch {
        // post may have been deleted
      }
    });
    const namePromises = [...actors].map((pubkey) => resolveName(pubkey));
    await Promise.all([...postPromises, ...namePromises]);
  }

  const node = useNodeInit(async () => {
    await loadNotifications();
    await invoke("mark_notifications_read");
  });

  async function loadNotifications() {
    try {
      const result: AppNotification[] = await invoke("get_notifications", {
        limit: 30,
      });
      notifications = result;
      scroll.setHasMore(result.length);
      await fetchPostsForNotifications(result);
    } catch (e) {
      console.error("Failed to load notifications:", e);
    }
  }

  const scroll = useInfiniteScroll(
    () => sentinel,
    async () => {
      try {
        const oldest = notifications[notifications.length - 1];
        const more: AppNotification[] = await invoke("get_notifications", {
          limit: 30,
          before: oldest.timestamp,
        });
        if (more.length === 0) {
          scroll.setNoMore();
        } else {
          notifications = [...notifications, ...more];
          scroll.setHasMore(more.length);
          await fetchPostsForNotifications(more);
        }
      } catch (e) {
        console.error("Failed to load more notifications:", e);
      }
    },
    30,
  );

  function postForNotification(n: AppNotification): Post | undefined {
    if (n.kind === "like") {
      return n.target_post_id ? postCache[n.target_post_id] : undefined;
    }
    return n.post_id ? postCache[n.post_id] : undefined;
  }

  function notifLabel(kind: string): string {
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
      default:
        return "";
    }
  }

  $effect(() => {
    return scroll.setupEffect();
  });

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "notification-received": () => {
        loadNotifications();
        invoke("mark_notifications_read");
      },
      "mentioned-in-post": () => {
        loadNotifications();
        invoke("mark_notifications_read");
      },
    });
    return () => {
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
      onclick={() => (filter = f.value)}
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
{:else if filtered.length === 0}
  <div class="empty">
    <p>No notifications yet.</p>
    <p class="hint">
      Mentions, likes, replies, quotes, and new followers will appear here.
    </p>
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
              name={nameCache[notif.actor] || shortId(notif.actor)}
              size={28}
            />
            <span class="actor-name"
              >{nameCache[notif.actor] || shortId(notif.actor)}</span
            >
          </a>
          <span class="notif-label">{notifLabel(notif.kind)}</span>
          <Timeago timestamp={notif.timestamp} />
        </div>

        {#if notif.kind === "follower"}
          <a href="/profile/{notif.actor}" class="follower-link">View profile</a
          >
        {:else if post}
          <div class="notif-post">
            <PostCard {post} pubkey={node.pubkey} onlightbox={lightbox.open} />
          </div>
        {:else}
          <p class="notif-deleted">Post no longer available</p>
        {/if}
      </div>
    {/each}
  </div>

  {#if scroll.hasMore && notifications.length > 0}
    <div bind:this={sentinel} class="sentinel">
      {#if scroll.loadingMore}
        <span class="btn-spinner"></span> Loading...
      {/if}
    </div>
  {/if}
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
