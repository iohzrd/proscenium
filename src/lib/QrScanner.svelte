<script lang="ts">
  import { onMount } from "svelte";

  let {
    hint,
    onresult,
    onunavailable,
    oncancel,
  }: {
    hint: string;
    /** Called with the text of the scanned QR code. */
    onresult: (text: string) => void;
    /**
     * Called when no scan runs: "denied" if camera permission was refused,
     * "error" if the scanner plugin is absent (desktop) or the scan failed.
     */
    onunavailable: (reason: "denied" | "error", error?: unknown) => void;
    oncancel: () => void;
  } = $props();

  let scanning = $state(true);

  type Outcome =
    | { text: string }
    | { reason: "denied" | "error"; error?: unknown };

  onMount(() => {
    let destroyed = false;
    let cancelScan: (() => Promise<void>) | null = null;

    // The native camera preview is drawn behind the webview.
    function setPageBackground(value: string) {
      document.documentElement.style.background = value;
      document.body.style.background = value;
    }

    /** Resolves to null if the component was destroyed before the scan ended. */
    async function scanOnce(): Promise<Outcome | null> {
      try {
        const scanner = await import("@tauri-apps/plugin-barcode-scanner");
        if (destroyed) return null;
        let permission = await scanner.checkPermissions();
        if (destroyed) return null;
        if (permission !== "granted") {
          permission = await scanner.requestPermissions();
          if (destroyed) return null;
        }
        if (permission !== "granted") return { reason: "denied" };

        setPageBackground("transparent");
        cancelScan = scanner.cancel;
        const result = await scanner.scan({
          formats: [scanner.Format.QRCode],
          windowed: true,
        });
        return destroyed ? null : { text: result.content };
      } catch (error) {
        return destroyed ? null : { reason: "error", error };
      } finally {
        cancelScan = null;
        setPageBackground("");
      }
    }

    scanOnce().then((outcome) => {
      if (!outcome) return;
      scanning = false;
      if ("text" in outcome) onresult(outcome.text);
      else onunavailable(outcome.reason, outcome.error);
    });

    return () => {
      destroyed = true;
      cancelScan?.().catch(() => {
        // The scan already ended.
      });
      cancelScan = null;
      setPageBackground("");
    };
  });
</script>

<svelte:window
  onkeydown={(e) => {
    if (scanning && e.key === "Escape") oncancel();
  }}
/>

{#if scanning}
  <div class="scanner-overlay">
    <div class="scanner-top"></div>
    <div class="scanner-middle">
      <div class="scanner-side"></div>
      <div class="scanner-viewfinder"></div>
      <div class="scanner-side"></div>
    </div>
    <div class="scanner-bottom">
      <p class="scanner-hint">{hint}</p>
      <button class="scanner-close" onclick={oncancel}>Cancel</button>
    </div>
  </div>
{/if}

<style>
  .scanner-overlay {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    z-index: var(--z-scanner);
  }

  .scanner-top {
    flex: 1;
    background: var(--overlay-medium);
  }

  .scanner-middle {
    display: flex;
  }

  .scanner-side {
    flex: 1;
    background: var(--overlay-medium);
  }

  .scanner-viewfinder {
    width: 250px;
    height: 250px;
    border: 3px solid var(--accent-medium);
    border-radius: var(--radius-2xl);
    flex-shrink: 0;
  }

  .scanner-bottom {
    flex: 1;
    background: var(--overlay-medium);
    display: flex;
    flex-direction: column;
    align-items: center;
    padding-top: 1rem;
  }

  .scanner-hint {
    color: var(--text-on-accent);
    font-size: var(--text-base);
    margin: 0;
    text-shadow: 0 1px 4px rgba(0, 0, 0, 0.8);
  }

  .scanner-close {
    margin-top: 1.5rem;
    background: var(--overlay-light);
    color: var(--text-on-accent);
    border: 1px solid var(--overlay-white-faint);
    border-radius: var(--radius-lg);
    padding: 0.6rem 2rem;
    font-size: var(--text-icon);
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .scanner-close:hover {
    background: var(--overlay-medium);
  }
</style>
