<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import QrCodeDialog from "#lib/QrCodeDialog.svelte";
  import type { LinkQrPayload } from "#lib/types.ts";
  import { copyToClipboard } from "#lib/utils.ts";

  interface Props {
    payload: LinkQrPayload;
    onclose: () => void;
  }

  let { payload, onclose }: Props = $props();

  let qrData = $derived(JSON.stringify(payload));
  let expiresAt = $derived(payload.expires_at);
  let now = $state(Date.now());
  let secondsLeft = $derived(Math.max(0, Math.ceil((expiresAt - now) / 1000)));

  // A device without a camera cannot scan the code, so the payload text can
  // be copied and pasted on the other device. The clipboard is overwritten
  // when this dialog is destroyed (closed or expired).
  let copied = $state(false);
  let copyError = $state("");

  async function copyPayload() {
    copyError = "";
    try {
      await copyToClipboard(qrData);
      copied = true;
    } catch {
      copyError = "Copy failed";
    }
  }

  $effect(() => {
    return () => {
      if (copied) copyToClipboard("").catch(() => {});
    };
  });

  // Counts down to `expires_at`. At expiry the link session is cancelled in
  // the backend and the dialog closes, so no expired code stays on screen.
  $effect(() => {
    const deadline = expiresAt;
    const timer = setInterval(() => {
      const current = Date.now();
      now = current;
      if (current < deadline) return;
      clearInterval(timer);
      invoke("cancel_device_link").catch((e) =>
        console.error("Failed to cancel device link:", e),
      );
      onclose();
    }, 250);
    return () => clearInterval(timer);
  });
</script>

<QrCodeDialog
  label="Device link QR code"
  caption="Scan with new device to pair"
  data={qrData}
  closeLabel="Cancel"
  {onclose}
>
  <p class="qr-hint">
    This code expires in {secondsLeft}
    {secondsLeft === 1 ? "second" : "seconds"}. Show it only to your own device:
    it transfers your identity keys.
  </p>
  <button class="copy-btn" onclick={copyPayload}>
    {copied ? "Copied" : "Copy code as text"}
  </button>
  <p class="qr-hint">
    For a device without a camera: paste the text there. The clipboard is
    cleared when this dialog closes.
  </p>
  {#if copyError}
    <p class="qr-error" role="alert">{copyError}</p>
  {/if}
</QrCodeDialog>

<style>
  .qr-hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 0 0 0.75rem;
  }

  .copy-btn {
    margin: 0 0 0.5rem;
  }

  .qr-error {
    color: var(--danger-text);
    font-size: var(--text-sm);
    margin: 0 0 0.75rem;
  }
</style>
