<script lang="ts">
  import Avatar from "$lib/Avatar.svelte";
  import Icon from "$lib/Icon.svelte";
  import { focusTrap } from "$lib/composables";
  import type { StageParticipant } from "$lib/types";
  import { shortId, truncate } from "$lib/utils";

  const MAX_NAME_CHARS = 64;

  let {
    target,
    onpromote,
    ondemote,
    onmute,
    onkick,
    onban,
    onclose,
  }: {
    target: StageParticipant;
    onpromote: (pubkey: string) => void;
    ondemote: (pubkey: string) => void;
    /** Mutes the microphone of a speaker for all listeners. */
    onmute?: (pubkey: string) => void;
    /** Removes the participant from the stage. */
    onkick?: (pubkey: string) => void;
    /** Removes the participant and rejects every later join. */
    onban?: (pubkey: string) => void;
    onclose: () => void;
  } = $props();

  // `onban` is called on the second click of the ban button.
  let confirmingBan = $state(false);

  let name = $derived(
    truncate(target.display_name ?? shortId(target.pubkey), MAX_NAME_CHARS),
  );
  let canSpeak = $derived(
    target.role === "Speaker" || target.role === "CoHost",
  );

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<button
  class="popover-backdrop"
  onclick={onclose}
  aria-label="Close"
  tabindex="-1"
></button>
<div
  class="participant-popover"
  role="dialog"
  aria-modal="true"
  aria-label="Actions for {name}"
  tabindex="-1"
  use:focusTrap
>
  <div class="popover-header">
    <Avatar pubkey={target.pubkey} {name} size={40} />
    <span class="popover-name">{name}</span>
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
    {#if onmute && canSpeak}
      <button
        class="popover-btn"
        onclick={() => onmute?.(target.pubkey)}
        disabled={target.host_muted}
      >
        {target.host_muted ? "Muted by host" : "Mute microphone"}
      </button>
    {/if}
    {#if target.role !== "Host"}
      {#if onkick}
        <button
          class="popover-btn danger"
          onclick={() => onkick?.(target.pubkey)}
        >
          Remove from stage
        </button>
      {/if}
      {#if onban}
        {#if confirmingBan}
          <button
            class="popover-btn danger"
            onclick={() => onban?.(target.pubkey)}
          >
            Confirm ban: cannot rejoin this stage
          </button>
        {:else}
          <button
            class="popover-btn danger"
            onclick={() => (confirmingBan = true)}
          >
            Ban from stage
          </button>
        {/if}
      {/if}
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

  .participant-popover:focus {
    outline: none;
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
    min-width: 0;
    overflow-wrap: anywhere;
    padding-right: 1.25rem;
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

  .popover-btn:hover:not(:disabled) {
    background: var(--bg-elevated-hover);
  }

  .popover-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .popover-btn.danger {
    color: var(--color-error-light);
    border-color: var(--color-error-light-border);
  }

  .popover-btn.danger:hover {
    background: var(--color-error-bg-hover);
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
