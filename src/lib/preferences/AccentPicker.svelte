<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { ACCENT_PRESETS, DEFAULT_ACCENT, applyAccent } from "$lib/accent";

  let selected = $state(DEFAULT_ACCENT);

  const presets = Object.values(ACCENT_PRESETS);

  onMount(async () => {
    try {
      const saved = await invoke<string | null>("get_accent_color");
      if (saved && ACCENT_PRESETS[saved]) {
        selected = saved;
      }
    } catch {
      // use default
    }
  });

  async function pick(name: string) {
    selected = name;
    applyAccent(name);
    try {
      await invoke("set_accent_color", { name });
    } catch {
      // best-effort persist
    }
  }
</script>

<section class="settings-section">
  <h3>Accent Color</h3>
  <div class="swatches">
    {#each presets as preset}
      <button
        class="swatch"
        class:active={selected === preset.name}
        style="--swatch-color: {preset.accent}"
        onclick={() => pick(preset.name)}
        aria-label={preset.label}
        title={preset.label}
      ></button>
    {/each}
  </div>
</section>

<style>
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

  .swatches {
    display: flex;
    gap: 0.6rem;
    flex-wrap: wrap;
  }

  .swatch {
    width: 32px;
    height: 32px;
    border-radius: var(--radius-full);
    border: 2px solid transparent;
    background: var(--swatch-color);
    cursor: pointer;
    transition:
      border-color var(--transition-fast),
      transform var(--transition-fast);
  }

  .swatch:hover {
    transform: scale(1.15);
  }

  .swatch.active {
    border-color: var(--text-primary);
    box-shadow:
      0 0 0 2px var(--bg-surface),
      0 0 0 4px var(--swatch-color);
  }
</style>
