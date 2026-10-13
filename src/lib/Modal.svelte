<script lang="ts">
  import type { Snippet } from "svelte";
  import { focusTrap } from "$lib/composables";

  let {
    label,
    onclose,
    maxWidth,
    children,
  }: {
    /** Accessible name of the dialog. */
    label: string;
    /** Called on Escape and on a click outside the dialog. */
    onclose: () => void;
    /** CSS `max-width` of the dialog; the `.modal` default applies if unset. */
    maxWidth?: string;
    children: Snippet;
  } = $props();

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }

  function handleOverlayClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="modal-overlay" onclick={handleOverlayClick} role="presentation">
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={label}
    tabindex="-1"
    style:max-width={maxWidth}
    use:focusTrap
  >
    {@render children()}
  </div>
</div>

<style>
  .modal:focus {
    outline: none;
  }
</style>
