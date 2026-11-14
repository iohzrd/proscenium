import { invoke } from "@tauri-apps/api/core";
import { platform } from "@tauri-apps/plugin-os";
import { SvelteMap } from "svelte/reactivity";
import type { Profile, PendingAttachment } from "#lib/types.ts";

/** Upload size limit in bytes; equals `MAX_BLOB_SIZE` in the types crate. */
export const MAX_BLOB_SIZE = 50 * 1024 * 1024;

/** Attachment limit per post or message; equals `MAX_MEDIA_COUNT` in the types crate. */
export const MAX_ATTACHMENTS = 10;

/**
 * Discovery server offered on the profile setup screen. The server's configured
 * `public_url` must equal this URL: registration requests name it and are signed.
 */
export const DEFAULT_DISCOVERY_SERVER = "https://discovery.identia.app";

const AVATAR_COLORS = [
  "#c24b3f",
  "#4f7fc0",
  "#5f9a5a",
  "#d0853a",
  "#8a6fc4",
  "#c4628c",
  "#6272b8",
  "#40978f",
];

export function avatarColor(pubkey: string): string {
  let hash = 0;
  for (let i = 0; i < pubkey.length; i++) {
    hash = pubkey.charCodeAt(i) + ((hash << 5) - hash);
  }
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length];
}

/** First `max` code points of `text`. */
export function truncate(text: string, max: number): string {
  if (text.length <= max) return text;
  const codePoints = Array.from(text);
  return codePoints.length <= max ? text : codePoints.slice(0, max).join("");
}

export function getInitials(name: string, isSelf = false): string {
  if (!name || isSelf) return "Y";
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length >= 2) {
    return (truncate(parts[0], 1) + truncate(parts[1], 1)).toUpperCase();
  }
  return truncate(name.trim(), 2).toUpperCase();
}

export function shortId(id: string): string {
  const codePoints = Array.from(id);
  if (codePoints.length <= 12) return id;
  return (
    codePoints.slice(0, 8).join("") + "..." + codePoints.slice(-4).join("")
  );
}

const PUBKEY_PATTERN = /^[0-9a-f]{64}$/;

/** True if `s` is 64 lowercase hex digits: the text form of a public key or Node ID. */
export function isPubkeyFormat(s: string): boolean {
  return PUBKEY_PATTERN.test(s);
}

const POST_ID_PATTERN = /^[0-9a-f]{32}$/;

/** True if `s` is 32 lowercase hex digits: the text form of a post id. */
export function isPostIdFormat(s: string): boolean {
  return POST_ID_PATTERN.test(s);
}

/** True if the document is visible and its window has input focus. */
export function isWindowActive(): boolean {
  return !document.hidden && document.hasFocus();
}

// Shared profile cache (name + avatar) and resolver
interface CachedProfile {
  name: string;
  avatarTicket: string | null;
}

// Reactive: a template or effect that reads an entry runs again when the entry changes.
const profileCache = new SvelteMap<string, CachedProfile>();
const pendingLookups = new Map<string, Promise<string>>();
// Incremented per pubkey by evictDisplayName; a lookup started before the
// increment does not write its result to the cache.
const evictionCounts = new Map<string, number>();

export function evictDisplayName(pubkey: string) {
  evictionCounts.set(pubkey, (evictionCounts.get(pubkey) ?? 0) + 1);
  pendingLookups.delete(pubkey);
  profileCache.delete(pubkey);
}

async function lookupDisplayName(pubkey: string): Promise<string> {
  const evictions = evictionCounts.get(pubkey) ?? 0;
  try {
    const profile = await invoke<Profile | null>("get_remote_profile", {
      pubkey,
    });
    const name = profile?.display_name || shortId(pubkey);
    if ((evictionCounts.get(pubkey) ?? 0) === evictions) {
      profileCache.set(pubkey, {
        name,
        avatarTicket: profile?.avatar_ticket ?? null,
      });
    }
    return name;
  } catch {
    // Not cached: the next call repeats the lookup.
    return shortId(pubkey);
  }
}

