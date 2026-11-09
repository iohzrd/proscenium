<script lang="ts" module>
  import { invoke } from "@tauri-apps/api/core";
  import { detectImageMime } from "#lib/utils.ts";

  /** A ticket whose fetch failed is not fetched again for this long. */
  const RETRY_AFTER_MS = 60_000;
  /** Sum of the image sizes the cache keeps; the least recently used are dropped beyond it. */
  const MAX_CACHED_BYTES = 64 * 1024 * 1024;
  const MAX_FAILED_ENTRIES = 256;

  // Shared by all instances: one fetch and one object URL per ticket. The Map
  // order is the use order, oldest first.
  const cache = new Map<string, { url: string; bytes: number }>();
  let cachedBytes = 0;
  const pending = new Map<string, Promise<string>>();
  const failedAt = new Map<string, number>();

  function cachedUrl(ticket: string): string | undefined {
    const entry = cache.get(ticket);
    if (!entry) return undefined;
    cache.delete(ticket);
    cache.set(ticket, entry);
    return entry.url;
  }

  function store(ticket: string, url: string, bytes: number) {
    cache.set(ticket, { url, bytes });
    cachedBytes += bytes;
    for (const [oldTicket, old] of cache) {
      if (cachedBytes <= MAX_CACHED_BYTES || oldTicket === ticket) break;
      cache.delete(oldTicket);
      cachedBytes -= old.bytes;
      URL.revokeObjectURL(old.url);
    }
  }

  function recordFailure(ticket: string) {
    const now = Date.now();
    if (failedAt.size >= MAX_FAILED_ENTRIES) {
      for (const [old, at] of failedAt) {
        if (now - at >= RETRY_AFTER_MS) failedAt.delete(old);
      }
      if (failedAt.size >= MAX_FAILED_ENTRIES) failedAt.clear();
    }
    failedAt.set(ticket, now);
  }

  function fetchAvatarUrl(ticket: string): Promise<string> {
    const running = pending.get(ticket);
    if (running) return running;
    const failed = failedAt.get(ticket);
    if (failed !== undefined && Date.now() - failed < RETRY_AFTER_MS) {
      return Promise.reject(new Error("avatar fetch failed recently"));
    }
    const request = invoke<ArrayBuffer>("fetch_blob_bytes", { ticket })
      .then((buffer) => {
        const data = new Uint8Array(buffer);
        // The Blob type comes from the leading bytes, never from the peer.
        const blob = new Blob([data], { type: detectImageMime(data) });
        const url = URL.createObjectURL(blob);
        store(ticket, url, data.byteLength);
        failedAt.delete(ticket);
        return url;
      })
      .catch((e) => {
        recordFailure(ticket);
        throw e;
      })
      .finally(() => {
        pending.delete(ticket);
      });
    pending.set(ticket, request);
    return request;
  }
</script>

<script lang="ts">
  import { avatarColor, getInitials } from "#lib/utils.ts";

  interface Props {
    pubkey: string;
    name: string;
    isSelf?: boolean;
    ticket?: string | null;
    size?: number;
  }

  let {
    pubkey,
    name,
    isSelf = false,
    ticket = null,
    size = 32,
  }: Props = $props();

  let imgUrl = $state<string | null>(null);

  $effect(() => {
    const current = ticket;
    if (!current) {
      imgUrl = null;
      return;
    }
    const cached = cachedUrl(current);
    if (cached) {
      imgUrl = cached;
      return;
    }
    imgUrl = null;
    let stale = false;
    fetchAvatarUrl(current).then(
      (url) => {
        if (!stale) imgUrl = url;
      },
      () => {
        // The initials stay visible.
      },
    );
    return () => {
      stale = true;
    };
  });
</script>

{#if imgUrl}
  <img
    src={imgUrl}
    alt={name}
    class="avatar-img"
    style="width:{size}px;height:{size}px"
    onerror={() => (imgUrl = null)}
  />
{:else}
  <div
    class="avatar"
    style="background:{avatarColor(
      pubkey,
    )};width:{size}px;height:{size}px;font-size:{size * 0.35}px"
  >
    {getInitials(name, isSelf)}
  </div>
{/if}

<style>
  .avatar-img {
    border-radius: 50%;
    object-fit: cover;
    flex-shrink: 0;
  }

  .avatar {
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 700;
    color: var(--text-on-accent);
    flex-shrink: 0;
    text-transform: uppercase;
  }
</style>
