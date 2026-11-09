<script lang="ts">
  import Modal from "#lib/Modal.svelte";

  let {
    message = "Delete this post? This cannot be undone.",
    confirmLabel = "Delete",
    onconfirm,
    oncancel,
  }: {
    message?: string;
    confirmLabel?: string;
    onconfirm: () => void;
    oncancel: () => void;
  } = $props();
</script>

<Modal label="Confirm action" onclose={oncancel}>
  <p>{message}</p>
  <div class="modal-actions">
    <button class="modal-cancel" onclick={oncancel}>Cancel</button>
    <button class="modal-confirm" onclick={onconfirm}>{confirmLabel}</button>
  </div>
</Modal>