export function getDisplayName(
  pubkey: string,
  selfId: string,
): Promise<string> {
  if (pubkey === selfId) return Promise.resolve("You");
  const cached = profileCache.get(pubkey);
  if (cached !== undefined) return Promise.resolve(cached.name);
  let pending = pendingLookups.get(pubkey);
  if (!pending) {
    const lookup = lookupDisplayName(pubkey).finally(() => {
      if (pendingLookups.get(pubkey) === lookup) pendingLookups.delete(pubkey);
    });
    pending = lookup;
    pendingLookups.set(pubkey, lookup);
  }
  return pending;
}

/** Cached display name of `pubkey`, or undefined if no lookup has completed. */
export function getCachedDisplayName(pubkey: string): string | undefined {
  return profileCache.get(pubkey)?.name;
}

export function getCachedAvatarTicket(pubkey: string): string | null {
  return profileCache.get(pubkey)?.avatarTicket ?? null;
}

export async function seedOwnProfile(pubkey: string): Promise<void> {
  try {
    const profile = (await invoke("get_my_profile")) as Profile | null;
    profileCache.set(pubkey, {
      name: "You",
      avatarTicket: profile?.avatar_ticket ?? null,
    });
  } catch {
    // ignore
  }
}

export async function copyToClipboard(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}

// Shared helpers extracted from +page.svelte

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function renderContent(text: string, selfId: string): string {
  const urlPattern = /https?:\/\/[^\s<>"')\]]+/g;
  // Exactly 64 hex digits; a longer hex run is not a mention.
  const mentionPattern = /@([0-9a-fA-F]{64})(?![0-9a-fA-F])/g;
  const trailingPunctuation = /[.,!?;:]+$/;

  interface ContentMatch {
    start: number;
    end: number;
    type: "url" | "mention";
    text: string;
    pubkey?: string;
  }

  const matches: ContentMatch[] = [];

  let m;
  while ((m = urlPattern.exec(text)) !== null) {
    const url = m[0].replace(trailingPunctuation, "");
    // Skip a scheme with no host, such as "https://" followed by punctuation.
    if (!/^https?:\/\/[^/]/.test(url)) continue;
    matches.push({
      start: m.index,
      end: m.index + url.length,
      type: "url",
      text: url,
    });
  }
  while ((m = mentionPattern.exec(text)) !== null) {
    matches.push({
      start: m.index,
      end: m.index + m[0].length,
      type: "mention",
      text: m[0],
      pubkey: m[1].toLowerCase(),
    });
  }

  matches.sort((a, b) => a.start - b.start);
  const filtered: ContentMatch[] = [];
  let lastEnd = 0;
  for (const match of matches) {
    if (match.start >= lastEnd) {
      filtered.push(match);
      lastEnd = match.end;
    }
  }

  const parts: string[] = [];
  let lastIndex = 0;
  for (const match of filtered) {
    if (match.start > lastIndex) {
      parts.push(escapeHtml(text.slice(lastIndex, match.start)));
    }
    if (match.type === "url") {
      parts.push(
        `<a href="${escapeHtml(match.text)}" target="_blank" rel="noopener noreferrer">${escapeHtml(match.text)}</a>`,
      );
    } else if (match.type === "mention" && match.pubkey) {
      const cached = profileCache.get(match.pubkey);
      const displayName =
        match.pubkey === selfId
          ? "You"
          : (cached?.name ?? shortId(match.pubkey));
      parts.push(
        `<a href="/profile/${escapeHtml(match.pubkey)}" class="mention">@${escapeHtml(displayName)}</a>`,
      );
      if (!cached && match.pubkey !== selfId) {
        getDisplayName(match.pubkey, selfId);
      }
    }
    lastIndex = match.end;
  }
  if (lastIndex < text.length) {
    parts.push(escapeHtml(text.slice(lastIndex)));
  }
  return parts.join("");
}

/** MIME type without parameters, lowercase. */
export function normalizeMime(mime: string): string {
  return mime.split(";")[0].trim().toLowerCase();
}

// Raster formats only. `image/svg+xml` is a document format that can hold script.
const INLINE_IMAGE_TYPES = new Set([
  "image/png",
  "image/apng",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/avif",
  "image/bmp",
]);

export function isImage(mime: string): boolean {
  return INLINE_IMAGE_TYPES.has(normalizeMime(mime));
}

export function isVideo(mime: string): boolean {
  return normalizeMime(mime).startsWith("video/");
}

export function isAudio(mime: string): boolean {
  return normalizeMime(mime).startsWith("audio/");
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1048576) return (bytes / 1024).toFixed(1) + " KB";
  return (bytes / 1048576).toFixed(1) + " MB";
}

