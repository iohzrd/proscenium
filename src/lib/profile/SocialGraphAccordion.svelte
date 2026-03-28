<script lang="ts">
  import Avatar from "$lib/Avatar.svelte";
  import type { RemoteSocialResult, RemoteFollowersResult } from "$lib/types";
  import { shortId } from "$lib/utils";

  let {
    remoteFollows,
    remoteFollowers,
  }: {
    remoteFollows: RemoteSocialResult | null;
    remoteFollowers: RemoteFollowersResult | null;
  } = $props();

  let showFollowsList = $state(false);
  let showFollowersList = $state(false);
</script>

{#if remoteFollows || remoteFollowers}
  <div class="social-graph">
    {#if remoteFollows}
      <button
        class="social-toggle"
        onclick={() => (showFollowsList = !showFollowsList)}
      >
        Following
        {#if remoteFollows.hidden}
          <span class="hidden-badge">Hidden</span>
        {:else}
          ({remoteFollows.follows.length})
        {/if}
        <span class="toggle-arrow">{showFollowsList ? "\u25BC" : "\u25B6"}</span
        >
      </button>
      {#if showFollowsList && !remoteFollows.hidden}
        <ul class="social-list">
          {#each remoteFollows.follows as f (f.pubkey)}
            <li>
              <a href="/profile/{f.pubkey}" class="social-link">
                <Avatar pubkey={f.pubkey} name={shortId(f.pubkey)} size={28} />
                <span class="social-name">{shortId(f.pubkey)}</span>
              </a>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}

    {#if remoteFollowers}
      <button
        class="social-toggle"
        onclick={() => (showFollowersList = !showFollowersList)}
      >
        Followers
        {#if remoteFollowers.hidden}
          <span class="hidden-badge">Hidden</span>
        {:else}
          ({remoteFollowers.followers.length})
        {/if}
        <span class="toggle-arrow"
          >{showFollowersList ? "\u25BC" : "\u25B6"}</span
        >
      </button>
      {#if showFollowersList && !remoteFollowers.hidden}
        <ul class="social-list">
          {#each remoteFollowers.followers as f (f.pubkey)}
            <li>
              <a href="/profile/{f.pubkey}" class="social-link">
                <Avatar pubkey={f.pubkey} name={shortId(f.pubkey)} size={28} />
                <span class="social-name">{shortId(f.pubkey)}</span>
              </a>
            </li>
          {/each}
        </ul>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .social-graph {
    margin: 1rem 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    overflow: hidden;
  }

  .social-toggle {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    padding: 0.75rem 1rem;
    background: var(--bg-surface);
    border: none;
    border-bottom: 1px solid var(--border);
    color: var(--text-primary);
    font-size: var(--text-base);
    font-weight: 500;
    cursor: pointer;
    text-align: left;
  }

  .social-toggle:last-child,
  .social-toggle + .social-list + .social-toggle {
    border-bottom: none;
  }

  .social-toggle:hover {
    background: var(--bg-deep);
  }

  .toggle-arrow {
    margin-left: auto;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .hidden-badge {
    font-size: var(--text-sm);
    color: var(--text-muted);
    font-weight: 400;
    font-style: italic;
  }

  .social-list {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 300px;
    overflow-y: auto;
    border-bottom: 1px solid var(--border);
  }

  .social-list li {
    border-bottom: 1px solid var(--border);
  }

  .social-list li:last-child {
    border-bottom: none;
  }

  .social-link {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.5rem 1rem;
    text-decoration: none;
    color: var(--text-primary);
  }

  .social-link:hover {
    background: var(--bg-deep);
  }

  .social-name {
    font-size: var(--text-sm);
  }
</style>
