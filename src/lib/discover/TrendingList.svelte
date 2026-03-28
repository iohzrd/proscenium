<script lang="ts">
  import type { TrendingHashtag } from "$lib/types";

  let {
    trending,
    loading,
    onsearchtag,
  }: {
    trending: TrendingHashtag[];
    loading: boolean;
    onsearchtag: (tag: string) => void;
  } = $props();
</script>

{#if loading}
  <div class="info-loading">
    <div class="spinner small"></div>
  </div>
{:else if trending.length > 0}
  <div class="trending-list">
    {#each trending as t, i (t.tag)}
      <button class="trending-item" onclick={() => onsearchtag(t.tag)}>
        <span class="trending-rank">{i + 1}</span>
        <div class="trending-info">
          <span class="trending-tag">#{t.tag}</span>
          <span class="trending-count">{t.post_count} posts</span>
        </div>
      </button>
    {/each}
  </div>
{:else}
  <p class="empty">No trending topics yet.</p>
{/if}

<style>
  .trending-list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .trending-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
    cursor: pointer;
    font-family: inherit;
    text-align: left;
    width: 100%;
    transition: border-color var(--transition-fast);
  }

  .trending-item:hover {
    border-color: var(--accent-medium);
  }

  .trending-rank {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--bg-elevated);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--text-sm);
    font-weight: 700;
    color: var(--accent-light);
    flex-shrink: 0;
  }

  .trending-info {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }

  .trending-tag {
    font-weight: 600;
    color: var(--accent-light);
  }

  .trending-count {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }

  .info-loading {
    display: flex;
    justify-content: center;
    padding: 2rem 0;
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2rem 0;
  }
</style>