/** Distance below the viewport at which the infinite-scroll sentinel counts as reached. */
export const SENTINEL_MARGIN_PX = 200;

/**
 * Sets up an IntersectionObserver for infinite scroll on a sentinel element.
 * Call from within a $effect; returns a cleanup function.
 */
export function setupInfiniteScroll(
  sentinel: HTMLElement | null,
  getHasMore: () => boolean,
  getLoadingMore: () => boolean,
  loadMore: () => void,
): (() => void) | undefined {
  if (!sentinel) return;
  const observer = new IntersectionObserver(
    (entries) => {
      // One callback can deliver several records for the sentinel, oldest
      // first. The last record holds the current intersection state.
      const latest = entries[entries.length - 1];
      if (latest?.isIntersecting && getHasMore() && !getLoadingMore()) {
        loadMore();
      }
    },
    { rootMargin: `0px 0px ${SENTINEL_MARGIN_PX}px 0px` },
  );
  observer.observe(sentinel);
  return () => observer.disconnect();
}

/** Rows with a `(timestamp, id)` position, as used by the paginated commands. */
export interface Positioned {
  id: string;
  timestamp: number;
}

/** Negative if `a` precedes `b` in ascending `(timestamp, id)` order. */
export function comparePosition(a: Positioned, b: Positioned): number {
  if (a.timestamp !== b.timestamp) return a.timestamp - b.timestamp;
  if (a.id === b.id) return 0;
  return a.id < b.id ? -1 : 1;
}

/** `items` without later elements whose key equals that of an earlier element. */
export function dedupeBy<T>(items: T[], key: (item: T) => string): T[] {
  const seen = new Set<string>();
  const result: T[] = [];
  for (const item of items) {
    const k = key(item);
    if (seen.has(k)) continue;
    seen.add(k);
    result.push(item);
  }
  return result;
}

export function dedupeById<T extends { id: string }>(items: T[]): T[] {
  return dedupeBy(items, (item) => item.id);
}

/**
 * Merge `fresh`, the first `limit` rows of a query, into `existing`, rows
 * loaded earlier from the same query. Rows of `existing` positioned after the
 * last row of `fresh` in query order are kept; all others are replaced by
 * `fresh`. `exhausted` is true if `fresh` holds every row of the query.
 */
export function mergeFirstPage<T extends Positioned>(
  existing: T[],
  fresh: T[],
  limit: number,
  order: "asc" | "desc",
): { items: T[]; exhausted: boolean; keptExisting: boolean } {
  const head = dedupeById(fresh);
  if (fresh.length < limit || head.length === 0) {
    return { items: head, exhausted: true, keptExisting: false };
  }
  const last = head[head.length - 1];
  const headIds = new Set(head.map((item) => item.id));
  const rest = existing.filter((item) => {
    if (headIds.has(item.id)) return false;
    const cmp = comparePosition(item, last);
    return order === "asc" ? cmp > 0 : cmp < 0;
  });
  return {
    items: [...head, ...dedupeById(rest)],
    exhausted: false,
    keptExisting: rest.length > 0,
  };
}

/**
 * Wrap `fn` so that calls never overlap: a call made while `fn` is running
 * schedules exactly one more run after the current one finishes.
 */
