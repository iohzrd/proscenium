import { invoke } from "@tauri-apps/api/core";
import type { Profile, PendingAttachment } from "$lib/types";

const AVATAR_COLORS = [
  "#dc2626",
  "#2563eb",
  "#059669",
  "#d97706",
  "#7c3aed",
  "#db2777",
  "#4f46e5",
  "#0891b2",
];

export function avatarColor(pubkey: string): string {
  let hash = 0;
  for (let i = 0; i < pubkey.length; i++) {
    hash = pubkey.charCodeAt(i) + ((hash << 5) - hash);
  }
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length];
}

export function getInitials(name: string, isSelf = false): string {
  if (!name || isSelf) return "Y";
  const parts = name.trim().split(/\s+/);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

export function shortId(id: string): string {
  return id.slice(0, 8) + "..." + id.slice(-4);
}

// Shared profile cache (name + avatar) and resolver
interface CachedProfile {
  name: string;
  avatarTicket: string | null;
}

const profileCache = new Map<string, CachedProfile>();

export function clearDisplayNameCache() {
  profileCache.clear();
}

export function evictDisplayName(pubkey: string) {
  profileCache.delete(pubkey);
}

export async function getDisplayName(
  pubkey: string,
  selfId: string,
): Promise<string> {
  if (pubkey === selfId) return "You";
  const cached = profileCache.get(pubkey);
  if (cached !== undefined) return cached.name;
  try {
    const profile = (await invoke("get_remote_profile", {
      pubkey,
    })) as Profile | null;
    const name =
      profile && profile.display_name ? profile.display_name : shortId(pubkey);
    profileCache.set(pubkey, {
      name,
      avatarTicket: profile?.avatar_ticket ?? null,
    });
    return name;
  } catch {
    const name = shortId(pubkey);
    profileCache.set(pubkey, { name, avatarTicket: null });
    return name;
  }
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

export function linkify(text: string): string {
  return renderContent(text, "");
}

export function renderContent(text: string, selfId: string): string {
  const urlPattern = /https?:\/\/[^\s<>"')\]]+/g;
  const mentionPattern = /@([0-9a-fA-F]{52,64})/g;

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
    matches.push({
      start: m.index,
      end: m.index + m[0].length,
      type: "url",
      text: m[0],
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

export function isImage(mime: string): boolean {
  return mime.startsWith("image/");
}

export function isVideo(mime: string): boolean {
  return mime.startsWith("video/");
}

export function isAudio(mime: string): boolean {
  return mime.startsWith("audio/");
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1048576) return (bytes / 1024).toFixed(1) + " KB";
  return (bytes / 1048576).toFixed(1) + " MB";
}

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
      if (entries[0].isIntersecting && getHasMore() && !getLoadingMore()) {
        loadMore();
      }
    },
    { rootMargin: "0px 0px 200px 0px" },
  );
  observer.observe(sentinel);
  return () => observer.disconnect();
}

export async function uploadFiles(
  files: FileList,
): Promise<PendingAttachment[]> {
  const results: PendingAttachment[] = [];
  for (const file of files) {
    const buffer = await file.arrayBuffer();
    const data = Array.from(new Uint8Array(buffer));
    const result: { hash: string; ticket: string } = await invoke(
      "add_blob_bytes",
      { data },
    );
    const previewUrl = URL.createObjectURL(file);
    results.push({
      hash: result.hash,
      ticket: result.ticket,
      mime_type: file.type || "application/octet-stream",
      filename: file.name,
      size: file.size,
      previewUrl,
    });
  }
  return results;
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
