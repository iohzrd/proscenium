import { invoke } from "@tauri-apps/api/core";
import type { PendingAttachment } from "#lib/types.ts";
import {
  MAX_ATTACHMENTS,
  formatSize,
  isImage,
  isVideo,
  normalizeMime,
  uploadFiles,
} from "#lib/utils.ts";

const ERROR_DURATION_MS = 4000;
// Largest raw RGBA buffer accepted from the clipboard (4096 x 4096 pixels).
const MAX_RGBA_BYTES = 4096 * 4096 * 4;

interface AddedBlob {
  hash: string;
  ticket: string;
  filename: string;
  size: number;
  mime_type: string;
}

function revokePreview(attachment: PendingAttachment) {
  if (attachment.previewUrl.startsWith("blob:")) {
    URL.revokeObjectURL(attachment.previewUrl);
  }
}

/**
 * Pending attachments of one composer. At most `MAX_ATTACHMENTS` are kept, an
 * upload whose hash is already attached is discarded, and an upload that
 * completes after `clear()` or `revokeAll()` is discarded.
 */
export function useFileUpload() {
  let attachments = $state<PendingAttachment[]>([]);
  let activeUploads = $state(0);
  let errorMessage = $state("");
  let errorTimer: ReturnType<typeof setTimeout> | null = null;
  // Incremented by clear() and revokeAll(); identifies the attachment list an upload belongs to.
  let generation = 0;

  function showError(message: string) {
    errorMessage = message;
    if (errorTimer) clearTimeout(errorTimer);
    errorTimer = setTimeout(() => {
      errorMessage = "";
      errorTimer = null;
    }, ERROR_DURATION_MS);
  }

  /** Number of further attachments the list accepts; shows an error if it is zero. */
  function remainingSlots(): number {
    const remaining = MAX_ATTACHMENTS - attachments.length;
    if (remaining <= 0) showError(`At most ${MAX_ATTACHMENTS} attachments`);
    return Math.max(remaining, 0);
  }

  /** Append `uploaded` if it belongs to generation `startedIn`; else revoke its previews. */
  function accept(startedIn: number, uploaded: PendingAttachment[]) {
    for (const item of uploaded) {
      const outdated = startedIn !== generation;
      const duplicate = attachments.some((a) => a.hash === item.hash);
      const full = attachments.length >= MAX_ATTACHMENTS;
      if (outdated || duplicate || full) {
        revokePreview(item);
        if (!outdated && full) {
          showError(`At most ${MAX_ATTACHMENTS} attachments`);
        }
        continue;
      }
      attachments = [...attachments, item];
    }
  }

  async function addFiles(files: Iterable<File>) {
    const list = Array.from(files);
    if (list.length === 0) return;
    const slots = remainingSlots();
    if (slots === 0) return;
    const startedIn = generation;
    activeUploads += 1;
    try {
      const uploaded = await uploadFiles(list.slice(0, slots));
      accept(startedIn, uploaded);
      if (list.length > slots && startedIn === generation) {
        showError(`At most ${MAX_ATTACHMENTS} attachments`);
      }
    } catch (err) {
      if (startedIn === generation) {
        showError(err instanceof Error ? err.message : "Failed to upload file");
      }
      console.error("Failed to upload file:", err);
    } finally {
      activeUploads -= 1;
    }
  }

  /** Object URL for the preview of a blob that is in the local store. */
  async function localPreviewUrl(blob: AddedBlob): Promise<string> {
    if (!isImage(blob.mime_type) && !isVideo(blob.mime_type)) return "";
    try {
      const bytes = await invoke<ArrayBuffer>("fetch_blob_bytes", {
        ticket: blob.ticket,
      });
      return URL.createObjectURL(
        new Blob([bytes], { type: normalizeMime(blob.mime_type) }),
      );
    } catch (err) {
      console.error("Failed to build attachment preview:", err);
      return "";
    }
  }

  async function addFilesFromPaths(paths: string[]) {
    if (paths.length === 0) return;
    const slots = remainingSlots();
    if (slots === 0) return;
    const startedIn = generation;
    activeUploads += 1;
    try {
      for (const path of paths.slice(0, slots)) {
        const result = await invoke<AddedBlob>("add_blob_from_path", { path });
        const previewUrl = await localPreviewUrl(result);
        accept(startedIn, [
          {
            hash: result.hash,
            ticket: result.ticket,
            mime_type: result.mime_type,
            filename: result.filename,
            size: result.size,
            previewUrl,
          },
        ]);
        if (startedIn !== generation) return;
      }
      if (paths.length > slots) {
        showError(`At most ${MAX_ATTACHMENTS} attachments`);
      }
    } catch (err) {
      if (startedIn === generation) showError("Failed to upload file");
      console.error("Failed to upload file:", err);
    } finally {
      activeUploads -= 1;
    }
  }

  async function addImageFromRgba(
    rgba: Uint8Array,
    width: number,
    height: number,
  ) {
    if (remainingSlots() === 0) return;
    if (rgba.byteLength !== width * height * 4) {
      showError("Clipboard image has an invalid size");
      return;
    }
    if (rgba.byteLength > MAX_RGBA_BYTES) {
      showError(
        `Clipboard image is ${formatSize(rgba.byteLength)} uncompressed; the limit is ${formatSize(MAX_RGBA_BYTES)}`,
      );
      return;
    }
    const startedIn = generation;
    activeUploads += 1;
    try {
      const result = await invoke<AddedBlob>("add_blob_from_rgba", {
        data: rgba,
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
      accept(startedIn, [
        {
          hash: result.hash,
          ticket: result.ticket,
          mime_type: result.mime_type,
          filename: result.filename,
          size: result.size,
          previewUrl,
        },
      ]);
    } catch (err) {
      if (startedIn === generation) {
        showError("Failed to upload clipboard image");
      }
      console.error("Failed to upload clipboard image:", err);
    } finally {
      activeUploads -= 1;
    }
  }

  async function handleFiles(e: Event) {
    const input = e.target as HTMLInputElement;
    const files = input.files;
    if (!files || files.length === 0) return;
    // Copy the selection: clearing `value` empties the live FileList.
    const selected = Array.from(files);
    input.value = "";
    await addFiles(selected);
  }

  function removeAttachment(index: number) {
    const removed = attachments[index];
    if (removed) revokePreview(removed);
    attachments = attachments.filter((_, i) => i !== index);
  }

  function revokeAll() {
    generation += 1;
    for (const a of attachments) revokePreview(a);
    attachments = [];
  }

  function clear() {
    revokeAll();
    if (errorTimer) clearTimeout(errorTimer);
    errorTimer = null;
    errorMessage = "";
  }

  return {
    get attachments() {
      return attachments;
    },
    get uploading() {
      return activeUploads > 0;
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
