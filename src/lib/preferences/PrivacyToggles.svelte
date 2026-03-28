<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  let mdnsDiscovery = $state(false);
  let dhtDiscovery = $state(false);
  let mdnsLoading = $state(false);
  let dhtLoading = $state(false);
  let shareFollows = $state(true);
  let shareFollowers = $state(true);
  let shareFollowsLoading = $state(false);
  let shareFollowersLoading = $state(false);

  onMount(async () => {
    try {
      [mdnsDiscovery, dhtDiscovery, shareFollows, shareFollowers] =
        await Promise.all([
          invoke<boolean>("get_mdns_discovery"),
          invoke<boolean>("get_dht_discovery"),
          invoke<boolean>("get_share_follows"),
          invoke<boolean>("get_share_followers"),
        ]);
    } catch {
      // preferences not available yet
    }
  });

  async function toggleMdns() {
    mdnsLoading = true;
    try {
      const next = !mdnsDiscovery;
      await invoke("set_mdns_discovery", { enabled: next });
      mdnsDiscovery = next;
    } catch (e) {
      console.error("Failed to toggle mDNS:", e);
    }
    mdnsLoading = false;
  }

  async function toggleDht() {
    dhtLoading = true;
    try {
      const next = !dhtDiscovery;
      await invoke("set_dht_discovery", { enabled: next });
      dhtDiscovery = next;
    } catch (e) {
      console.error("Failed to toggle DHT:", e);
    }
    dhtLoading = false;
  }

  async function toggleShareFollows() {
    shareFollowsLoading = true;
    try {
      const next = !shareFollows;
      await invoke("set_share_follows", { enabled: next });
      shareFollows = next;
    } catch (e) {
      console.error("Failed to toggle share follows:", e);
    }
    shareFollowsLoading = false;
  }

  async function toggleShareFollowers() {
    shareFollowersLoading = true;
    try {
      const next = !shareFollowers;
      await invoke("set_share_followers", { enabled: next });
      shareFollowers = next;
    } catch (e) {
      console.error("Failed to toggle share followers:", e);
    }
    shareFollowersLoading = false;
  }
</script>

<section class="settings-section">
  <h3>Privacy</h3>
  <div class="toggle-row">
    <div class="toggle-info">
      <span class="toggle-label">Local network discovery (mDNS)</span>
      <p class="toggle-desc">
        Announce on your local network so nearby peers can discover and connect
        directly. Only exposes your IP to devices on the same LAN.
      </p>
    </div>
    <button
      class="toggle-switch"
      class:active={mdnsDiscovery}
      onclick={toggleMdns}
      disabled={mdnsLoading}
      aria-label="Toggle mDNS discovery"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
  <div class="toggle-row">
    <div class="toggle-info">
      <span class="toggle-label">Global discovery (DHT)</span>
      <p class="toggle-desc">
        Publish your IP address to the Mainline DHT so any peer who knows your
        public key can connect directly without a relay. Exposes your IP
        globally.
      </p>
    </div>
    <button
      class="toggle-switch"
      class:active={dhtDiscovery}
      onclick={toggleDht}
      disabled={dhtLoading}
      aria-label="Toggle DHT discovery"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
  <p class="setting-hint">Changes take effect on next restart.</p>
  <div class="toggle-row">
    <div class="toggle-info">
      <span class="toggle-label">Share follow list</span>
      <p class="toggle-desc">
        Allow others to see who you follow when they visit your profile.
      </p>
    </div>
    <button
      class="toggle-switch"
      class:active={shareFollows}
      onclick={toggleShareFollows}
      disabled={shareFollowsLoading}
      aria-label="Toggle share follows"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
  <div class="toggle-row">
    <div class="toggle-info">
      <span class="toggle-label">Share followers list</span>
      <p class="toggle-desc">
        Allow others to see who follows you when they visit your profile.
      </p>
    </div>
    <button
      class="toggle-switch"
      class:active={shareFollowers}
      onclick={toggleShareFollowers}
      disabled={shareFollowersLoading}
      aria-label="Toggle share followers"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
</section>

<style>
  .settings-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1rem 1.25rem;
  }

  h3 {
    margin: 0 0 0.75rem;
    font-size: var(--text-lg);
    color: var(--text-primary);
  }

  .setting-hint {
    color: var(--text-tertiary);
    font-size: var(--text-xs);
    margin: 0.5rem 0 0;
    line-height: 1.4;
  }

  .toggle-row {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
  }

  .toggle-info {
    flex: 1;
  }

  .toggle-label {
    font-weight: 600;
    font-size: var(--text-sm);
    color: var(--text-primary);
  }

  .toggle-desc {
    color: var(--text-secondary);
    font-size: var(--text-xs);
    margin: 0.25rem 0 0;
    line-height: 1.4;
  }

  .toggle-switch {
    position: relative;
    width: 44px;
    height: 24px;
    border-radius: 12px;
    border: none;
    background: var(--bg-elevated);
    cursor: pointer;
    flex-shrink: 0;
    padding: 0;
    transition: background 0.2s;
    margin-top: 0.1rem;
  }

  .toggle-switch.active {
    background: var(--accent);
  }

  .toggle-switch:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .toggle-knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--text-primary);
    transition: transform 0.2s;
  }

  .toggle-switch.active .toggle-knob {
    transform: translateX(20px);
  }
</style>
