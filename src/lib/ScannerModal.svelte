<script lang="ts">
  import { hapticNotification } from "#lib/haptics.ts";
  import Modal from "#lib/Modal.svelte";
  import QrScanner from "#lib/QrScanner.svelte";
  import { isPubkeyFormat } from "#lib/utils.ts";

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

  /**
   * Parses `proscenium://profile/<pubkey>?transport=<node id>`. Returns null
   * unless both ids are 64 lowercase hex digits (`transport` is optional).
   */
  function parseFromUrl(url: string): ScannedResult | null {
    try {
      const parsed = new URL(url);
      if (parsed.protocol !== "proscenium:") return null;
      const host = parsed.hostname;
      if (host !== "user" && host !== "profile") return null;
      const pubkey = parsed.pathname.slice(1);
      if (!isPubkeyFormat(pubkey)) return null;
      const transport = parsed.searchParams.get("transport");
      if (transport === null) return { pubkey };
      if (!isPubkeyFormat(transport)) return null;
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
      error = "The scanned code is not an invite link.";
    }
  }

  function handleUnavailable(reason: "denied" | "error", e?: unknown) {
    error =
      reason === "denied" ? "Camera permission denied" : `Scan error: ${e}`;
  }
</script>

<QrScanner
  hint="Point camera at a QR code"
  onresult={handleScanResult}
  onunavailable={handleUnavailable}
  oncancel={onclose}
/>

{#if error}
  <Modal label="Scan error" {onclose} maxWidth="340px">
    <p class="scanner-label">{error}</p>
    <div class="modal-actions">
      <button class="modal-cancel" onclick={onclose}>Close</button>
    </div>
  </Modal>
{/if}

<style>
  .scanner-label {
    margin: 0 0 0.75rem;
    color: var(--text-secondary);
    font-size: var(--text-base);
    text-align: center;
  }
</style>
