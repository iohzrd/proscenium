<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import type { CallState } from "$lib/types";
  import Avatar from "$lib/Avatar.svelte";
  import AudioDeviceSelect from "$lib/AudioDeviceSelect.svelte";
  import { focusTrap } from "$lib/composables";
  import { useDisplayName } from "$lib/name.svelte";

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

  // Per-call state. Each value returns to its initial value when `callId`
  // changes: this instance can be reused for the next call, which starts
  // unmuted in the backend.
  let muted = $derived.by(() => {
    void callId;
    return false;
  });
  let showDevices = $derived.by(() => {
    void callId;
    return false;
  });
  let errorMessage = $derived.by(() => {
    void callId;
    return "";
  });
  // True from the click on Answer until `callState` changes or the command fails.
  let accepting = $derived.by(() => {
    void callId;
    void callState;
    return false;
  });

  let duration = $state(0);

  const peer = useDisplayName(
    () => peerPubkey,
    () => selfId,
  );

  $effect(() => {
    if (callState !== "active") return;
    duration = 0;
    const timer = setInterval(() => (duration += 1), 1000);
    return () => clearInterval(timer);
  });

  function formatDuration(seconds: number): string {
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}:${s.toString().padStart(2, "0")}`;
  }

  async function accept() {
    if (accepting) return;
    accepting = true;
    errorMessage = "";
    try {
      await invoke("accept_call", { callId });
    } catch (e) {
      errorMessage = `Failed to answer: ${e}`;
      accepting = false;
    }
  }

  async function reject() {
    try {
      await invoke("reject_call", { callId });
    } catch (e) {
      errorMessage = `Failed to decline: ${e}`;
    }
  }

  async function hangup() {
    try {
      await invoke("hangup_call");
    } catch (e) {
      errorMessage = `Failed to hang up: ${e}`;
    }
  }

  async function toggleMute() {
    try {
      muted = await invoke<boolean>("toggle_mute_call");
    } catch (e) {
      errorMessage = `Failed to change mute: ${e}`;
    }
  }
</script>

<div class="call-overlay">
  <div
    class="call-card"
    role="dialog"
    aria-modal="true"
    aria-label="Call with {peer.name}"
    tabindex="-1"
    use:focusTrap
  >
    <div class="call-avatar">
      <Avatar pubkey={peerPubkey} name={peer.name} size={72} />
    </div>
    <!-- Takes the initial focus, so that Enter or Space does not activate Answer, Decline or Hang up. -->
    <div class="call-peer-name" tabindex="-1" data-autofocus>{peer.name}</div>

    {#if callState === "ringing"}
      <div class="call-status" role="status">Calling...</div>
      <div class="call-actions">
        <button class="call-btn hangup" onclick={hangup}>Cancel</button>
      </div>
    {:else if callState === "incoming"}
      <div class="call-status" role="status">
        {accepting ? "Connecting..." : "Incoming call"}
      </div>
      <div class="call-actions">
        <button class="call-btn accept" onclick={accept} disabled={accepting}>
          {accepting ? "Answering..." : "Answer"}
        </button>
        <button class="call-btn hangup" onclick={reject}>Decline</button>
      </div>
    {:else if callState === "active"}
      <div class="call-status">{formatDuration(duration)}</div>
      <div class="call-actions">
        <button
          class="call-btn mute"
          class:muted
          onclick={toggleMute}
          aria-pressed={muted}
        >
          {muted ? "Unmute" : "Mute"}
        </button>
        <button
          class="call-btn devices-btn"
          onclick={() => (showDevices = !showDevices)}
          aria-expanded={showDevices}
        >
          Audio
        </button>
        <button class="call-btn hangup" onclick={hangup}>Hang up</button>
      </div>
      {#if showDevices}
        <div class="call-devices">
          <AudioDeviceSelect liveCall={true} />
        </div>
      {/if}
    {:else if callState === "failed"}
      <div class="call-status call-failed" role="status">Call failed</div>
    {:else if callState === "ended"}
      <div class="call-status" role="status">Call ended</div>
    {/if}

    {#if errorMessage}
      <p class="call-error" role="alert">{errorMessage}</p>
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
    background: var(--overlay-medium);
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

  .call-card:focus,
  .call-peer-name:focus {
    outline: none;
  }

  .call-status {
    font-size: var(--text-base);
    color: var(--text-secondary);
    font-variant-numeric: tabular-nums;
  }

  .call-failed {
    color: var(--color-error);
  }

  .call-error {
    color: var(--color-error-light);
    font-size: var(--text-sm);
    text-align: center;
    margin: 0;
    overflow-wrap: anywhere;
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

  .call-btn:hover:not(:disabled) {
    transform: scale(1.05);
  }

  .call-btn:active:not(:disabled) {
    transform: scale(0.95);
  }

  .call-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .call-btn.accept {
    background: var(--color-success);
    color: var(--text-on-accent);
  }

  .call-btn.accept:hover {
    background: var(--color-success-hover);
  }

  .call-btn.hangup {
    background: var(--color-error);
    color: var(--text-on-accent);
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
    color: var(--bg-base);
  }

  .call-btn.devices-btn {
    background: var(--bg-elevated);
    color: var(--text-primary);
  }

  .call-btn.devices-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .call-devices {
    width: 100%;
    padding-top: 0.5rem;
    border-top: 1px solid var(--border);
  }
</style>