export function coalesce(fn: () => Promise<void>): () => Promise<void> {
  let running: Promise<void> | null = null;
  let again = false;
  return () => {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      try {
        do {
          again = false;
          await fn();
        } while (again);
      } finally {
        running = null;
      }
    })();
    return running;
  };
}

function revokePreviews(attachments: PendingAttachment[]) {
  for (const a of attachments) {
    if (a.previewUrl.startsWith("blob:")) URL.revokeObjectURL(a.previewUrl);
  }
}

/** Bytes per `append_blob_upload` call; equals `MAX_UPLOAD_CHUNK` in the backend. */
const UPLOAD_CHUNK_BYTES = 1024 * 1024;

/** Base64 of `bytes`, built in slices so no call takes one argument per byte. */
function toBase64(bytes: Uint8Array): string {
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/**
 * Add `blob` to the blob store in chunks of `UPLOAD_CHUNK_BYTES`
 * (`begin_blob_upload`, `append_blob_upload`, `finish_blob_upload`), so no IPC
 * message holds the whole file and only one chunk is read into memory at a time.
 * A chunk is a raw body on desktop; on Android, whose IPC encodes a byte array
 * as a JSON array of numbers, it is base64. A failed upload is aborted.
 */
export async function uploadBlob(
  blob: Blob,
): Promise<{ hash: string; ticket: string }> {
  const uploadId = await invoke<string>("begin_blob_upload", {
    size: blob.size,
  });
  try {
    const android = platform() === "android";
    const headers = { "upload-id": uploadId };
    for (let offset = 0; offset < blob.size; offset += UPLOAD_CHUNK_BYTES) {
      const chunk = new Uint8Array(
        await blob.slice(offset, offset + UPLOAD_CHUNK_BYTES).arrayBuffer(),
      );
      await invoke(
        "append_blob_upload",
        android ? { chunk: toBase64(chunk) } : chunk,
        { headers },
      );
    }
    return await invoke("finish_blob_upload", { uploadId });
  } catch (e) {
    await invoke("abort_blob_upload", { uploadId }).catch(() => {});
    throw e;
  }
}

/**
 * Add each file to the blob store. Throws before reading any file if one
 * exceeds `MAX_BLOB_SIZE`. If an upload fails, the preview URLs created for
 * the earlier files are revoked before the error is rethrown.
 */
export async function uploadFiles(
  files: Iterable<File>,
): Promise<PendingAttachment[]> {
  const list = Array.from(files);
  for (const file of list) {
    if (file.size > MAX_BLOB_SIZE) {
      throw new Error(
        `${truncate(file.name, 64)} is larger than ${formatSize(MAX_BLOB_SIZE)}`,
      );
    }
  }
  const results: PendingAttachment[] = [];
  let completed = false;
  try {
    for (const file of list) {
      const result = await uploadBlob(file);
      results.push({
        hash: result.hash,
        ticket: result.ticket,
        mime_type: file.type || "application/octet-stream",
        filename: file.name,
        size: file.size,
        previewUrl: URL.createObjectURL(file),
      });
    }
    completed = true;
    return results;
  } finally {
    if (!completed) revokePreviews(results);
  }
}

export function detectImageMime(data: Uint8Array): string {
  if (data[0] === 0x89 && data[1] === 0x50) return "image/png";
  if (data[0] === 0xff && data[1] === 0xd8) return "image/jpeg";
  if (data[0] === 0x47 && data[1] === 0x49) return "image/gif";
  if (
    data[0] === 0x52 &&
    data[1] === 0x49 &&
    data[8] === 0x57 &&
    data[9] === 0x45
  )
    return "image/webp";
  return "image/png";
}

/** Toast text and type for a `follow_user` result that stored no follow. */
export function followRequestNotice(
  outcome: "pending" | "denied",
): [string, "success" | "error"] {
  return outcome === "pending"
    ? [
        "Follow request sent. Their posts appear once they approve it.",
        "success",
      ]
    : ["Follow request denied", "error"];
}
