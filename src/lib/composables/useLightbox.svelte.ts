import type { MediaAttachment } from "$lib/types";

export function useLightbox() {
  let src = $state("");
  let alt = $state("");
  let attachment = $state<MediaAttachment | undefined>(undefined);

  function open(s: string, a: string, att?: MediaAttachment) {
    src = s;
    alt = a;
    attachment = att;
  }

  function close() {
    src = "";
    alt = "";
    attachment = undefined;
  }

  return {
    get src() {
      return src;
    },
    get alt() {
      return alt;
    },
    get attachment() {
      return attachment;
    },
    open,
    close,
  };
}
