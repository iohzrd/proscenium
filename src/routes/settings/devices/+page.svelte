<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import LinkQrModal from "$lib/LinkQrModal.svelte";
  import LinkScannerModal from "$lib/LinkScannerModal.svelte";
  import Timeago from "$lib/Timeago.svelte";
  import type { DeviceEntry, LinkQrPayload } from "$lib/types";

  let devices = $state<DeviceEntry[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let linkStatus = $state<string | null>(null);

  let showLinkQr = $state(false);
  let linkPayload = $state<LinkQrPayload | null>(null);
  let showScanner = $state(false);
  let linking = $state(false);

  async function loadDevices() {
    try {
      devices = await invoke<DeviceEntry[]>("get_linked_devices");
    } catch (e) {
      console.error("Failed to load devices:", e);
    }
    loading = false;
  }

  async function startLink(transferMasterKey: boolean) {
    error = null;
    try {
      linkPayload = await invoke<LinkQrPayload>("start_device_link", {
        transferMasterKey,
      });
      showLinkQr = true;
    } catch (e) {
      error = `Failed to start link: ${e}`;
    }
  }

  async function cancelLink() {
    showLinkQr = false;
    linkPayload = null;
    try {
      await invoke("cancel_device_link");
    } catch {
      // ignore
    }
  }

  async function handleScanned(payload: LinkQrPayload) {
    showScanner = false;
    linking = true;
    error = null;
    linkStatus = "Connecting to device...";
    try {
      await invoke("link_with_device", { qrPayload: payload });
      linkStatus =
        "Device linked successfully! Restart the app to use the new identity.";
      await loadDevices();
    } catch (e) {
      error = `Link failed: ${e}`;
      linkStatus = null;
    }
    linking = false;
  }

  onMount(() => {
    loadDevices();

    const unlisten = listen("device-link-progress", (event) => {
      const stage = event.payload as string;
      if (stage === "bundle_sent") {
        showLinkQr = false;
        linkPayload = null;
        linkStatus = "Link bundle sent to new device.";
        loadDevices();
      }
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  });
</script>

<h2>Linked Devices</h2>

{#if error}
  <p class="error-msg">{error}</p>
{/if}

{#if linkStatus}
  <p class="status-msg">{linkStatus}</p>
{/if}

<section class="devices-section">
  <h3>Your Devices</h3>
  {#if loading}
    <p class="loading-text">Loading...</p>
  {:else if devices.length === 0}
    <p class="empty-text">No linked devices yet.</p>
  {:else}
    <div class="device-list">
      {#each devices as device (device.node_id)}
        <div class="device-row">
          <div class="device-icon">
            <svg
              width="20"
              height="20"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <rect x="5" y="2" width="14" height="20" rx="2" ry="2" />
              <line x1="12" y1="18" x2="12.01" y2="18" />
            </svg>
          </div>
          <div class="device-info">
            <span class="device-name">
              {device.device_name}
              {#if device.is_primary}
                <span class="device-badge">Primary</span>
              {/if}
            </span>
            <span class="device-meta">
              <code class="device-node-id"
                >{device.node_id.slice(0, 12)}...</code
              >
              <span class="device-sep">&middot;</span>
              Added <Timeago timestamp={device.added_at} />
            </span>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</section>

<section class="devices-section">
  <h3>Link a Device</h3>
  <p class="section-desc">
    Share your identity across multiple devices. Your posts, follows, and
    messages will be available on all linked devices.
  </p>

  <div class="link-actions">
    <div class="link-card">
      <h4>I have an existing device</h4>
      <p class="link-card-desc">
        Show a QR code on this device for a new device to scan.
      </p>
      <div class="link-card-buttons">
        <button
          class="btn-accent"
          onclick={() => startLink(false)}
          disabled={linking}
        >
          Link (read-only)
        </button>
        <button
          class="btn-elevated"
          onclick={() => startLink(true)}
          disabled={linking}
        >
          Link (full access)
        </button>
      </div>
      <p class="link-card-hint">
        "Full access" transfers the master key, allowing the new device to link
        additional devices.
      </p>
    </div>

    <div class="link-card">
      <h4>I have a QR code</h4>
      <p class="link-card-desc">
        Scan or paste a link payload from your existing device.
      </p>
      <button
        class="btn-accent"
        onclick={() => (showScanner = true)}
        disabled={linking}
      >
        {linking ? "Linking..." : "Scan / Paste"}
      </button>
    </div>
  </div>
</section>

{#if showLinkQr && linkPayload}
  <LinkQrModal payload={linkPayload} onclose={cancelLink} />
{/if}

{#if showScanner}
  <LinkScannerModal
    onscanned={handleScanned}
    onclose={() => (showScanner = false)}
  />
{/if}

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

  h4 {
    margin: 0 0 0.5rem;
    font-size: var(--text-base);
    color: var(--text-primary);
  }

  .devices-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1rem 1.25rem;
  }

  .devices-section + .devices-section {
    margin-top: 1.25rem;
  }

  .error-msg {
    color: var(--color-error, #ef4444);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
    padding: 0.5rem 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--color-error, #ef4444);
    border-radius: var(--radius-md);
  }

  .status-msg {
    color: var(--color-success, #22c55e);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
    padding: 0.5rem 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--color-success, #22c55e);
    border-radius: var(--radius-md);
  }

  .loading-text,
  .empty-text {
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 0;
  }

  .device-list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .device-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.6rem 0.75rem;
    background: var(--bg-elevated);
    border-radius: var(--radius-md);
  }

  .device-icon {
    color: var(--text-muted);
    flex-shrink: 0;
    display: flex;
  }

  .device-info {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 0.15rem;
  }

  .device-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-primary);
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .device-badge {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--accent-light);
    background: var(--bg-deep);
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
  }

  .device-meta {
    font-size: var(--text-xs);
    color: var(--text-muted);
    display: flex;
    align-items: center;
    gap: 0.3rem;
  }

  .device-node-id {
    font-size: var(--text-xs);
    color: var(--text-muted);
  }

  .device-sep {
    color: var(--text-muted);
  }

  .section-desc {
    color: var(--text-secondary);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
    line-height: 1.5;
  }

  .link-actions {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .link-card {
    background: var(--bg-elevated);
    border-radius: var(--radius-md);
    padding: 0.75rem 1rem;
  }

  .link-card-desc {
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 0 0 0.75rem;
  }

  .link-card-buttons {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .link-card-hint {
    color: var(--text-muted);
    font-size: var(--text-xs);
    margin: 0.5rem 0 0;
    line-height: 1.4;
  }
</style>
