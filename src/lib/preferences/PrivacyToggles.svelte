<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  interface Toggle {
    label: string;
    description: string;
    getCommand: string;
    setCommand: string;
    /** Value shown until the stored preference is loaded. */
    initial: boolean;
  }

  // Applied at the next start of the app.
  const DISCOVERY_TOGGLES: Toggle[] = [
    {
      label: "Local network discovery (mDNS)",
      description:
        "Announce on your local network so nearby peers can discover and connect directly. Only exposes your IP to devices on the same LAN.",
      getCommand: "get_mdns_discovery",
      setCommand: "set_mdns_discovery",
      initial: false,
    },
    {
      label: "Global discovery (DHT)",
      description:
        "Publish your IP address to the Mainline DHT so any peer who knows your public key can connect directly without a relay. Exposes your IP globally.",
      getCommand: "get_dht_discovery",
      setCommand: "set_dht_discovery",
      initial: false,
    },
  ];

  const SHARING_TOGGLES: Toggle[] = [
    {
      label: "Share follow list",
      description:
        "Allow others to see who you follow when they visit your profile.",
      getCommand: "get_share_follows",
      setCommand: "set_share_follows",
      initial: true,
    },
    {
      label: "Share followers list",
      description:
        "Allow others to see who follows you when they visit your profile.",
      getCommand: "get_share_followers",
      setCommand: "set_share_followers",
      initial: true,
    },
  ];

  const ALL_TOGGLES = [...DISCOVERY_TOGGLES, ...SHARING_TOGGLES];

  // Keyed by `setCommand`.
  let enabled = $state<Record<string, boolean>>(
    Object.fromEntries(ALL_TOGGLES.map((t) => [t.setCommand, t.initial])),
  );
  let loaded = $state(false);
  let saving = $state<Record<string, boolean>>({});
  let errorMessage = $state("");

  onMount(async () => {
    try {
      const values = await Promise.all(
        ALL_TOGGLES.map((t) => invoke<boolean>(t.getCommand)),
      );
      for (const [i, toggle] of ALL_TOGGLES.entries()) {
        enabled[toggle.setCommand] = values[i];
      }
      loaded = true;
    } catch (e) {
      errorMessage = `Failed to load privacy settings: ${e}`;
    }
  });

  async function flip(toggle: Toggle) {
    const next = !enabled[toggle.setCommand];
    saving[toggle.setCommand] = true;
    errorMessage = "";
    try {
      await invoke(toggle.setCommand, { enabled: next });
      enabled[toggle.setCommand] = next;
    } catch (e) {
      errorMessage = `Failed to change "${toggle.label}": ${e}`;
    }
    saving[toggle.setCommand] = false;
  }
</script>

{#snippet row(toggle: Toggle)}
  <div class="toggle-row">
    <div class="toggle-info">
      <span class="toggle-label">{toggle.label}</span>
      <p class="toggle-desc">{toggle.description}</p>
    </div>
    <button
      class="toggle-switch"
      class:active={enabled[toggle.setCommand]}
      role="switch"
      aria-checked={enabled[toggle.setCommand]}
      aria-label={toggle.label}
      onclick={() => flip(toggle)}
      disabled={!loaded || saving[toggle.setCommand]}
    >
      <span class="toggle-knob"></span>
    </button>
  </div>
{/snippet}

<section class="settings-section">
  <h3>Privacy</h3>
  {#each DISCOVERY_TOGGLES as toggle}
    {@render row(toggle)}
  {/each}
  <p class="setting-hint">Changes take effect on next restart.</p>
  {#each SHARING_TOGGLES as toggle}
    {@render row(toggle)}
  {/each}
  {#if errorMessage}
    <p class="toggle-error" role="alert">{errorMessage}</p>
  {/if}
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

  .toggle-error {
    color: var(--color-error-light);
    font-size: var(--text-sm);
    margin: 0.5rem 0 0;
  }
</style>
