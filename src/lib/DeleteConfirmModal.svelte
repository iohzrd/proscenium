<script lang="ts">
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

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="modal-overlay" onclick={oncancel} role="presentation">
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    class="modal"
    onclick={(e) => e.stopPropagation()}
    role="dialog"
    aria-label="Confirm action"
  >
    <p>{message}</p>
    <div class="modal-actions">
      <button class="modal-cancel" onclick={oncancel}>Cancel</button>
      <button class="modal-confirm" onclick={onconfirm}>{confirmLabel}</button>
    </div>
  </div>
</div>
