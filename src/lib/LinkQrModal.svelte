<script lang="ts">
  import QR from "@svelte-put/qr/svg/QR.svelte";
  import { copyToClipboard } from "$lib/utils";
  import type { LinkQrPayload } from "$lib/types";

  interface Props {
    payload: LinkQrPayload;
    onclose: () => void;
  }

  let { payload, onclose }: Props = $props();
  let copyFeedback = $state(false);

  let qrData = $derived(JSON.stringify(payload));

  async function copyPayload() {
    await copyToClipboard(qrData);
    copyFeedback = true;
    setTimeout(() => (copyFeedback = false), 1500);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="modal-overlay" onclick={onclose} role="presentation">
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    class="modal link-qr-modal"
    onclick={(e) => e.stopPropagation()}
    role="dialog"
    aria-label="Device link QR code"
  >
    <p class="qr-label">Scan with new device to pair</p>
    <div class="qr-wrapper">
      <QR
        data={qrData}
        moduleFill="#000000"
        anchorOuterFill="#000000"
        anchorInnerFill="#000000"
      />
    </div>
    <p class="qr-hint">This code expires in 60 seconds</p>
    <div class="qr-actions">
      <button class="btn-elevated copy-btn" onclick={copyPayload}>
        {copyFeedback ? "Copied!" : "Copy payload"}
      </button>
    </div>
    <div class="modal-actions">
      <button class="modal-cancel" onclick={onclose}>Cancel</button>
    </div>
  </div>
</div>

<style>
  .link-qr-modal {
    max-width: 300px;
    text-align: center;
  }

  .qr-label {
    color: var(--text-secondary);
    font-size: var(--text-base);
    margin: 0 0 1rem;
  }

  .qr-wrapper {
    background: #ffffff;
    border-radius: var(--radius-lg);
    padding: 0.75rem;
    display: inline-block;
    margin-bottom: 0.75rem;
    line-height: 0;
  }

  .qr-wrapper :global(svg) {
    width: 200px;
    height: 200px;
  }

  .qr-hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
    margin: 0 0 0.75rem;
  }

  .qr-actions {
    margin-bottom: 0.75rem;
  }

  .copy-btn {
    border-radius: var(--radius-sm);
    padding: 0.4rem 0.8rem;
    font-size: var(--text-sm);
  }
</style>
