<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import { platform } from "@tauri-apps/plugin-os";
  import { onMount } from "svelte";
  import DeleteConfirmModal from "#lib/DeleteConfirmModal.svelte";
  import PersonItem from "#lib/PersonItem.svelte";
  import ScannerModal from "#lib/ScannerModal.svelte";
  import { hapticImpact } from "#lib/haptics.ts";
  import type {
    SocialGraphEntry,
    FollowOutcome,
    FollowRequestEntry,
  } from "#lib/types.ts";
  import {
    dedupeBy,
    followRequestNotice,
    getCachedDisplayName,
    isPubkeyFormat,
  } from "#lib/utils.ts";
  import {
    useNodeInit,
    useEventListeners,
    useCopyFeedback,
    useToast,
  } from "#lib/composables/index.ts";
  import ConfirmDialog from "../ConfirmDialog.svelte";

  type Tab = "following" | "followers" | "requests";

  let follows = $state<SocialGraphEntry[]>([]);
  let followers = $state<SocialGraphEntry[]>([]);
  let followRequests = $state<FollowRequestEntry[]>([]);
  let mutedPubkeys = $state<string[]>([]);
  let blockedPubkeys = $state<string[]>([]);
  let newNodeId = $state("");
  let addingFollow = $state(false);
  let pendingUnfollowPubkey = $state<string | null>(null);
  // `/follows?tab=requests` opens the Requests tab.
  let activeTab = $state<Tab>(
    page.url.searchParams.get("tab") === "requests" ? "requests" : "following",
  );
  const isMobile = platform() === "android" || platform() === "ios";
  let showScanner = $state(false);
  // Identity read from a scanned invite code, waiting for confirmation.
  let scanned = $state<{ pubkey: string; nodeId: string } | null>(null);

  let pendingRequests = $derived(
    followRequests.filter((r) => r.status === "pending"),
  );
  let resolvedRequests = $derived(
    followRequests.filter((r) => r.status !== "pending"),
  );

  const copyFb = useCopyFeedback();
  const toast = useToast();

  const node = useNodeInit(async () => {
    await Promise.all([
      loadFollows(),
      loadFollowers(),
      loadFollowRequests(),
      loadMuted(),
      loadBlocked(),
    ]);
  });

  async function loadFollows() {
    try {
      const list: SocialGraphEntry[] = await invoke("get_follows");
      follows = dedupeBy(list, (f) => f.pubkey);
    } catch (e) {
      toast.show("Failed to load follows");
      console.error("Failed to load follows:", e);
    }
  }

  async function loadFollowers() {
    try {
      const list: SocialGraphEntry[] = await invoke("get_followers");
      followers = dedupeBy(list, (f) => f.pubkey);
    } catch (e) {
      toast.show("Failed to load followers");
      console.error("Failed to load followers:", e);
    }
  }

  async function loadMuted() {
    try {
      const list: string[] = await invoke("get_muted_pubkeys");
      mutedPubkeys = [...new Set(list)];
    } catch (e) {
      toast.show("Failed to load muted users");
      console.error("Failed to load muted:", e);
    }
  }

  async function loadBlocked() {
    try {
      const list: string[] = await invoke("get_blocked_pubkeys");
      blockedPubkeys = [...new Set(list)];
    } catch (e) {
      toast.show("Failed to load blocked users");
      console.error("Failed to load blocked:", e);
    }
  }

  async function loadFollowRequests() {
    try {
      const list: FollowRequestEntry[] = await invoke("get_follow_requests");
      followRequests = dedupeBy(list, (r) => r.pubkey);
    } catch (e) {
      toast.show("Failed to load follow requests");
      console.error("Failed to load follow requests:", e);
    }
  }

  async function approveRequest(pubkey: string) {
    try {
      const approved: boolean = await invoke("approve_follow_request", {
        pubkey,
      });
      await loadFollowRequests();
      await loadFollowers();
      if (approved) {
        hapticImpact("light");
        toast.show("Approved", "success");
      } else {
        toast.show("The request is no longer pending");
      }
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
  }

  async function denyRequest(pubkey: string) {
    try {
      const denied: boolean = await invoke("deny_follow_request", { pubkey });
      await loadFollowRequests();
      if (denied) {
        hapticImpact("light");
        toast.show("Denied", "success");
      } else {
        toast.show("The request is no longer pending");
      }
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
  }

  async function unmute(pubkey: string) {
    try {
      await invoke("unmute_user", { pubkey });
      await loadMuted();
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
  }

  async function unblock(pubkey: string) {
    try {
      await invoke("unblock_user", { pubkey });
      await loadBlocked();
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
  }

  /**
   * Follow the user whose device has `nodeId`. With `expectedPubkey`, the
   * command fails unless the node reports that master pubkey.
   */
  async function followNode(nodeId: string, expectedPubkey: string | null) {
    if (addingFollow) return false;
    addingFollow = true;
    try {
      const outcome: FollowOutcome = await invoke("follow_user", {
        nodeId,
        expectedPubkey,
      });
      if (outcome !== "followed") {
        toast.show(...followRequestNotice(outcome));
        // A pending request stores the follow too.
        if (outcome === "pending") await loadFollows();
        return outcome === "pending";
      }
      await loadFollows();
      hapticImpact("light");
      toast.show("Followed", "success");
      return true;
    } catch (e) {
      toast.show(`Error: ${e}`);
      return false;
    } finally {
      addingFollow = false;
    }
  }

  async function followFromInput() {
    const nodeId = newNodeId.trim().toLowerCase();
    if (!nodeId) return;
    if (!isPubkeyFormat(nodeId)) {
      toast.show("A Node ID is 64 hex digits");
      return;
    }
    if (await followNode(nodeId, null)) newNodeId = "";
  }

  /** Keep the scanned identity for the confirmation dialog; nothing is followed yet. */
  async function handleScanned(result: {
    pubkey: string;
    transportNodeId?: string;
  }) {
    showScanner = false;
    if (!isPubkeyFormat(result.pubkey)) {
      toast.show("The code does not contain a valid public key");
      return;
    }
    let nodeId = result.transportNodeId;
    if (!nodeId) {
      try {
        const cached: string[] = await invoke("get_peer_node_ids", {
          pubkey: result.pubkey,
        });
        nodeId = cached[0];
      } catch {
        // no cached Node ID
      }
    }
    if (!nodeId || !isPubkeyFormat(nodeId)) {
      toast.show("The code does not contain a Node ID for this user");
      return;
    }
    scanned = { pubkey: result.pubkey, nodeId };
  }

  async function confirmScanned() {
    if (!scanned) return;
    const { pubkey, nodeId } = scanned;
    scanned = null;
    await followNode(nodeId, pubkey);
  }

  async function executeUnfollow() {
    if (!pendingUnfollowPubkey) return;
    const pubkey = pendingUnfollowPubkey;
    pendingUnfollowPubkey = null;
    try {
      await invoke("unfollow_user", { pubkey });
      await loadFollows();
      hapticImpact("light");
    } catch (e) {
      toast.show(`Error: ${e}`);
    }
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      followFromInput();
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape" && showScanner) showScanner = false;
  }

  onMount(() => {
    node.init();
    const cleanupListeners = useEventListeners({
      "follower-changed": () => {
        loadFollowers();
      },
      "new-follower": () => {
        loadFollowers();
      },
      "follow-request-received": () => {
        loadFollowRequests();
      },
    });

    window.addEventListener("keydown", handleGlobalKey);
    return () => {
      window.removeEventListener("keydown", handleGlobalKey);
      cleanupListeners();
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
{:else}
  <div class="tabs">
    <button
      class="tab"
      class:active={activeTab === "following"}
      onclick={() => (activeTab = "following")}
    >
      Following ({follows.length})
    </button>
    <button
      class="tab"
      class:active={activeTab === "followers"}
      onclick={() => (activeTab = "followers")}
    >
      Followers ({followers.length})
    </button>
    <button
      class="tab"
      class:active={activeTab === "requests"}
      onclick={() => (activeTab = "requests")}
    >
      Requests
      {#if pendingRequests.length > 0}
        <span class="request-badge">{pendingRequests.length}</span>
      {/if}
    </button>
  </div>

  {#if activeTab === "following"}
    <div class="add-follow">
      <input
        class="input-base"
        bind:value={newNodeId}
        placeholder="Paste a Node ID to follow..."
        aria-label="Node ID of the user to follow"
        autocomplete="off"
        autocapitalize="none"
        spellcheck="false"
        onkeydown={handleKey}
      />
      {#if isMobile}
        <button class="scan-btn" onclick={() => (showScanner = true)}
          >Scan</button
        >
      {/if}
      <button
        class="follow-btn"
        onclick={followFromInput}
        disabled={!newNodeId.trim() || addingFollow}
      >
        {#if addingFollow}
          <span class="btn-spinner"></span>
        {:else}
          Follow
        {/if}
      </button>
    </div>

    {#if showScanner}
      <ScannerModal
        onscanned={handleScanned}
        onclose={() => (showScanner = false)}
      />
    {/if}

    <div class="follow-list">
      {#each follows as f (f.pubkey)}
        <PersonItem pubkey={f.pubkey}>
          {#snippet actions()}
            <button
              class="btn-elevated"
              title="Copy public key"
              onclick={() => copyFb.copy(f.pubkey, f.pubkey)}
            >
              {copyFb.feedback === f.pubkey ? "Copied!" : "Copy key"}
            </button>
            <button
              class="btn-moderation danger"
              onclick={() => (pendingUnfollowPubkey = f.pubkey)}
            >
              Unfollow
            </button>
          {/snippet}
        </PersonItem>
      {:else}
        <p class="empty">
          Not following anyone yet. Paste a Node ID above to follow someone!
        </p>
      {/each}
    </div>
  {:else if activeTab === "followers"}
    <div class="follow-list">
      {#each followers as f (f.pubkey)}
        <PersonItem
          pubkey={f.pubkey}
          showOnlineStatus={true}
          isOnline={f.is_online}
        >
          {#snippet actions()}
            <button
              class="btn-elevated"
              title="Copy public key"
              onclick={() => copyFb.copy(f.pubkey, f.pubkey)}
            >
              {copyFb.feedback === f.pubkey ? "Copied!" : "Copy key"}
            </button>
          {/snippet}
        </PersonItem>
      {:else}
        <p class="empty">
          No followers yet. Share your Node ID for others to follow you!
        </p>
      {/each}
    </div>
  {:else if activeTab === "requests"}
    <div class="follow-list">
      {#each pendingRequests as req (req.pubkey)}
        <PersonItem pubkey={req.pubkey}>
          {#snippet actions()}
            <button
              class="btn-approve"
              onclick={() => approveRequest(req.pubkey)}
            >
              Approve
            </button>
            <button
              class="btn-moderation danger"
              onclick={() => denyRequest(req.pubkey)}
            >
              Deny
            </button>
          {/snippet}
        </PersonItem>
      {:else}
        <p class="empty">No pending follow requests.</p>
      {/each}
    </div>

    {#if resolvedRequests.length > 0}
      <details class="moderation-section">
        <summary class="moderation-header resolved">
          Resolved ({resolvedRequests.length})
        </summary>
        <div class="follow-list">
          {#each resolvedRequests as req (req.pubkey)}
            <PersonItem pubkey={req.pubkey}>
              {#snippet actions()}
                <span
                  class="request-status"
                  class:approved={req.status === "approved"}
                  class:denied={req.status === "denied"}
                >
                  {req.status}
                </span>
              {/snippet}
            </PersonItem>
          {/each}
        </div>
      </details>
    {/if}
  {/if}

  {#if mutedPubkeys.length > 0}
    <details class="moderation-section">
      <summary class="moderation-header muted">
        Muted ({mutedPubkeys.length})
      </summary>
      <div class="follow-list">
        {#each mutedPubkeys as pubkey (pubkey)}
          <PersonItem {pubkey}>
            {#snippet actions()}
              <button
                class="btn-moderation warn"
                onclick={() => unmute(pubkey)}
              >
                Unmute
              </button>
            {/snippet}
          </PersonItem>
        {/each}
      </div>
    </details>
  {/if}

  {#if blockedPubkeys.length > 0}
    <details class="moderation-section">
      <summary class="moderation-header blocked">
        Blocked ({blockedPubkeys.length})
      </summary>
      <div class="follow-list">
        {#each blockedPubkeys as pubkey (pubkey)}
          <PersonItem {pubkey}>
            {#snippet actions()}
              <button
                class="btn-moderation danger"
                onclick={() => unblock(pubkey)}
              >
                Unblock
              </button>
            {/snippet}
          </PersonItem>
        {/each}
      </div>
    </details>
  {/if}
{/if}

{#if pendingUnfollowPubkey}
  <DeleteConfirmModal
    message="Unfollow this user? Their posts will be deleted from your device."
    confirmLabel="Unfollow"
    onconfirm={executeUnfollow}
    oncancel={() => (pendingUnfollowPubkey = null)}
  />
{/if}

{#if scanned}
  {@const knownName = getCachedDisplayName(scanned.pubkey)}
  <ConfirmDialog
    label="Confirm follow"
    confirmLabel="Follow"
    danger={false}
    wide
    onconfirm={confirmScanned}
    oncancel={() => (scanned = null)}
  >
    <p>Follow the user with this public key?</p>
    {#if knownName}
      <p class="scanned-name">{knownName}</p>
    {/if}
    <code class="scanned-key">{scanned.pubkey}</code>
    <p class="scanned-hint">
      Compare it with the key the user shows on their profile. The follow is
      rejected if the scanned device reports another key.
    </p>
  </ConfirmDialog>
{/if}

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

<style>
  .tabs {
    display: flex;
    margin-bottom: 1rem;
    border-bottom: 1px solid var(--border);
  }

  .tab {
    flex: 1;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-secondary);
    font-size: var(--text-base);
    font-weight: 600;
    padding: 0.75rem;
    cursor: pointer;
    transition:
      color var(--transition-normal),
      border-color var(--transition-normal);
  }

  .tab:hover {
    color: var(--accent-light);
  }

  .tab.active {
    color: var(--accent-medium);
    border-bottom-color: var(--accent-medium);
  }

  .add-follow {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .add-follow input {
    flex: 1;
  }

  .follow-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.6rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    min-width: 72px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .follow-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .scan-btn {
    background: var(--bg-elevated);
    color: var(--accent-light);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.6rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
  }

  .scan-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .scanned-name {
    font-weight: 600;
  }

  .scanned-key {
    display: block;
    margin: 0 0 1rem;
    padding: 0.5rem;
    background: var(--bg-elevated);
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    word-break: break-all;
  }

  .scanned-hint {
    color: var(--text-secondary);
    font-size: var(--text-sm);
  }

  .moderation-section {
    margin-top: 1.5rem;
    border-top: 1px solid var(--border);
    padding-top: 0.75rem;
  }

  .moderation-header {
    cursor: pointer;
    font-size: var(--text-base);
    font-weight: 600;
    padding: 0.4rem 0;
    list-style: none;
    user-select: none;
  }

  .moderation-header::-webkit-details-marker {
    display: none;
  }

  .moderation-header::before {
    content: "\25B6";
    display: inline-block;
    margin-right: 0.4rem;
    font-size: var(--text-xs);
    transition: transform var(--transition-fast);
  }

  details[open] > .moderation-header::before {
    transform: rotate(90deg);
  }

  .moderation-header.muted {
    color: var(--color-warning);
  }

  .moderation-header.blocked {
    color: var(--color-error-light);
  }

  .btn-approve {
    background: var(--color-success);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.35rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
  }

  .btn-approve:hover {
    filter: brightness(1.1);
  }

  .request-badge {
    background: var(--accent);
    color: var(--text-on-accent);
    font-size: var(--text-xs);
    font-weight: 700;
    border-radius: 999px;
    padding: 0.1rem 0.45rem;
    margin-left: 0.35rem;
    vertical-align: middle;
  }

  .request-status {
    font-size: var(--text-sm);
    font-weight: 600;
    text-transform: capitalize;
  }

  .request-status.approved {
    color: var(--color-success, #22c55e);
  }

  .request-status.denied {
    color: var(--color-error-light, #ef4444);
  }

  .moderation-header.resolved {
    color: var(--text-secondary);
  }
</style>
