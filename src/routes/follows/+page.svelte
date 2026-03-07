<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { platform } from "@tauri-apps/plugin-os";
  import { onMount } from "svelte";
  import Avatar from "$lib/Avatar.svelte";
  import ScannerModal from "$lib/ScannerModal.svelte";
  import { hapticImpact } from "$lib/haptics";
  import type { FollowEntry, FollowerEntry } from "$lib/types";
  import { shortId, getDisplayName, getCachedAvatarTicket } from "$lib/utils";
  import {
    useNodeInit,
    useEventListeners,
    useCopyFeedback,
  } from "$lib/composables.svelte";

  let follows = $state<FollowEntry[]>([]);
  let followers = $state<FollowerEntry[]>([]);
  let mutedPubkeys = $state<string[]>([]);
  let blockedPubkeys = $state<string[]>([]);
  let newPubkey = $state("");
  let status = $state("");
  let addingFollow = $state(false);
  let pendingUnfollowPubkey = $state<string | null>(null);
  let activeTab = $state<"following" | "followers">("following");
  let editingAlias = $state<string | null>(null);
  let aliasInput = $state("");
  const isMobile = platform() === "android" || platform() === "ios";
  let showScanner = $state(false);

  const copyFb = useCopyFeedback();

  const node = useNodeInit(async () => {
    await Promise.all([
      loadFollows(),
      loadFollowers(),
      loadMuted(),
      loadBlocked(),
    ]);
  });

  async function loadFollows() {
    try {
      follows = await invoke("get_follows");
    } catch (e) {
      console.error("Failed to load follows:", e);
    }
  }

  async function loadFollowers() {
    try {
      followers = await invoke("get_followers");
    } catch (e) {
      console.error("Failed to load followers:", e);
    }
  }

  async function loadMuted() {
    try {
      mutedPubkeys = await invoke("get_muted_pubkeys");
    } catch (e) {
      console.error("Failed to load muted:", e);
    }
  }

  async function loadBlocked() {
    try {
      blockedPubkeys = await invoke("get_blocked_pubkeys");
    } catch (e) {
      console.error("Failed to load blocked:", e);
    }
  }

  async function unmute(pubkey: string) {
    try {
      await invoke("unmute_user", { pubkey });
      await loadMuted();
    } catch (e) {
      status = `Error: ${e}`;
    }
  }

  async function unblock(pubkey: string) {
    try {
      await invoke("unblock_user", { pubkey });
      await loadBlocked();
    } catch (e) {
      status = `Error: ${e}`;
    }
  }

  async function followUser() {
    const pubkey = newPubkey.trim();
    if (!pubkey) return;
    addingFollow = true;
    status = "";
    try {
      await invoke("follow_user", { pubkey });
      newPubkey = "";
      await loadFollows();
      hapticImpact("light");
      status = "Followed!";
      setTimeout(() => (status = ""), 2000);
    } catch (e) {
      status = `Error: ${e}`;
    }
    addingFollow = false;
  }

  function confirmUnfollow(pubkey: string) {
    pendingUnfollowPubkey = pubkey;
  }

  async function executeUnfollow() {
    if (!pendingUnfollowPubkey) return;
    try {
      await invoke("unfollow_user", { pubkey: pendingUnfollowPubkey });
      await loadFollows();
      hapticImpact("light");
    } catch (e) {
      status = `Error: ${e}`;
    }
    pendingUnfollowPubkey = null;
  }

  function cancelUnfollow() {
    pendingUnfollowPubkey = null;
  }

  function handleKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      followUser();
    }
  }

  function handleGlobalKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      if (pendingUnfollowPubkey) cancelUnfollow();
      else if (editingAlias) editingAlias = null;
      else if (showScanner) showScanner = false;
    }
  }

  async function saveAlias() {
    if (!editingAlias) return;
    try {
      const alias = aliasInput.trim() || null;
      await invoke("update_follow_alias", { pubkey: editingAlias, alias });
      await loadFollows();
    } catch (e) {
      status = `Error: ${e}`;
    }
    editingAlias = null;
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
  </div>

  {#if activeTab === "following"}
    <div class="add-follow">
      <input
        class="input-base"
        bind:value={newPubkey}
        placeholder="Paste a Node ID to follow..."
        onkeydown={handleKey}
      />
      {#if isMobile}
        <button class="scan-btn" onclick={() => (showScanner = true)}
          >Scan</button
        >
      {/if}
      <button
        class="follow-btn"
        onclick={followUser}
        disabled={!newPubkey.trim() || addingFollow}
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
        onscanned={(nodeId) => {
          showScanner = false;
          newPubkey = nodeId;
          followUser();
        }}
        onclose={() => (showScanner = false)}
      />
    {/if}

    {#if status}
      <p class="status">{status}</p>
    {/if}

    {#if pendingUnfollowPubkey}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <div class="modal-overlay" onclick={cancelUnfollow} role="presentation">
        <!-- svelte-ignore a11y_interactive_supports_focus -->
        <div
          class="modal"
          onclick={(e) => e.stopPropagation()}
          role="dialog"
          aria-label="Confirm unfollow"
        >
          <p>Unfollow this user? You will stop receiving their posts.</p>
          <div class="modal-actions">
            <button class="modal-cancel" onclick={cancelUnfollow}>Cancel</button
            >
            <button class="modal-confirm" onclick={executeUnfollow}
              >Unfollow</button
            >
          </div>
        </div>
      </div>
    {/if}

    <div class="follow-list">
      {#each follows as f (f.pubkey)}
        <div class="follow-item">
          <a href="/profile/{f.pubkey}" class="follow-info">
            {#await getDisplayName(f.pubkey, "") then name}
              <Avatar
                pubkey={f.pubkey}
                {name}
                ticket={getCachedAvatarTicket(f.pubkey)}
              />
              <div class="follow-identity">
                {#if f.alias}
                  <span class="display-name">{f.alias}</span>
                {:else if name !== shortId(f.pubkey)}
                  <span class="display-name">{name}</span>
                {/if}
                <code>{shortId(f.pubkey)}</code>
              </div>
            {/await}
          </a>
          <div class="follow-actions">
            <button
              class="btn-elevated"
              onclick={(e) => {
                e.preventDefault();
                editingAlias = f.pubkey;
                aliasInput = f.alias ?? "";
              }}
            >
              {f.alias ? "Edit alias" : "Set alias"}
            </button>
            <button
              class="btn-elevated"
              onclick={() => copyFb.copy(f.pubkey, f.pubkey)}
            >
              {copyFb.feedback === f.pubkey ? "Copied!" : "Copy"}
            </button>
            <button
              class="btn-moderation danger"
              onclick={() => confirmUnfollow(f.pubkey)}
            >
              Unfollow
            </button>
          </div>
        </div>
      {:else}
        <p class="empty">
          Not following anyone yet. Paste a Node ID above to follow someone!
        </p>
      {/each}
    </div>

    {#if editingAlias}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <div
        class="modal-overlay"
        onclick={() => (editingAlias = null)}
        role="presentation"
      >
        <!-- svelte-ignore a11y_interactive_supports_focus -->
        <div
          class="modal"
          onclick={(e) => e.stopPropagation()}
          role="dialog"
          aria-label="Set alias"
        >
          <p>Set a local alias for this user</p>
          <input
            class="input-base alias-input"
            bind:value={aliasInput}
            placeholder="Alias (leave empty to clear)"
            onkeydown={(e) => {
              if (e.key === "Enter") saveAlias();
            }}
          />
          <div class="modal-actions">
            <button class="modal-cancel" onclick={() => (editingAlias = null)}
              >Cancel</button
            >
            <button class="modal-confirm save" onclick={saveAlias}>Save</button>
          </div>
        </div>
      </div>
    {/if}
  {:else}
    <div class="follow-list">
      {#each followers as f (f.pubkey)}
        <div class="follow-item">
          <a href="/profile/{f.pubkey}" class="follow-info">
            {#await getDisplayName(f.pubkey, "") then name}
              <Avatar
                pubkey={f.pubkey}
                {name}
                ticket={getCachedAvatarTicket(f.pubkey)}
              />
              <div class="follow-identity">
                {#if name !== shortId(f.pubkey)}
                  <span class="display-name">{name}</span>
                {/if}
                <code>{shortId(f.pubkey)}</code>
                <span class="online-status" class:online={f.is_online}>
                  {f.is_online ? "online" : "offline"}
                </span>
              </div>
            {/await}
          </a>
          <div class="follow-actions">
            <button
              class="btn-elevated"
              onclick={() => copyFb.copy(f.pubkey, f.pubkey)}
            >
              {copyFb.feedback === f.pubkey ? "Copied!" : "Copy"}
            </button>
          </div>
        </div>
      {:else}
        <p class="empty">
          No followers yet. Share your Node ID for others to follow you!
        </p>
      {/each}
    </div>
  {/if}

  {#if mutedPubkeys.length > 0}
    <details class="moderation-section">
      <summary class="moderation-header muted">
        Muted ({mutedPubkeys.length})
      </summary>
      <div class="follow-list">
        {#each mutedPubkeys as pubkey (pubkey)}
          <div class="follow-item">
            <a href="/profile/{pubkey}" class="follow-info">
              {#await getDisplayName(pubkey, "") then name}
                <Avatar
                  {pubkey}
                  {name}
                  ticket={getCachedAvatarTicket(pubkey)}
                />
                <div class="follow-identity">
                  {#if name !== shortId(pubkey)}
                    <span class="display-name">{name}</span>
                  {/if}
                  <code>{shortId(pubkey)}</code>
                </div>
              {/await}
            </a>
            <div class="follow-actions">
              <button
                class="btn-moderation warn"
                onclick={() => unmute(pubkey)}
              >
                Unmute
              </button>
            </div>
          </div>
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
          <div class="follow-item">
            <a href="/profile/{pubkey}" class="follow-info">
              {#await getDisplayName(pubkey, "") then name}
                <Avatar
                  {pubkey}
                  {name}
                  ticket={getCachedAvatarTicket(pubkey)}
                />
                <div class="follow-identity">
                  {#if name !== shortId(pubkey)}
                    <span class="display-name">{name}</span>
                  {/if}
                  <code>{shortId(pubkey)}</code>
                </div>
              {/await}
            </a>
            <div class="follow-actions">
              <button
                class="btn-moderation danger"
                onclick={() => unblock(pubkey)}
              >
                Unblock
              </button>
            </div>
          </div>
        {/each}
      </div>
    </details>
  {/if}
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

  .online-status {
    font-size: var(--text-sm);
    color: var(--text-tertiary);
  }

  .online-status.online {
    color: var(--color-success);
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

  .follow-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-2xl);
    padding: 0.75rem 1rem;
    margin-bottom: 0.5rem;
    transition: border-color var(--transition-normal);
  }

  .follow-item:hover {
    border-color: var(--border-hover);
  }

  .follow-info {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    text-decoration: none;
    color: inherit;
    flex: 1;
    min-width: 0;
  }

  .follow-info:hover .display-name {
    text-decoration: underline;
  }

  .follow-identity {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }

  .display-name {
    font-weight: 600;
    color: var(--accent-light);
    font-size: var(--text-base);
  }

  code {
    color: var(--color-link);
    font-size: var(--text-base);
  }

  .follow-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-shrink: 0;
  }

  .status {
    text-align: center;
    color: var(--text-secondary);
    font-size: var(--text-base);
    margin: 0.5rem 0;
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

  .alias-input {
    margin-bottom: 1rem;
  }
</style>
