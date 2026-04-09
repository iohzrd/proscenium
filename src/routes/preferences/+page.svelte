<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import AudioDeviceSelect from "$lib/AudioDeviceSelect.svelte";
  import ServerManagement from "$lib/preferences/ServerManagement.svelte";
  import PrivacyToggles from "$lib/preferences/PrivacyToggles.svelte";
  import SecuritySection from "$lib/preferences/SecuritySection.svelte";
  import DangerZoneSection from "$lib/preferences/DangerZoneSection.svelte";
  import AccentPicker from "$lib/preferences/AccentPicker.svelte";

  let nodeId = $state("");
  let pubkey = $state("");

  onMount(async () => {
    try {
      [nodeId, pubkey] = await Promise.all([
        invoke<string>("get_node_id"),
        invoke<string>("get_pubkey"),
      ]);
    } catch {
      // Node not ready
    }
  });
</script>

<h2>Preferences</h2>

<div class="sections">
  <section class="settings-section">
    <h3>Identity</h3>
    <div class="setting-row">
      <span class="setting-label">Node ID</span>
      <code class="setting-value">{nodeId || "..."}</code>
    </div>
    <div class="setting-row">
      <span class="setting-label">Public Key</span>
      <code class="setting-value">{pubkey || "..."}</code>
    </div>
    <p class="setting-hint">
      Node ID is your transport address (share to connect). Public Key is your
      permanent identity.
    </p>
  </section>

  <ServerManagement />

  <section class="settings-section">
    <h3>Devices</h3>
    <p class="section-desc">
      Link multiple devices to share your identity, follows, and messages.
    </p>
    <a href="/preferences/devices" class="server-manage-link">Manage devices</a>
  </section>

  <section class="settings-section">
    <h3>Audio</h3>
    <p class="section-desc">
      Choose which microphone and speaker to use for calls and stages. Changes
      apply to the next call.
    </p>
    <AudioDeviceSelect />
  </section>

  <AccentPicker />

  <PrivacyToggles />

  <SecuritySection />

  <DangerZoneSection />
</div>

<style>
  h2 {
    margin: 0 0 1.5rem;
    font-size: var(--text-xl);
    color: var(--text-primary);
  }

  h3 {
    margin: 0 0 0.75rem;
    font-size: var(--text-lg);
    color: var(--text-primary);
  }

  .settings-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1rem 1.25rem;
  }

  .setting-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.4rem;
  }

  .setting-label {
    color: var(--text-secondary);
    font-weight: 500;
    white-space: nowrap;
  }

  .setting-value {
    color: var(--text-primary);
    font-size: var(--text-sm);
    word-break: break-all;
  }

  .setting-hint {
    color: var(--text-tertiary);
    font-size: var(--text-xs);
    margin: 0.5rem 0 0;
    line-height: 1.4;
  }

  .sections {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }

  .section-desc {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
    line-height: 1.5;
  }

  .server-manage-link {
    display: inline-block;
    margin-top: 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--accent-light);
    text-decoration: none;
  }

  .server-manage-link:hover {
    text-decoration: underline;
  }
</style>
