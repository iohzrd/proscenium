import { shortId, getDisplayName, getCachedDisplayName } from "$lib/utils";

/**
 * Reactive display name resolution using Svelte 5 runes.
 * Returns an object with a reactive `name` property that starts with a
 * synchronous fallback and updates when the async profile lookup completes.
 * The result of a lookup for an earlier pubkey is discarded.
 *
 * Must be called from a component's top-level script (so $effect binds correctly).
 */
export function useDisplayName(
  getPubkey: () => string,
  getSelfId: () => string,
): { readonly name: string } {
  let name = $state("");

  $effect(() => {
    const pubkey = getPubkey();
    const selfId = getSelfId();
    if (!pubkey) {
      name = "";
      return;
    }
    if (pubkey === selfId) {
      name = "You";
      return;
    }
    // Reading the cache entry makes this effect run again when it changes.
    const cached = getCachedDisplayName(pubkey);
    name = cached ?? shortId(pubkey);
    if (cached !== undefined) return;
    let cancelled = false;
    getDisplayName(pubkey, selfId).then((resolved) => {
      if (!cancelled) name = resolved;
    });
    return () => {
      cancelled = true;
    };
  });

  return {
    get name() {
      return name;
    },
  };
}
