<script lang="ts">
  import { goto } from "$app/navigation";
  import Icon from "$lib/Icon.svelte";
  import { shortId } from "$lib/utils";

  export interface StageAnnouncement {
    stage_id: string;
    title: string;
    ticket: string;
    host_pubkey: string;
    started_at: number;
  }

  let { announcement }: { announcement: StageAnnouncement } = $props();

  function elapsedLabel(startedAt: number): string {
    const secs = Math.floor((Date.now() - startedAt) / 1000);
    if (secs < 60) return "just started";
    const mins = Math.floor(secs / 60);
    if (mins < 60) return `${mins}m`;
    const hrs = Math.floor(mins / 60);
    return `${hrs}h ${mins % 60}m`;
  }

  function joinStage() {
    goto(`/stage?ticket=${encodeURIComponent(announcement.ticket)}`);
  }
</script>

<div class="stage-card">
  <div class="stage-left">
    <div class="live-badge">
      <Icon name="radio" size={11} />
      LIVE
    </div>
    <div class="stage-info">
      <span class="stage-title">{announcement.title}</span>
      <span class="stage-meta">
        {shortId(announcement.host_pubkey)} &middot; {elapsedLabel(
          announcement.started_at,
        )}
      </span>
    </div>
  </div>
  <button class="btn-join" onclick={joinStage}>Join</button>
</div>

<style>
  .stage-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--accent-medium);
    border-radius: var(--radius-2xl);
    padding: 0.75rem 1rem;
    margin-bottom: 0.4rem;
    animation: fadeIn var(--transition-slow) ease-out;
  }

  .stage-left {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    min-width: 0;
  }

  .live-badge {
    display: flex;
    align-items: center;
    gap: 0.25rem;
    background: var(--color-error, #e53e3e);
    color: #fff;
    font-size: 0.65rem;
    font-weight: 700;
    letter-spacing: 0.06em;
    padding: 0.2rem 0.45rem;
    border-radius: var(--radius-sm);
    flex-shrink: 0;
  }

  .stage-info {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
  }

  .stage-title {
    font-weight: 600;
    font-size: var(--text-base);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .stage-meta {
    font-size: var(--text-sm);
    color: var(--text-tertiary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .btn-join {
    flex-shrink: 0;
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-lg);
    padding: 0.4rem 1rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .btn-join:hover {
    background: var(--accent-hover);
  }
</style>
