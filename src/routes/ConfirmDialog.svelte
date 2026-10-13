<script lang="ts">
  import type { Snippet } from "svelte";
  import Modal from "$lib/Modal.svelte";

  let {
    label,
    confirmLabel,
    cancelLabel = "Cancel",
    danger = true,
    wide = false,
    confirmDisabled = false,
    onconfirm,
    oncancel,
    children,
  }: {
    /** Accessible name of the dialog. */
    label: string;
    confirmLabel: string;
    cancelLabel?: string;
    /** Style the confirm button as destructive. */
    danger?: boolean;
    wide?: boolean;
    confirmDisabled?: boolean;
    onconfirm: () => void;
    /** Called by the cancel button, on Escape and on a click outside the dialog. */
    oncancel: () => void;
    /** Content shown above the buttons. */
    children: Snippet;
  } = $props();
</script>

<Modal {label} onclose={oncancel} maxWidth={wide ? "420px" : undefined}>
  {@render children()}
  <div class="modal-actions">
    <!-- Initial focus is on Cancel, so Enter does not confirm by default. -->
    <button class="modal-cancel" onclick={oncancel} data-autofocus>
      {cancelLabel}
    </button>
    <button
      class="modal-confirm"
      class:save={!danger}
      onclick={onconfirm}
      disabled={confirmDisabled}
    >
      {confirmLabel}
    </button>
  </div>
</Modal>
