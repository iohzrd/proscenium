import { invoke } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { onMount } from "svelte";
import type { MediaAttachment, PendingAttachment } from "$lib/types";
import {
  copyToClipboard,
  isImage,
  isVideo,
  setupInfiniteScroll,
  uploadFiles,
} from "$lib/utils";

// --- useToast ---

export function useToast() {
  let message = $state("");
  let type = $state<"error" | "success">("error");

  function show(msg: string, t: "error" | "success" = "error") {
    message = msg;
    type = t;
    setTimeout(() => (message = ""), 4000);
  }

  return {
    get message() {
      return message;
    },
    get type() {
      return type;
    },
    show,
  };
}

// --- useCopyFeedback ---

export function useCopyFeedback() {
  let feedback = $state("");

  async function copy(text: string, label: string) {
    await copyToClipboard(text);
    feedback = label;
    setTimeout(() => (feedback = ""), 1500);
  }

  return {
    get feedback() {
      return feedback;
    },
    copy,
  };
}

// --- useNodeInit ---

export function useNodeInit(onReady: () => Promise<void>) {
  let nodeId = $state("");
  let pubkey = $state("");
  let loading = $state(true);

  async function init() {
    try {
      [nodeId, pubkey] = await Promise.all([
        invoke<string>("get_node_id"),
        invoke<string>("get_pubkey"),
      ]);
      await onReady();
      loading = false;
    } catch {
      setTimeout(init, 500);
    }
  }

  return {
    get nodeId() {
      return nodeId;
    },
    get pubkey() {
      return pubkey;
    },
    get loading() {
      return loading;
    },
    init,
  };
}

// --- useEventListeners ---

export function useEventListeners(
  listeners: Record<string, (payload: unknown) => void>,
) {
  const unlisteners: Promise<UnlistenFn>[] = [];
  for (const [event, handler] of Object.entries(listeners)) {
    unlisteners.push(listen(event, (e) => handler(e.payload)));
  }
  return () => {
    unlisteners.forEach((p) => p.then((fn) => fn()));
  };
}

// --- useInfiniteScroll ---

export function useInfiniteScroll(
  getSentinel: () => HTMLElement | null,
  loadMoreFn: () => Promise<void>,
  pageSize: number,
) {
  let hasMore = $state(true);
  let loadingMore = $state(false);

  function setHasMore(count: number) {
    hasMore = count >= pageSize;
  }

  function setNoMore() {
    hasMore = false;
  }

  async function loadMore() {
    if (loadingMore || !hasMore) return;
    loadingMore = true;
    await loadMoreFn();
    loadingMore = false;
  }

  function setupEffect() {
    return setupInfiniteScroll(
      getSentinel(),
      () => hasMore,
      () => loadingMore,
      loadMore,
    );
  }

  return {
    get hasMore() {
      return hasMore;
    },
    get loadingMore() {
      return loadingMore;
    },
    setHasMore,
    setNoMore,
    loadMore,
    setupEffect,
  };
}

// --- useMentionAutocomplete ---

export function useMentionAutocomplete(
  getContent: () => string,
  setContent: (v: string) => void,
  textareaSelector: string,
) {
  let query = $state("");
  let active = $state(false);

  function handleInput(e: Event) {
    const textarea = e.target as HTMLTextAreaElement;
    const cursorPos = textarea.selectionStart;
    const textBeforeCursor = textarea.value.slice(0, cursorPos);
    const match = textBeforeCursor.match(/@(\w*)$/);
    if (match) {
      active = true;
      query = match[1];
    } else {
      active = false;
      query = "";
    }
  }

  function insertMention(pubkey: string) {
    const textarea = document.querySelector(
      textareaSelector,
    ) as HTMLTextAreaElement;
    const cursorPos = textarea.selectionStart;
    const content = getContent();
    const textBeforeCursor = content.slice(0, cursorPos);
    const textAfterCursor = content.slice(cursorPos);
    const match = textBeforeCursor.match(/@(\w*)$/);
    if (match) {
      const beforeMention = textBeforeCursor.slice(0, match.index);
      setContent(`${beforeMention}@${pubkey} ${textAfterCursor}`);
    }
    active = false;
    query = "";
    textarea.focus();
  }

  return {
    get query() {
      return query;
    },
    get active() {
      return active;
    },
    handleInput,
    insertMention,
  };
}

// --- useFileUpload ---

