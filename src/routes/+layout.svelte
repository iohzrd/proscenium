<script lang="ts">
  import "../app.css";
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { onMount } from "svelte";
  import {
    isPermissionGranted,
    requestPermission,
    sendNotification,
  } from "@tauri-apps/plugin-notification";
  import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
  import { goto } from "$app/navigation";
  import type {
    NodeStatus,
    Post,
    StoredMessage,
    CallEvent,
    CallState,
  } from "$lib/types";
  import Sidebar from "$lib/Sidebar.svelte";
  import RightSidebar from "$lib/RightSidebar.svelte";
  import BottomNav from "$lib/BottomNav.svelte";
  import MobileHeader from "$lib/MobileHeader.svelte";
  import CallOverlay from "$lib/CallOverlay.svelte";
  import { applyAccent, DEFAULT_ACCENT } from "$lib/accent";
  import { useEventListeners, useNodeInit } from "$lib/composables";
  import {
    liveStages,
    stageStore,
    startLiveStageTracking,
    startStageSync,
  } from "$lib/stage.svelte";
  import {
    evictDisplayName,
    getDisplayName,
    isPubkeyFormat,
    isWindowActive,
    seedOwnProfile,
    truncate,
  } from "$lib/utils";
  import StageIndicator from "./StageIndicator.svelte";

  const ZOOM_KEY = "app-zoom-level";
  const ZOOM_STEP = 0.2;
  const ZOOM_MIN = 0.2;
  const ZOOM_MAX = 10.0;
  // Set in sessionStorage once the deep link that started the app was handled,
  // so a webview reload does not navigate to it again.
  const STARTUP_LINK_KEY = "startup-deep-link-handled";
  const NOTIFICATION_BODY_CHARS = 100;
  const CALL_END_DISPLAY_MS = 2000;

  let { children } = $props();
  let status = $state<NodeStatus | null>(null);
  let zoomLevel = $state(1.0);
  let unreadDmCount = $state(0);
  let unreadNotificationCount = $state(0);
  let activeCallId = $state<string | null>(null);
  let activeCallPeer = $state("");
  let activeCallState = $state<CallState | null>(null);
  // Result of the OS notification permission request; false until it resolves.
  let notificationsPermitted = false;

  // The identity is read once per webview load. Flows that replace the local
  // identity (recovery, device link) reload the webview afterwards.
  const node = useNodeInit(async () => {
    await seedOwnProfile(node.pubkey);
  });

  async function applyZoom(level: number) {
    zoomLevel = Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, level));
    localStorage.setItem(ZOOM_KEY, String(zoomLevel));
    await getCurrentWebview().setZoom(zoomLevel);
  }

  function handleZoomKeys(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (!mod) return;
    if (e.key === "=" || e.key === "+") {
      e.preventDefault();
      applyZoom(zoomLevel + ZOOM_STEP);
    } else if (e.key === "-") {
      e.preventDefault();
      applyZoom(zoomLevel - ZOOM_STEP);
    } else if (e.key === "0") {
      e.preventDefault();
      applyZoom(1.0);
    }
  }

  function handleDeepLink(url: string) {
    try {
      const parsed = new URL(url);
      if (parsed.protocol !== "proscenium:") return;
      if (parsed.hostname !== "profile" && parsed.hostname !== "user") return;
      const id = parsed.pathname.slice(1);
      if (!isPubkeyFormat(id)) return;
      const transport = parsed.searchParams.get("transport");
      const query =
        transport && isPubkeyFormat(transport) ? `?transport=${transport}` : "";
      goto(`/profile/${id}${query}`);
    } catch {
      // malformed URL, ignore
    }
  }

  /** Handle the deep link that started the app, once per app run. */
  async function handleStartupDeepLink() {
    if (sessionStorage.getItem(STARTUP_LINK_KEY)) return;
    try {
      const urls = await getCurrent();
      sessionStorage.setItem(STARTUP_LINK_KEY, "1");
      for (const url of urls ?? []) handleDeepLink(url);
    } catch (e) {
      console.error("Failed to read startup deep link:", e);
    }
  }

  async function pollStatus() {
    try {
      status = await invoke("get_node_status");
    } catch {
      // Node not ready yet
      return;
    }
    // The node answers now; load the identity if the bounded retries ended before that.
    if (node.error) node.init();
  }

  async function pollUnread() {
    try {
      unreadDmCount = await invoke("get_unread_dm_count");
    } catch {
      // Node not ready yet
    }
  }

  async function pollUnreadNotifications() {
    try {
      unreadNotificationCount = await invoke("get_unread_notification_count");
    } catch {
      // Node not ready yet
    }
  }

  async function resolveNotificationPermission() {
    try {
      let granted = await isPermissionGranted();
      if (!granted) granted = (await requestPermission()) === "granted";
      notificationsPermitted = granted;
    } catch (e) {
      console.error("Notification permission request failed:", e);
    }
  }

  /** True if the user is looking at `path`: it is the current route and the window is active. */
  function isViewing(path: string): boolean {
    return page.url.pathname === path && isWindowActive();
  }

  async function handleDmReceived(payload: {
    from: string;
    message: StoredMessage;
  }) {
    pollUnread();
    if (!notificationsPermitted || isViewing(`/messages/${payload.from}`)) {
      return;
    }
    const title = await getDisplayName(payload.from, node.pubkey);
    sendNotification({
      title,
      body:
        truncate(payload.message.content, NOTIFICATION_BODY_CHARS) ||
        "Sent a message",
    });
  }

  async function handleMention(post: Post) {
    pollUnreadNotifications();
    if (!notificationsPermitted || isViewing("/activity")) return;
    const name = await getDisplayName(post.author, node.pubkey);
    sendNotification({
      title: `${name} mentioned you`,
      body:
        truncate(post.content, NOTIFICATION_BODY_CHARS) ||
        "Mentioned you in a post",
    });
  }

  function handleCallState({ call_id, peer_pubkey, state }: CallEvent) {
    activeCallId = call_id;
    activeCallPeer = peer_pubkey;
    activeCallState = state;
    if (state === "ended" || state === "failed") {
      // Show briefly then dismiss
      setTimeout(() => {
        if (activeCallId === call_id) {
          activeCallId = null;
          activeCallState = null;
        }
      }, CALL_END_DISPLAY_MS);
    }
  }

  function handleProfileUpdated(updated: string) {
    evictDisplayName(updated);
    if (updated === node.pubkey) seedOwnProfile(updated);
  }

  onMount(() => {
    const saved = localStorage.getItem(ZOOM_KEY);
    if (saved) {
      const parsed = parseFloat(saved);
      if (Number.isFinite(parsed)) {
        applyZoom(parsed);
      }
    }

    window.addEventListener("keydown", handleZoomKeys);

    // Listeners are registered before any await, so no event emitted during
    // the permission request or the identity load is missed.
    const cleanupListeners = useEventListeners({
      "dm-received": (payload) =>
        handleDmReceived(payload as { from: string; message: StoredMessage }),
      "mentioned-in-post": (payload) => handleMention(payload as Post),
      "notification-received": () => pollUnreadNotifications(),
      "call-state": (payload) => handleCallState(payload as CallEvent),
      "profile-updated": (payload) => handleProfileUpdated(payload as string),
      "deep-link-received": (payload) => {
        for (const url of payload as string[]) handleDeepLink(url);
      },
    });
    const unlistenOpenUrl = onOpenUrl((urls) => {
      for (const url of urls) handleDeepLink(url);
    });
    const stopStageSync = startStageSync();
    const stopLiveStageTracking = startLiveStageTracking();

    node.init();
    invoke<string | null>("get_accent_color")
      .then((name) => applyAccent(name ?? DEFAULT_ACCENT))
      .catch(() => applyAccent(DEFAULT_ACCENT));
    resolveNotificationPermission();
    handleStartupDeepLink();
    pollStatus();
    pollUnread();
    pollUnreadNotifications();
    const statusInterval = setInterval(pollStatus, 10000);
    const unreadInterval = setInterval(pollUnread, 10000);
    const notificationInterval = setInterval(pollUnreadNotifications, 10000);

    return () => {
      window.removeEventListener("keydown", handleZoomKeys);
      clearInterval(statusInterval);
      clearInterval(unreadInterval);
      clearInterval(notificationInterval);
      cleanupListeners();
      unlistenOpenUrl.then((fn) => fn());
      stopStageSync();
      stopLiveStageTracking();
    };
  });
