<script lang="ts">
  import type { ServerSearchPost } from "$lib/types";

  let {
    posts,
    searchQuery,
    userNames,
  }: {
    posts: ServerSearchPost[];
    searchQuery: string;
    userNames: Map<string, string>;
  } = $props();

  function shortId(id: string): string {
    return id.length > 12 ? id.slice(0, 6) + ".." + id.slice(-4) : id;
  }

  function formatTime(ts: number): string {
    return new Date(ts).toLocaleDateString();
  }
</script>

{#if posts.length > 0}
  <div class="post-list">
    {#each posts as post (post.id)}
      <div class="feed-post">
        <div class="post-meta">
          <a href="/profile/{post.author}" class="post-author">
            {userNames.get(post.author) || shortId(post.author)}
          </a>
          <span class="post-time">{formatTime(post.timestamp)}</span>
        </div>
        <p class="post-content">{post.content}</p>
      </div>
    {/each}
  </div>
{:else}
  <p class="empty">
    {searchQuery.trim() ? "No posts found." : "Search for posts above."}
  </p>
{/if}

<style>
  .post-list {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .feed-post {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0.75rem 1rem;
  }

  .post-meta {
    display: flex;
    justify-content: space-between;
    margin-bottom: 0.35rem;
    font-size: var(--text-sm);
  }

  .post-author {
    color: var(--accent-light);
    text-decoration: none;
    font-weight: 600;
  }

  .post-author:hover {
    text-decoration: underline;
  }

  .post-time {
    color: var(--text-muted);
  }

  .post-content {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .empty {
    text-align: center;
    color: var(--text-muted);
    padding: 2rem 0;
  }
</style>
