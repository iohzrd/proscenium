<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import type { CallState } from "$lib/types";
  import { shortId, getDisplayName } from "$lib/utils";
  import Avatar from "$lib/Avatar.svelte";

  let {
    callId,
    peerPubkey,
    callState,
    selfId,
  }: {
    callId: string;
    peerPubkey: string;
    callState: CallState;
    selfId: string;
  } = $props();

  let muted = $state(false);
  let duration = $state(0);
  let durationInterval: ReturnType<typeof setInterval> | null = null;
  let peerName = $state("");

  $effect(() => {
    getDisplayName(peerPubkey, selfId).then((name) => (peerName = name));
  });

  $effect(() => {
    if (callState === "active") {
      duration = 0;
      durationInterval = setInterval(() => (duration += 1), 1000);
    } else {
      if (durationInterval) {
        clearInterval(durationInterval);
        durationInterval = null;
      }
    }
    return () => {
      if (durationInterval) clearInterval(durationInterval);
    };
  });

  function formatDuration(seconds: number): string {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}:${s.toString().padStart(2, "0")}`;
  }

  async function accept() {
    try {
      await invoke("accept_call", { callId });
    } catch (e) {
      console.error("Failed to accept call:", e);
    }
  }

  async function reject() {
    try {
      await invoke("reject_call", { callId });
    } catch (e) {
      console.error("Failed to reject call:", e);
    }
  }

  async function hangup() {
    try {
      await invoke("hangup_call");
    } catch (e) {
      console.error("Failed to hang up:", e);
    }
  }

  async function toggleMute() {
    try {
      muted = await invoke("toggle_mute_call");
    } catch (e) {
      console.error("Failed to toggle mute:", e);
    }
  }
</script>

<div class="call-overlay">
  <div class="call-card">
    <div class="call-avatar">
      <Avatar pubkey={peerPubkey} name={peerName} size={72} />
    </div>
    <div class="call-peer-name">{peerName}</div>

    {#if callState === "ringing"}
      <div class="call-status">Calling...</div>
      <div class="call-actions">
        <button class="call-btn hangup" onclick={hangup}>Cancel</button>
      </div>
    {:else if callState === "incoming"}
      <div class="call-status">Incoming call</div>
      <div class="call-actions">
        <button class="call-btn accept" onclick={accept}>Answer</button>
        <button class="call-btn hangup" onclick={reject}>Decline</button>
      </div>
    {:else if callState === "active"}
      <div class="call-status">{formatDuration(duration)}</div>
      <div class="call-actions">
        <button class="call-btn mute" class:muted onclick={toggleMute}>
          {muted ? "Unmute" : "Mute"}
        </button>
        <button class="call-btn hangup" onclick={hangup}>Hang up</button>
      </div>
    {:else if callState === "failed"}
      <div class="call-status call-failed">Call failed</div>
    {/if}
  </div>
</div>

<style>
  .call-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.75);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 9999;
    animation: fadeIn 0.2s ease-out;
  }

  .call-card {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 2rem;
    min-width: 280px;
    max-width: 360px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1rem;
    box-shadow: var(--shadow-md);
  }

  .call-avatar {
    margin-bottom: 0.25rem;
  }

  .call-peer-name {
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--text-primary);
    text-align: center;
    word-break: break-all;
  }

  .call-status {
    font-size: var(--text-base);
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
  }

  .call-failed {
    color: var(--color-error);
  }

  .call-actions {
    display: flex;
    gap: 1rem;
    margin-top: 0.5rem;
  }

  .call-btn {
    padding: 0.75rem 1.5rem;
    border: none;
    border-radius: var(--radius-lg);
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      transform var(--transition-fast);
    min-width: 100px;
  }

  .call-btn:hover {
    transform: scale(1.05);
  }

  .call-btn:active {
    transform: scale(0.95);
  }

  .call-btn.accept {
    background: var(--color-success);
    color: white;
  }

  .call-btn.accept:hover {
    background: #16a34a;
  }

  .call-btn.hangup {
    background: var(--color-error);
    color: white;
  }

  .call-btn.hangup:hover {
    background: var(--color-error-dark);
  }

  .call-btn.mute {
    background: var(--bg-elevated);
    color: var(--text-primary);
  }

  .call-btn.mute:hover {
    background: var(--bg-elevated-hover);
  }

  .call-btn.mute.muted {
    background: var(--color-warning);
    color: #1a1a2e;
  }
</style>