</script>

<div class="app-shell">
  <Sidebar
    pubkey={node.pubkey}
    {status}
    {unreadDmCount}
    {unreadNotificationCount}
    currentPath={page.url.pathname}
  />

  <MobileHeader {status} />

  <div class="main-column">
    {#if status && !status.has_relay}
      <div class="relay-banner">
        <span class="relay-banner-dot"></span>
        <span>Relay disconnected -- messages and sync may not work</span>
      </div>
    {/if}
    {#if stageStore.stage && page.url.pathname !== "/stage"}
      <StageIndicator />
    {/if}
    <main>
      {@render children()}
    </main>
  </div>

  <RightSidebar {liveStages} />

  <BottomNav
    pubkey={node.pubkey}
    {unreadDmCount}
    {unreadNotificationCount}
    currentPath={page.url.pathname}
  />
</div>

{#if activeCallId && activeCallState}
  {#key activeCallId}
    <CallOverlay
      callId={activeCallId}
      peerPubkey={activeCallPeer}
      callState={activeCallState}
      selfId={node.pubkey}
    />
  {/key}
{/if}

<style>
  .app-shell {
    min-height: 100vh;
  }

  .main-column {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    position: relative;
  }

  main {
    max-width: var(--content-max-width);
    width: 100%;
    margin: 0 auto;
    padding: var(--space-lg) var(--space-xl);
    padding-bottom: calc(
      var(--bottom-nav-height) + env(safe-area-inset-bottom) + var(--space-lg)
    );
    flex: 1;
  }

  .relay-banner {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 1rem;
    background: var(--danger-bg);
    border-bottom: 1px solid var(--danger-border);
    color: var(--danger-text);
    font-size: var(--text-base);
    font-weight: 500;
  }

  .relay-banner-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--color-error);
    flex-shrink: 0;
    animation: pulse-dot 2s infinite;
  }

  @keyframes pulse-dot {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.4;
    }
  }

  @media (min-width: 768px) {
    .app-shell {
      padding-left: var(--sidebar-width);
    }

    main {
      padding-bottom: var(--space-lg);
    }
  }

  @media (min-width: 1150px) {
    .app-shell {
      padding-right: var(--right-sidebar-width);
    }
  }
</style>