export function useFileUpload() {
  let attachments = $state<PendingAttachment[]>([]);
  let uploading = $state(false);
  let errorMessage = $state("");

  async function addFiles(files: FileList) {
    if (files.length === 0) return;
    uploading = true;
    try {
      const uploaded = await uploadFiles(files);
      attachments = [...attachments, ...uploaded];
    } catch (err) {
      errorMessage = "Failed to upload file";
      console.error("Failed to upload file:", err);
      setTimeout(() => (errorMessage = ""), 4000);
    }
    uploading = false;
  }

  async function addFilesFromPaths(paths: string[]) {
    if (paths.length === 0) return;
    uploading = true;
    try {
      for (const path of paths) {
        const result: {
          hash: string;
          ticket: string;
          filename: string;
          size: number;
          mime_type: string;
        } = await invoke("add_blob_from_path", { path });
        let previewUrl = "";
        if (isImage(result.mime_type) || isVideo(result.mime_type)) {
          const bytes = await readFile(path);
          const blob = new Blob([bytes], { type: result.mime_type });
          previewUrl = URL.createObjectURL(blob);
        }
        attachments = [
          ...attachments,
          {
            hash: result.hash,
            ticket: result.ticket,
            mime_type: result.mime_type,
            filename: result.filename,
            size: result.size,
            previewUrl,
          },
        ];
      }
    } catch (err) {
      errorMessage = "Failed to upload file";
      console.error("Failed to upload file:", err);
      setTimeout(() => (errorMessage = ""), 4000);
    }
    uploading = false;
  }

  async function addImageFromRgba(
    rgba: Uint8Array,
    width: number,
    height: number,
  ) {
    uploading = true;
    try {
      const result: {
        hash: string;
        ticket: string;
        filename: string;
        size: number;
        mime_type: string;
      } = await invoke("add_blob_from_rgba", {
        data: Array.from(rgba),
        width,
        height,
      });
      // Build a preview from the RGBA data
      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const ctx = canvas.getContext("2d")!;
      const imageData = new ImageData(
        new Uint8ClampedArray(rgba),
        width,
        height,
      );
      ctx.putImageData(imageData, 0, 0);
      const previewUrl = canvas.toDataURL("image/png");
      attachments = [
        ...attachments,
        {
          hash: result.hash,
          ticket: result.ticket,
          mime_type: result.mime_type,
          filename: result.filename,
          size: result.size,
          previewUrl,
        },
      ];
    } catch (err) {
      errorMessage = "Failed to upload clipboard image";
      console.error("Failed to upload clipboard image:", err);
      setTimeout(() => (errorMessage = ""), 4000);
    }
    uploading = false;
  }

  async function handleFiles(e: Event) {
    const input = e.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) return;
    await addFiles(files);
    input.value = "";
  }

  function removeAttachment(index: number) {
    const removed = attachments[index];
    if (removed) URL.revokeObjectURL(removed.previewUrl);
    attachments = attachments.filter((_, i) => i !== index);
  }

  function revokeAll() {
    for (const a of attachments) URL.revokeObjectURL(a.previewUrl);
    attachments = [];
  }

  function clear() {
    revokeAll();
    errorMessage = "";
  }

  return {
    get attachments() {
      return attachments;
    },
    get uploading() {
      return uploading;
    },
    get errorMessage() {
      return errorMessage;
    },
    handleFiles,
    addFiles,
    addFilesFromPaths,
    addImageFromRgba,
    removeAttachment,
    revokeAll,
    clear,
  };
}

// --- useDeleteConfirm ---

export function useDeleteConfirm(onDelete: (id: string) => Promise<void>) {
  let pendingId = $state<string | null>(null);

  function confirm(id: string) {
    pendingId = id;
  }

  async function execute() {
    if (!pendingId) return;
    await onDelete(pendingId);
    pendingId = null;
  }

  function cancel() {
    pendingId = null;
  }

  return {
    get pendingId() {
      return pendingId;
    },
    confirm,
    execute,
    cancel,
  };
}

// --- usePullToRefresh ---

export function usePullToRefresh(onRefresh: () => Promise<void>) {
  let pullStartY = 0;
  let pullDistance = $state(0);
  let isPulling = $state(false);
  let pullTriggered = $state(false);
  const PULL_THRESHOLD = 80;

  function handleTouchStart(e: TouchEvent) {
    if (window.scrollY === 0) {
      pullStartY = e.touches[0].clientY;
      isPulling = true;
    }
  }

  function handleTouchMove(e: TouchEvent) {
    if (!isPulling) return;
    const delta = e.touches[0].clientY - pullStartY;
    if (delta > 0) {
      pullDistance = Math.min(delta * 0.5, 120);
      pullTriggered = pullDistance >= PULL_THRESHOLD;
    } else {
      pullDistance = 0;
      isPulling = false;
    }
  }

  async function handleTouchEnd() {
    if (isPulling && pullTriggered) {
      await onRefresh();
    }
    pullDistance = 0;
    isPulling = false;
    pullTriggered = false;
  }

  return {
    get pullDistance() {
      return pullDistance;
    },
    get isPulling() {
      return isPulling;
    },
    get pullTriggered() {
      return pullTriggered;
    },
    handleTouchStart,
    handleTouchMove,
    handleTouchEnd,
  };
}

// --- useLightbox ---

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
