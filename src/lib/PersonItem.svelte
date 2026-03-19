<script lang="ts">
  import type { Snippet } from "svelte";
  import Avatar from "$lib/Avatar.svelte";
  import { shortId, getDisplayName, getCachedAvatarTicket } from "$lib/utils";

  let {
    pubkey,
    showOnlineStatus = false,
    isOnline = false,
    actions,
  }: {
    pubkey: string;
    showOnlineStatus?: boolean;
    isOnline?: boolean;
    actions?: Snippet;
  } = $props();
</script>

<div class="follow-item">
  <a href="/profile/{pubkey}" class="follow-info">
    {#await getDisplayName(pubkey, "") then name}
      <Avatar {pubkey} {name} ticket={getCachedAvatarTicket(pubkey)} />
      <div class="follow-identity">
        {#if name !== shortId(pubkey)}
          <span class="display-name">{name}</span>
        {/if}
        <code>{shortId(pubkey)}</code>
        {#if showOnlineStatus}
          <span class="online-status" class:online={isOnline}>
            {isOnline ? "online" : "offline"}
          </span>
        {/if}
      </div>
    {/await}
  </a>
  {#if actions}
    <div class="follow-actions">
      {@render actions()}
    </div>
  {/if}
</div>

<style>
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

  .online-status {
    font-size: var(--text-sm);
    color: var(--text-tertiary);
  }

  .online-status.online {
    color: var(--color-success);
  }
</style>
