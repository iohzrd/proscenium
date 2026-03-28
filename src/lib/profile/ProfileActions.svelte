<script lang="ts">
  let {
    pubkey,
    isFollowing,
    toggling,
    isBlocked,
    isMuted,
    togglingMute,
    togglingBlock,
    onToggleFollow,
    onToggleMute,
    onToggleBlock,
    onStartCall,
  }: {
    pubkey: string;
    isFollowing: boolean;
    toggling: boolean;
    isBlocked: boolean;
    isMuted: boolean;
    togglingMute: boolean;
    togglingBlock: boolean;
    onToggleFollow: () => void;
    onToggleMute: () => void;
    onToggleBlock: () => void;
    onStartCall: () => void;
  } = $props();
</script>

<div class="action-row">
  <button
    class="follow-toggle"
    class:following={isFollowing}
    onclick={onToggleFollow}
    disabled={toggling || isBlocked}
  >
    {#if toggling}<span class="btn-spinner"></span>{:else}{isFollowing
        ? "Unfollow"
        : "Follow"}{/if}
  </button>
  <a href="/messages/{pubkey}" class="message-btn">Message</a>
  <button class="call-btn" onclick={onStartCall} disabled={isBlocked}>
    Call
  </button>
</div>
<div class="moderation-row">
  <button
    class="mod-btn mute"
    class:active={isMuted}
    onclick={onToggleMute}
    disabled={togglingMute}
  >
    {#if togglingMute}<span class="btn-spinner"></span>{:else}{isMuted
        ? "Unmute"
        : "Mute"}{/if}
  </button>
  <button
    class="mod-btn block"
    class:active={isBlocked}
    onclick={onToggleBlock}
    disabled={togglingBlock}
  >
    {#if togglingBlock}<span class="btn-spinner"></span>{:else}{isBlocked
        ? "Unblock"
        : "Block"}{/if}
  </button>
</div>

<style>
  .action-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .follow-toggle {
    flex: 1;
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 2.2rem;
  }

  .message-btn {
    background: var(--bg-elevated);
    color: var(--accent-light);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    text-decoration: none;
    text-align: center;
    transition: background var(--transition-normal);
  }

  .message-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .call-btn {
    background: var(--color-success);
    color: white;
    border: none;
    border-radius: var(--radius-md);
    padding: 0.5rem 1rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-normal);
  }

  .call-btn:hover:not(:disabled) {
    background: #16a34a;
  }

  .call-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .follow-toggle:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .follow-toggle.following {
    background: transparent;
    color: var(--color-error-light);
    border: 1px solid var(--color-error-light-border);
  }

  .follow-toggle.following:hover:not(:disabled) {
    background: var(--color-error-light-bg);
  }

  .moderation-row {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
  }

  .mod-btn {
    flex: 1;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.35rem;
    font-size: var(--text-base);
    font-weight: 500;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 1.8rem;
    transition:
      color var(--transition-fast),
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .mod-btn.mute {
    color: var(--text-secondary);
  }

  .mod-btn.mute:hover:not(:disabled) {
    color: var(--color-warning);
    border-color: var(--color-warning-border);
    background: var(--color-warning-bg-subtle);
  }

  .mod-btn.mute.active {
    color: var(--color-warning);
    border-color: var(--color-warning-border);
  }

  .mod-btn.mute.active:hover:not(:disabled) {
    background: var(--color-warning-bg-subtle);
  }

  .mod-btn.block {
    color: var(--text-secondary);
  }

  .mod-btn.block:hover:not(:disabled) {
    color: var(--color-error);
    border-color: var(--color-error-border);
    background: var(--color-error-bg-subtle);
  }

  .mod-btn.block.active {
    color: var(--color-error);
    border-color: var(--color-error-border);
  }

  .mod-btn.block.active:hover:not(:disabled) {
    background: var(--color-error-bg-subtle);
  }
</style>
