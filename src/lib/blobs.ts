import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";
import { setContext, getContext } from "svelte";
import type { MediaAttachment } from "$lib/types";

export type BlobCache = ReturnType<typeof createBlobCache>;

const BLOB_CTX = Symbol("blob-cache");

export function setBlobContext(cache: BlobCache): void {
  setContext(BLOB_CTX, cache);
}

export function getBlobContext(): BlobCache {
  return getContext<BlobCache>(BLOB_CTX);
}

export function createBlobCache() {
  const cache = new Map<string, string>();

  async function getBlobUrl(attachment: MediaAttachment): Promise<string> {
    const cached = cache.get(attachment.hash);
    if (cached) return cached;
    const bytes: number[] = await invoke("fetch_blob_bytes", {
      ticket: attachment.ticket,
    });
    const blob = new Blob([new Uint8Array(bytes)], {
      type: attachment.mime_type,
    });
    const url = URL.createObjectURL(blob);
    cache.set(attachment.hash, url);
    return url;
  }

  async function refetchBlobUrl(attachment: MediaAttachment): Promise<string> {
    const old = cache.get(attachment.hash);
    if (old) URL.revokeObjectURL(old);
    cache.delete(attachment.hash);
    const bytes: number[] = await invoke("refetch_blob_bytes", {
      ticket: attachment.ticket,
    });
    const blob = new Blob([new Uint8Array(bytes)], {
      type: attachment.mime_type,
    });
    const url = URL.createObjectURL(blob);
    cache.set(attachment.hash, url);
    return url;
  }

  async function saveFileAs(att: MediaAttachment): Promise<void> {
    const path = await save({ defaultPath: att.filename });
    if (!path) return;
    const bytes: number[] = await invoke("fetch_blob_bytes", {
      ticket: att.ticket,
    });
    await writeFile(path, new Uint8Array(bytes));
  }

  function revokeAll(): void {
    for (const url of cache.values()) URL.revokeObjectURL(url);
    cache.clear();
  }

  function revokeStale(activeHashes: Set<string>): void {
    for (const [hash, url] of cache) {
      if (!activeHashes.has(hash)) {
        URL.revokeObjectURL(url);
        cache.delete(hash);
      }
    }
  }

  return {
    getBlobUrl,
    refetchBlobUrl,
    saveFileAs,
    revokeAll,
    revokeStale,
  };
}
