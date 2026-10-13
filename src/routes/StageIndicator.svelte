<script lang="ts">
  import Icon from "$lib/Icon.svelte";
  import {
    stageStore,
    selfMuted,
    hostMuted,
    toggleStageMute,
    leaveStage,
  } from "$lib/stage.svelte";

  let busy = $state(false);
  let error = $state("");

  let stage = $derived(stageStore.stage);
  let canSpeak = $derived(
    stage?.my_role === "Speaker" ||
      stage?.my_role === "Host" ||
      stage?.my_role === "CoHost",
  );
  // The backend ends the stage when its host leaves.
  let leaveLabel = $derived(stage?.my_role === "Host" ? "End" : "Leave");

  async function run(action: () => Promise<void>, failure: string) {
    if (busy) return;
    busy = true;
    error = "";
    try {
      await action();
    } catch (e) {
      error = `${failure}: ${e}`;
    }
    busy = false;
  }
</script>

{#if stage}
  <div class="stage-indicator" role="status">
    <span class="live-dot"></span>
    <a href="/stage" class="stage-link" title="Open stage">
      <span class="stage-title">{stage.title}</span>
      <span class="stage-role">{stage.my_role}</span>
    </a>
    {#if error}
      <span class="stage-error">{error}</span>
    {/if}
    {#if canSpeak}
      <button
        class="stage-btn"
        class:muted={selfMuted() || hostMuted()}
        onclick={() => run(toggleStageMute, "Mute failed")}
        disabled={busy || hostMuted()}
        title={hostMuted() ? "Muted by host" : undefined}
      >
        <Icon name={selfMuted() || hostMuted() ? "mic-off" : "mic"} size={14} />
        {#if hostMuted()}
          Muted by host
        {:else}
          {selfMuted() ? "Unmute" : "Mute"}
        {/if}
      </button>
    {/if}
    <button
      class="stage-btn leave"
      onclick={() => run(leaveStage, "Leave failed")}
      disabled={busy}
    >
      <Icon name="log-out" size={14} />
      {leaveLabel}
    </button>
  </div>
{/if}

<style>
  .stage-indicator {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 1rem;
    background: var(--bg-surface);
    border-bottom: 1px solid var(--border);
    font-size: var(--text-sm);
    /* Below the sticky mobile header, which is hidden from 768px. */
    position: sticky;
    top: var(--mobile-header-height);
    z-index: var(--z-nav);
  }

  @media (min-width: 768px) {
    .stage-indicator {
      top: 0;
    }
  }

  .live-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--color-error);
    flex-shrink: 0;
  }

  .stage-link {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    flex: 1;
    min-width: 0;
    color: var(--text-primary);
    text-decoration: none;
  }

  .stage-link:hover .stage-title {
    text-decoration: underline;
  }

  .stage-title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .stage-role {
    color: var(--text-muted);
    font-size: var(--text-xs);
    flex-shrink: 0;
  }

  .stage-error {
    color: var(--color-error-light);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40%;
  }

  .stage-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    background: var(--bg-elevated);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    padding: 0.25rem 0.7rem;
    font-size: var(--text-xs);
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .stage-btn:hover:not(:disabled) {
    background: var(--bg-elevated-hover);
  }

  .stage-btn.muted {
    color: var(--color-error-light);
    border-color: var(--color-error-light-border);
  }

  .stage-btn.leave {
    color: var(--color-error-light);
  }
</style>
