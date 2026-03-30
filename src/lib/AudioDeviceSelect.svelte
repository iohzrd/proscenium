<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { platform } from "@tauri-apps/plugin-os";
  import { onMount } from "svelte";

  interface AudioDevice {
    name: string;
    is_default: boolean;
  }

  interface CommAudioDevice {
    id: number;
    name: string;
    device_type: number;
    label: string;
  }

  /** When true, switching devices also hot-swaps the active call stream. */
  let { liveCall = false }: { liveCall?: boolean } = $props();

  let isAndroid = $state(false);
  let inputDevices = $state<AudioDevice[]>([]);
  let outputDevices = $state<AudioDevice[]>([]);
  let commDevices = $state<CommAudioDevice[]>([]);
  let selectedInput = $state("");
  let selectedOutput = $state("");
  let selectedCommId = $state<number | null>(null);
  let switching = $state(false);
  let loading = $state(true);

  function deviceIcon(deviceType: number): string {
    switch (deviceType) {
      case 1:
        return "phone-earpiece";
      case 2:
        return "speaker";
      case 7:
      case 26:
      case 27:
        return "bluetooth";
      case 3:
      case 4:
      case 22:
        return "wired";
      default:
        return "audio";
    }
  }

  async function load() {
    loading = true;
    try {
      isAndroid = platform() === "android";

      if (isAndroid) {
        commDevices = await invoke<CommAudioDevice[]>(
          "list_android_audio_devices",
        );
        loading = false;
        return;
      }

      const [inputs, outputs, savedInput, savedOutput] = await Promise.all([
        invoke<AudioDevice[]>("list_audio_input_devices"),
        invoke<AudioDevice[]>("list_audio_output_devices"),
        invoke<string | null>("get_audio_input_device"),
        invoke<string | null>("get_audio_output_device"),
      ]);
      inputDevices = inputs;
      outputDevices = outputs;
      selectedInput = savedInput || "";
      selectedOutput = savedOutput || "";
    } catch (e) {
      console.error("Failed to load audio devices:", e);
    }
    loading = false;
  }

  async function setInput(e: Event) {
    const name = (e.target as HTMLSelectElement).value;
    selectedInput = name;
    try {
      if (liveCall) {
        await invoke("switch_call_input_device", { name });
      } else {
        await invoke("set_audio_input_device", { name });
      }
    } catch (err) {
      console.error("Failed to set input device:", err);
    }
  }

  async function setOutput(e: Event) {
    const name = (e.target as HTMLSelectElement).value;
    selectedOutput = name;
    try {
      if (liveCall) {
        await invoke("switch_call_output_device", { name });
      } else {
        await invoke("set_audio_output_device", { name });
      }
    } catch (err) {
      console.error("Failed to set output device:", err);
    }
  }

  async function selectCommDevice(device: CommAudioDevice) {
    if (switching) return;
    switching = true;
    selectedCommId = device.id;
    try {
      await invoke("set_android_audio_device", { deviceId: device.id });
    } catch (err) {
      console.error("Failed to set audio device:", err);
    }
    switching = false;
  }

  onMount(load);
</script>

{#if loading}
  <p class="audio-loading">Loading audio devices...</p>
{:else if isAndroid}
  <div class="comm-heading">Audio output</div>
  <div class="comm-device-list">
    {#each commDevices as device (device.id)}
      <button
        class="comm-device-row"
        class:active={selectedCommId === device.id}
        class:switching
        onclick={() => selectCommDevice(device)}
        disabled={switching}
      >
        <span class="comm-radio" class:checked={selectedCommId === device.id}
        ></span>
        <span class="comm-icon" data-type={deviceIcon(device.device_type)}
        ></span>
        <span class="comm-label">{device.label}</span>
      </button>
    {/each}
  </div>
{:else}
  <div class="audio-device-row">
    <label class="audio-label" for="audio-input">Microphone</label>
    <select
      id="audio-input"
      class="audio-select"
      value={selectedInput}
      onchange={setInput}
    >
      <option value="">Default</option>
      {#each inputDevices as dev}
        <option value={dev.name}>
          {dev.name}{dev.is_default ? " (system default)" : ""}
        </option>
      {/each}
    </select>
  </div>
  <div class="audio-device-row">
    <label class="audio-label" for="audio-output">Speaker</label>
    <select
      id="audio-output"
      class="audio-select"
      value={selectedOutput}
      onchange={setOutput}
    >
      <option value="">Default</option>
      {#each outputDevices as dev}
        <option value={dev.name}>
          {dev.name}{dev.is_default ? " (system default)" : ""}
        </option>
      {/each}
    </select>
  </div>
  <button class="audio-refresh-btn" onclick={load}>Refresh devices</button>
{/if}

<style>
  .audio-loading {
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 0;
  }

  /* Android communication device picker (Signal-style) */
  .comm-heading {
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--text-primary);
    margin-bottom: 0.75rem;
  }

  .comm-device-list {
    display: flex;
    flex-direction: column;
  }

  .comm-device-row {
    display: flex;
    align-items: center;
    gap: 0.85rem;
    padding: 0.9rem 0.5rem;
    border: none;
    background: none;
    cursor: pointer;
    color: var(--text-primary);
    font-size: var(--text-base);
    font-family: inherit;
    border-top: 1px solid var(--border);
    transition: opacity var(--transition-fast);
  }

  .comm-device-row:first-child {
    border-top: none;
  }

  .comm-device-row:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .comm-device-row.switching:not(.active) {
    opacity: 0.4;
  }

  .comm-radio {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    border: 2px solid var(--text-muted);
    flex-shrink: 0;
    position: relative;
    transition:
      border-color var(--transition-fast),
      background var(--transition-fast);
  }

  .comm-radio.checked {
    border-color: var(--accent);
    background: var(--accent);
  }

  .comm-radio.checked::after {
    content: "";
    position: absolute;
    top: 4px;
    left: 4px;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--text-on-accent);
  }

  .comm-icon {
    width: 24px;
    height: 24px;
    flex-shrink: 0;
    opacity: 0.7;
  }

  .comm-label {
    flex: 1;
    text-align: left;
  }

  /* Desktop device selectors */
  .audio-device-row {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    margin-bottom: 0.75rem;
  }

  .audio-label {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-primary);
  }

  .audio-select {
    width: 100%;
    padding: 0.45rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-sm);
    font-family: inherit;
    cursor: pointer;
    appearance: auto;
  }

  .audio-select:focus {
    outline: none;
    border-color: var(--accent);
  }

  .audio-refresh-btn {
    background: none;
    border: none;
    color: var(--accent-light);
    font-size: var(--text-xs);
    font-weight: 600;
    cursor: pointer;
    padding: 0;
    font-family: inherit;
  }

  .audio-refresh-btn:hover {
    text-decoration: underline;
  }
</style>
