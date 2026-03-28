import { invoke } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import type { PendingAttachment } from "$lib/types";
import { isImage, isVideo, uploadFiles } from "$lib/utils";

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
