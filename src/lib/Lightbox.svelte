<script lang="ts">
  import type { MediaAttachment } from "#lib/types.ts";
  import { getBlobContext } from "#lib/blobs.ts";
  import { focusTrap } from "#lib/composables/index.ts";
  import { showContextMenu } from "#lib/context-menu.ts";

  interface Props {
    src: string;
    alt: string;
    attachment?: MediaAttachment;
    onclose: () => void;
  }

  let { src, alt, attachment, onclose }: Props = $props();
  const { saveFileAs } = getBlobContext();

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  function handleOverlayClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }

  function handleContextMenu(e: MouseEvent) {
    const att = attachment;
    if (!att) return;
    showContextMenu(e, [
      { text: "Save Image As...", action: () => saveFileAs(att) },
    ]).catch((err) => console.error("Failed to show context menu:", err));
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="lightbox-overlay" onclick={handleOverlayClick} role="presentation">
  <div
    class="lightbox-dialog"
    role="dialog"
    aria-modal="true"
    aria-label="Image viewer"
    tabindex="-1"
    use:focusTrap
  >
    <button
      class="lightbox-close"
      onclick={onclose}
      aria-label="Close lightbox"
    >
      &times;
    </button>
    <img {src} {alt} class="lightbox-img" oncontextmenu={handleContextMenu} />
  </div>
</div>

<style>
  .lightbox-overlay {
    position: fixed;
    inset: 0;
    background: var(--overlay-dark);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: var(--z-lightbox);
    cursor: pointer;
  }

  /* Not positioned: the close button is placed relative to the overlay, and
     the box is as large as the image, so a click beside it reaches the overlay. */
  .lightbox-dialog {
    display: flex;
    cursor: default;
  }

  .lightbox-dialog:focus {
    outline: none;
  }

  .lightbox-close {
    position: absolute;
    top: 1rem;
    right: 1rem;
    background: none;
    border: none;
    color: var(--text-on-accent);
    font-size: var(--text-3xl);
    cursor: pointer;
    min-width: 44px;
    min-height: 44px;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1;
  }

  .lightbox-img {
    max-width: 95vw;
    max-height: 95vh;
    object-fit: contain;
    cursor: default;
    border-radius: var(--radius-sm);
  }
</style>
