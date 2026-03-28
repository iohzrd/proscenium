<script lang="ts">
  import Icon from "$lib/Icon.svelte";

  let {
    isSpeaker,
    isHost,
    selfMuted,
    handRaised,
    quickReactions,
    ontoggleMute,
    ontoggleHand,
    onsendReaction,
    onleave,
    onend,
  }: {
    isSpeaker: boolean;
    isHost: boolean;
    selfMuted: boolean;
    handRaised: boolean;
    quickReactions: string[];
    ontoggleMute: () => void;
    ontoggleHand: () => void;
    onsendReaction: (emoji: string) => void;
    onleave: () => void;
    onend: () => void;
  } = $props();
</script>

<div class="room-controls">
  <div class="controls-left">
    {#if isSpeaker}
      <button
        class="ctrl-btn"
        class:ctrl-muted={selfMuted}
        onclick={ontoggleMute}
        title={selfMuted ? "Unmute" : "Mute"}
      >
        <Icon name={selfMuted ? "mic-off" : "mic"} size={18} />
        <span>{selfMuted ? "Unmute" : "Mute"}</span>
      </button>
    {:else}
      <button
        class="ctrl-btn"
        class:ctrl-active={handRaised}
        onclick={ontoggleHand}
        title={handRaised ? "Lower hand" : "Raise hand"}
      >
        <Icon name="hand" size={18} />
        <span>{handRaised ? "Lower Hand" : "Raise Hand"}</span>
      </button>
    {/if}
  </div>

  <div class="reactions-bar">
    {#each quickReactions as emoji}
      <button class="reaction-btn" onclick={() => onsendReaction(emoji)}
        >{emoji}</button
      >
    {/each}
  </div>

  <div class="controls-right">
    {#if isHost}
      <button class="ctrl-btn ctrl-danger" onclick={onend}>
        <Icon name="log-out" size={18} />
        <span>End Stage</span>
      </button>
    {:else}
      <button class="ctrl-btn ctrl-danger" onclick={onleave}>
        <Icon name="log-out" size={18} />
        <span>Leave</span>
      </button>
    {/if}
  </div>
</div>

<style>
  .room-controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.85rem 0 0;
    border-top: 1px solid var(--border);
    margin-top: 0.75rem;
    flex-shrink: 0;
    gap: 1rem;
  }

  .controls-left,
  .controls-right {
    flex: 0 0 auto;
  }

  .reactions-bar {
    display: flex;
    gap: 0.3rem;
    justify-content: center;
  }

  .reaction-btn {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    padding: 0.35rem 0.55rem;
    font-size: 1.1rem;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      transform var(--transition-fast);
    line-height: 1;
  }

  .reaction-btn:hover {
    background: var(--bg-elevated);
    transform: scale(1.2);
  }

  .ctrl-btn {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.5rem 1.1rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    color: var(--text-primary);
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
    white-space: nowrap;
  }

  .ctrl-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .ctrl-btn.ctrl-muted {
    background: var(--color-warning-bg);
    color: var(--color-warning);
    border-color: var(--color-warning-border);
  }

  .ctrl-btn.ctrl-active {
    background: var(--accent-light-hover-bg);
    color: var(--accent-light);
    border-color: var(--accent-light-faint);
  }

  .ctrl-btn.ctrl-danger {
    background: var(--color-error-bg-subtle);
    color: var(--color-error-light);
    border-color: var(--color-error-light-border);
  }

  .ctrl-btn.ctrl-danger:hover {
    background: var(--color-error-bg-hover);
  }

  @media (max-width: 640px) {
    .reactions-bar {
      gap: 0.2rem;
    }

    .reaction-btn {
      padding: 0.3rem 0.4rem;
      font-size: 1rem;
    }
  }
</style>
