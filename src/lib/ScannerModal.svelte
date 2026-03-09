<script lang="ts">
  import { onMount } from "svelte";
  import { hapticNotification } from "$lib/haptics";

  interface ScannedResult {
    pubkey: string;
    transportNodeId?: string;
  }

  interface Props {
    onscanned: (result: ScannedResult) => void;
    onclose: () => void;
  }

  let { onscanned, onclose }: Props = $props();

  let error = $state<string | null>(null);
  let scanning = $state(true);
  let cancelFn: (() => Promise<void>) | null = null;

  function parseFromUrl(url: string): ScannedResult | null {
    try {
      const parsed = new URL(url);
      if (parsed.protocol !== "iroh-social:") return null;
      const host = parsed.hostname;
      if (host !== "user" && host !== "profile") return null;
      const pubkey = parsed.pathname.slice(1);
      if (!pubkey) return null;
      const transport = parsed.searchParams.get("transport") ?? undefined;
      return { pubkey, transportNodeId: transport };
    } catch {
      return null;
    }
  }

  function handleScanResult(text: string) {
    const result = parseFromUrl(text);
    if (result) {
      hapticNotification("success");
      onscanned(result);
    } else {
      error = `Not an invite link: ${text}`;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  async function stopScanning() {
    if (cancelFn) {
      try {
        await cancelFn();
      } catch {
        // ignore cancel errors
      }
      cancelFn = null;
    }
    document.documentElement.style.background = "";
    document.body.style.background = "";
  }

  onMount(() => {
    (async () => {
      try {
        const { scan, cancel, Format, checkPermissions, requestPermissions } =
          await import("@tauri-apps/plugin-barcode-scanner");

        cancelFn = cancel;

        let perms = await checkPermissions();
        if (perms !== "granted") {
          perms = await requestPermissions();
        }
        if (perms !== "granted") {
          error = "Camera permission denied";
          scanning = false;
          return;
        }

        // Make webview background transparent so native camera preview is visible
        document.documentElement.style.background = "transparent";
        document.body.style.background = "transparent";

        const result = await scan({
          formats: [Format.QRCode],
          windowed: true,
        });
        scanning = false;
        await stopScanning();
        handleScanResult(result.content);
      } catch (e) {
        error = `Scan error: ${e}`;
        scanning = false;
        await stopScanning();
      }
    })();

    return () => {
      stopScanning();
    };
  });
</script>

<svelte:window onkeydown={handleKeydown} />

{#if scanning}
  <div class="scanner-overlay">
    <div class="scanner-top"></div>
    <div class="scanner-middle">
      <div class="scanner-side"></div>
      <div class="scanner-viewfinder"></div>
      <div class="scanner-side"></div>
    </div>
    <div class="scanner-bottom">
      <p class="scanner-hint">Point camera at a QR code</p>
      <button class="scanner-close" onclick={onclose}>Cancel</button>
    </div>
  </div>
{/if}

{#if error}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="modal-overlay" onclick={onclose} role="presentation">
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="modal scanner-modal"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      aria-label="Scan error"
    >
      <p class="scanner-label">{error}</p>
      <div class="modal-actions">
        <button class="modal-cancel" onclick={onclose}>Close</button>
      </div>
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

  .scanner-modal {
    max-width: 340px;
  }

  .scanner-label {
    margin: 0 0 0.75rem;
    color: var(--text-secondary);
    font-size: var(--text-base);
    text-align: center;
  }
</style>
