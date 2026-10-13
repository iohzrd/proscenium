import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";
import { setContext, getContext } from "svelte";
import type { MediaAttachment } from "$lib/types";
import { isAudio, isImage, isVideo, normalizeMime } from "$lib/utils";

export type BlobCache = ReturnType<typeof createBlobCache>;

const BLOB_CTX = Symbol("blob-cache");

export function setBlobContext(cache: BlobCache): void {
  setContext(BLOB_CTX, cache);
}

export function getBlobContext(): BlobCache {
  return getContext<BlobCache>(BLOB_CTX);
}

/** Largest declared attachment size in bytes that is fetched without a click. */
export const AUTO_FETCH_MAX_BYTES = 10 * 1024 * 1024;

/**
 * True if the attachment must not be fetched or rendered until the user asks
 * for it: its declared size exceeds `AUTO_FETCH_MAX_BYTES` or is not a
 * non-negative number.
 */
export function requiresClickToLoad(
  attachment: Pick<MediaAttachment, "size">,
): boolean {
  const size = attachment.size;
  return !(
    typeof size === "number" &&
    size >= 0 &&
    size <= AUTO_FETCH_MAX_BYTES
  );
}

const MAX_FILENAME_CHARS = 128;
const MAX_EXTENSION_CHARS = 16;

function isControlOrBidi(codePoint: number): boolean {
  return (
    codePoint <= 0x1f ||
    (codePoint >= 0x7f && codePoint <= 0x9f) ||
    codePoint === 0x200e ||
    codePoint === 0x200f ||
    (codePoint >= 0x202a && codePoint <= 0x202e) ||
    (codePoint >= 0x2066 && codePoint <= 0x2069)
  );
}

/**
 * Reduce a remote-supplied file name to a base name for a save dialog: the
 * last path component, without control or bidirectional-override characters,
 * without characters Windows rejects, without leading dots, at most
 * `MAX_FILENAME_CHARS` code points. Returns "download" if nothing remains.
 */
export function sanitizeFilename(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? "";
  const cleaned = Array.from(base)
    .filter((ch) => !isControlOrBidi(ch.codePointAt(0) ?? 0))
    .join("")
    .replace(/[<>:"|?*]/g, "_")
    .replace(/^[.\s]+/, "")
    .replace(/[.\s]+$/, "");
  const codePoints = Array.from(cleaned);
  if (codePoints.length === 0) return "download";
  if (codePoints.length <= MAX_FILENAME_CHARS) return cleaned;
  const dot = codePoints.lastIndexOf(".");
  const extension =
    dot > 0 && codePoints.length - dot <= MAX_EXTENSION_CHARS
      ? codePoints.slice(dot)
      : [];
  return codePoints
    .slice(0, MAX_FILENAME_CHARS - extension.length)
    .concat(extension)
    .join("");
}

/**
 * Blob type for an object URL: the declared MIME type if it is rendered
 * inline (raster image, video, audio), else `application/octet-stream`.
 */
function blobType(mime: string): string {
  return isImage(mime) || isVideo(mime) || isAudio(mime)
    ? normalizeMime(mime)
    : "application/octet-stream";
}

export interface BlobCacheOptions {
  /** Called with a message when `saveFileAs` fails. */
  onError?: (message: string) => void;
}

export function createBlobCache(options: BlobCacheOptions = {}) {
  // Object URLs by blob type and ticket. The remote-declared `hash` field is
  // not part of the key: nothing compares it with the ticket.
  const cache = new Map<string, string>();
  const pending = new Map<string, Promise<string>>();
  // Incremented by revokeAll; a fetch started before the increment creates no URL.
  let generation = 0;

  function cacheKey(attachment: MediaAttachment): string {
    return `${blobType(attachment.mime_type)}\n${attachment.ticket}`;
  }

  async function load(
    command: "fetch_blob_bytes" | "refetch_blob_bytes",
    attachment: MediaAttachment,
  ): Promise<string> {
    const startedIn = generation;
    const bytes = await invoke<ArrayBuffer>(command, {
      ticket: attachment.ticket,
    });
    if (startedIn !== generation) throw new Error("blob cache was cleared");
    const key = cacheKey(attachment);
    const blob = new Blob([bytes], { type: blobType(attachment.mime_type) });
    const url = URL.createObjectURL(blob);
    const previous = cache.get(key);
    if (previous) URL.revokeObjectURL(previous);
    cache.set(key, url);
    return url;
  }

  /** Run `start` unless a load for the same key is pending; then return that load. */
  function loadOnce(
    key: string,
    start: () => Promise<string>,
  ): Promise<string> {
    const running = pending.get(key);
    if (running) return running;
    const started = start().finally(() => {
      if (pending.get(key) === started) pending.delete(key);
    });
    pending.set(key, started);
    return started;
  }

  function getBlobUrl(attachment: MediaAttachment): Promise<string> {
    const key = cacheKey(attachment);
    const cached = cache.get(key);
    if (cached) return Promise.resolve(cached);
    return loadOnce(key, () => load("fetch_blob_bytes", attachment));
  }

  function refetchBlobUrl(attachment: MediaAttachment): Promise<string> {
    const key = cacheKey(attachment);
    const old = cache.get(key);
    if (old) URL.revokeObjectURL(old);
    cache.delete(key);
    return loadOnce(`refetch\n${key}`, () =>
      load("refetch_blob_bytes", attachment),
    );
  }

  /** Never rejects: a failure is logged and passed to `options.onError`. */
  async function saveFileAs(att: MediaAttachment): Promise<void> {
    try {
      const path = await save({ defaultPath: sanitizeFilename(att.filename) });
      if (!path) return;
      const bytes = await invoke<ArrayBuffer>("fetch_blob_bytes", {
        ticket: att.ticket,
      });
      await writeFile(path, new Uint8Array(bytes));
    } catch (e) {
      console.error("Failed to save file:", e);
      options.onError?.("Failed to save file");
    }
  }

  function revokeAll(): void {
    generation += 1;
    for (const url of cache.values()) URL.revokeObjectURL(url);
    cache.clear();
    pending.clear();
  }

  return {
    getBlobUrl,
    refetchBlobUrl,
    saveFileAs,
    revokeAll,
  };
}
