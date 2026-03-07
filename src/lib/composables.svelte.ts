import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { onMount } from "svelte";
import type { PendingAttachment } from "$lib/types";
import { copyToClipboard, setupInfiniteScroll, uploadFiles } from "$lib/utils";

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

export function useNodeInit(onReady: (nodeId: string) => Promise<void>) {
  let nodeId = $state("");
  let loading = $state(true);

  async function init() {
    try {
      nodeId = await invoke("get_node_id");
      await onReady(nodeId);
      loading = false;
    } catch {
      setTimeout(init, 500);
    }
  }

  return {
    get nodeId() {
      return nodeId;
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

  async function handleFiles(e: Event) {
    const input = e.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) return;
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

// --- useLightbox ---

export function useLightbox() {
  let src = $state("");
  let alt = $state("");

  function open(s: string, a: string) {
    src = s;
    alt = a;
  }

  function close() {
    src = "";
    alt = "";
  }

  return {
    get src() {
      return src;
    },
    get alt() {
      return alt;
    },
    open,
    close,
  };
}
