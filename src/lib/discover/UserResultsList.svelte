<script lang="ts">
  import Icon from "$lib/Icon.svelte";
  import type { ServerUser } from "$lib/types";

  let {
    users,
    myPubkey,
    followedPubkeys,
    togglingFollow,
    ontogglefollow,
  }: {
    users: ServerUser[];
    myPubkey: string;
    followedPubkeys: Set<string>;
    togglingFollow: string | null;
    ontogglefollow: (pubkey: string) => void;
  } = $props();

  function shortId(id: string): string {
    return id.length > 12 ? id.slice(0, 6) + ".." + id.slice(-4) : id;
  }
</script>

{#if users.length > 0}
  <div class="user-list">
    {#each users as user (user.pubkey)}
      <div class="user-card">
        <a href="/profile/{user.pubkey}" class="user-card-link">
          <div class="user-avatar">
            <Icon name="user" size={24} />
          </div>
          <div class="user-info">
            <span class="user-name">
              {user.display_name || shortId(user.pubkey)}
            </span>
            <span class="user-pubkey">{shortId(user.pubkey)}</span>
            {#if user.bio}
              <span class="user-bio">{user.bio}</span>
            {/if}
          </div>
          <div class="user-stats">
            <span class="user-stat">{user.post_count} posts</span>
          </div>
        </a>
        {#if user.pubkey !== myPubkey}
          <button
            class="follow-btn"
            class:following={followedPubkeys.has(user.pubkey)}
            onclick={() => ontogglefollow(user.pubkey)}
            disabled={togglingFollow === user.pubkey}
          >
            {#if togglingFollow === user.pubkey}
              ...
            {:else if followedPubkeys.has(user.pubkey)}
              Following
            {:else}
              Follow
            {/if}
          </button>
        {/if}
      </div>
    {/each}
  </div>
{:else}
  <p class="empty">No users found.</p>
{/if}

<style>
  .user-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .user-card {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
    transition: border-color var(--transition-fast);
  }

  .user-card:hover {
    border-color: var(--accent-medium);
  }

  .user-card-link {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.75rem;
    text-decoration: none;
    color: inherit;
    min-width: 0;
  }

  .follow-btn {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.35rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    font-family: inherit;
    flex-shrink: 0;
  }

  .follow-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .follow-btn.following {
    background: transparent;
    color: var(--text-secondary);
    border: 1px solid var(--border);
  }

  .follow-btn.following:hover:not(:disabled) {
    color: var(--color-error, #ef4444);
    border-color: var(--color-error, #ef4444);
  }

  .follow-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .user-avatar {
    width: 40px;
    height: 40px;
    border-radius: 50%;
    background: var(--bg-elevated);
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .user-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 0;
  }

  .user-name {
    font-weight: 600;
    font-size: var(--text-base);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .user-pubkey {
    font-size: var(--text-xs);
    color: var(--text-muted);
    font-family: monospace;
  }

  .user-bio {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .user-stats {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    flex-shrink: 0;
  }

  .user-stat {
    font-size: var(--text-xs);
    color: var(--text-muted);
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2rem 0;
  }
</style>
