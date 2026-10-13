import { isPubkeyFormat } from "$lib/utils";

const POST_ID = /^[0-9a-f]{32}$/;

function isPubkey(value: unknown): value is string {
  return typeof value === "string" && isPubkeyFormat(value);
}

/** Path of the thread page of `id`, or null if `id` is not 32 lowercase hex digits. */
export function postHref(id: unknown): string | null {
  return typeof id === "string" && POST_ID.test(id)
    ? `/post/${encodeURIComponent(id)}`
    : null;
}

/** Path of the profile page of `pubkey`, or null if `pubkey` is not 64 lowercase hex digits. */
export function profileHref(pubkey: unknown): string | null {
  return isPubkey(pubkey) ? `/profile/${encodeURIComponent(pubkey)}` : null;
}

/** Path of the conversation with `pubkey`, or null if `pubkey` is not 64 lowercase hex digits. */
export function messagesHref(pubkey: unknown): string | null {
  return isPubkey(pubkey) ? `/messages/${encodeURIComponent(pubkey)}` : null;
}
