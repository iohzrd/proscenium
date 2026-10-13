const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Svelte action for a modal dialog element. On mount it moves focus into the
 * node: to the descendant with `data-autofocus`, else the first focusable
 * descendant, else the node. Tab and Shift+Tab cycle through the node's
 * focusable descendants. On destroy it returns focus to the element that was
 * focused before the mount.
 */
export function focusTrap(node: HTMLElement) {
  const previouslyFocused =
    document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;

  function focusable(): HTMLElement[] {
    // An element without client rects is not rendered and cannot take focus.
    return Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (el) => el.getClientRects().length > 0,
    );
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key !== "Tab") return;
    const items = focusable();
    if (items.length === 0) {
      e.preventDefault();
      node.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || !node.contains(active))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (active === last || !node.contains(active))) {
      e.preventDefault();
      first.focus();
    }
  }

  if (!node.hasAttribute("tabindex")) node.tabIndex = -1;
  const initial =
    node.querySelector<HTMLElement>("[data-autofocus]") ?? focusable()[0];
  (initial ?? node).focus();
  node.addEventListener("keydown", handleKeydown);

  return {
    destroy() {
      node.removeEventListener("keydown", handleKeydown);
      if (previouslyFocused?.isConnected) previouslyFocused.focus();
    },
  };
}
