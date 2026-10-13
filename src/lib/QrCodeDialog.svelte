<script lang="ts">
  import QR from "@svelte-put/qr/svg/QR.svelte";
  import type { Snippet } from "svelte";
  import Modal from "$lib/Modal.svelte";

  let {
    label,
    caption,
    data,
    closeLabel = "Close",
    onclose,
    children,
  }: {
    /** Accessible name of the dialog. */
    label: string;
    /** Text above the code. */
    caption: string;
    /** Content of the QR code. */
    data: string;
    closeLabel?: string;
    onclose: () => void;
    /** Rendered between the code and the close button. */
    children?: Snippet;
  } = $props();
</script>

<Modal {label} {onclose} maxWidth="300px">
  <div class="qr-dialog">
    <p class="qr-caption">{caption}</p>
    <div class="qr-wrapper">
      <QR
        {data}
        moduleFill="#000000"
        anchorOuterFill="#000000"
        anchorInnerFill="#000000"
      />
    </div>
    {@render children?.()}
  </div>
  <div class="modal-actions">
    <button class="modal-cancel" onclick={onclose}>{closeLabel}</button>
  </div>
</Modal>

<style>
  .qr-dialog {
    text-align: center;
  }

  .qr-caption {
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
</style>
