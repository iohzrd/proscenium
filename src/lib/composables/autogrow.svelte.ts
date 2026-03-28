/** Svelte action: auto-grow a textarea to fit its content. */
export function autogrow(node: HTMLTextAreaElement) {
  function resize() {
    node.style.height = "auto";
    node.style.overflow = "hidden";
    const max = parseFloat(getComputedStyle(node).maxHeight) || Infinity;
    if (node.scrollHeight > max) {
      node.style.height = max + "px";
      node.style.overflow = "auto";
    } else {
      node.style.height = node.scrollHeight + "px";
    }
  }
  node.style.resize = "none";
  resize();
  node.addEventListener("input", resize);
  return {
    destroy() {
      node.removeEventListener("input", resize);
    },
  };
}
