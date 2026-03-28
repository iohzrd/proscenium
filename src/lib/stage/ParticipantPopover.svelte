<script lang="ts">
  import Avatar from "$lib/Avatar.svelte";
  import Icon from "$lib/Icon.svelte";
  import type { StageParticipant } from "$lib/types";
  import { shortId } from "$lib/utils";

  let {
    target,
    onpromote,
    ondemote,
    onclose,
  }: {
    target: StageParticipant;
    onpromote: (pubkey: string) => void;
    ondemote: (pubkey: string) => void;
    onclose: () => void;
  } = $props();

  function displayName(p: StageParticipant): string {
    return p.display_name ?? shortId(p.pubkey);
  }
</script>

<button class="popover-backdrop" onclick={onclose} aria-label="Close"></button>
<div class="participant-popover">
  <div class="popover-header">
    <Avatar pubkey={target.pubkey} name={displayName(target)} size={40} />
    <span class="popover-name">{displayName(target)}</span>
  </div>
  <div class="popover-actions">
    {#if target.role === "Listener"}
      <button class="popover-btn" onclick={() => onpromote(target.pubkey)}>
        Promote to Speaker
      </button>
    {:else if target.role === "Speaker"}
      <button class="popover-btn" onclick={() => ondemote(target.pubkey)}>
        Demote to Listener
      </button>
    {/if}
  </div>
  <button class="popover-close" onclick={onclose} aria-label="Close">
    <Icon name="x" size={16} />
  </button>
</div>

<style>
  .popover-backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
    background: transparent;
    border: none;
    padding: 0;
    cursor: default;
  }

  .participant-popover {
    position: fixed;
    bottom: 5rem;
    left: 50%;
    transform: translateX(-50%);
    z-index: 101;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 1rem 1.25rem;
    box-shadow: var(--shadow-md);
    min-width: 220px;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .popover-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .popover-name {
    font-weight: 600;
    color: var(--text-primary);
    font-size: var(--text-base);
  }

  .popover-actions {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .popover-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.5rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    text-align: left;
    color: var(--text-primary);
    transition: background var(--transition-fast);
  }

  .popover-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .popover-close {
    position: absolute;
    top: 0.5rem;
    right: 0.5rem;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    padding: 0.25rem;
    display: flex;
    align-items: center;
  }
</style>
