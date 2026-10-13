import { platform } from "@tauri-apps/plugin-os";
import type { MediaAttachment, PendingAttachment } from "$lib/types";

/** Byte limit of post and message text (`MAX_POST_CONTENT_LEN` in Rust). */
export const MAX_CONTENT_BYTES = 10_000;

const encoder = new TextEncoder();

/** Length of `text` in UTF-8 bytes. */
export function utf8Length(text: string): number {
  return encoder.encode(text).length;
}

/** True on Android and iOS. */
export function isMobilePlatform(): boolean {
  const name = platform();
  return name === "android" || name === "ios";
}

/**
 * True if `e` is an Enter keydown that submits the composer: Shift is not
 * held, no IME composition is active, and the platform is not Android or iOS
 * (their keyboards have no Shift+Enter, so Enter inserts a newline there).
 */
export function isSubmitEnter(e: KeyboardEvent, isMobile: boolean): boolean {
  return (
    e.key === "Enter" &&
    !e.shiftKey &&
    !e.isComposing &&
    e.keyCode !== 229 &&
    !isMobile
  );
}

/** Copies the fields of each pending attachment that the backend stores. */
export function toMediaAttachments(
  pending: PendingAttachment[],
): MediaAttachment[] {
  return pending.map(({ hash, ticket, mime_type, filename, size }) => ({
    hash,
    ticket,
    mime_type,
    filename,
    size,
  }));
}
