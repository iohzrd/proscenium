<script lang="ts">
  import { profileHref } from "#lib/profile/links.ts";
  import type { ServerSearchPost } from "#lib/types.ts";
  import { shortId } from "#lib/utils.ts";

  let {
    posts,
    searchQuery,
    userNames,
  }: {
    posts: ServerSearchPost[];
    searchQuery: string;
    userNames: Map<string, string>;
  } = $props();

  function formatTime(ts: number): string {
    return new Date(ts).toLocaleDateString();
  }
</script>

{#if posts.length > 0}
  <!-- Not keyed: the server can return the same id twice. -->
  <div class="post-list">
    {#each posts as post}
      <div class="feed-post">
        <div class="post-meta">
          <span class="post-byline">
            <a href={profileHref(post.author)} class="post-author">
              {userNames.get(post.author) || shortId(post.author)}
            </a>
          </span>
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
  }

  .feed-post {
    padding: 0.85rem 0.25rem;
    border-bottom: 1px solid var(--border);
  }

  .post-meta {
    display: flex;
    justify-content: space-between;
    gap: 0.5rem;
    margin-bottom: 0.35rem;
    font-size: var(--text-sm);
  }

  .post-author {
    font-family: var(--font-display);
    font-optical-sizing: auto;
    font-size: var(--text-base);
    color: var(--text-primary);
    text-decoration: none;
    font-weight: 600;
  }

  .post-author:hover {
    text-decoration: underline;
  }

  .post-byline {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    min-width: 0;
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
