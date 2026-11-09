<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "#lib/Avatar.svelte";
  import {
    getDisplayName,
    getCachedAvatarTicket,
    shortId,
  } from "#lib/utils.ts";
  import type { SocialGraphEntry } from "#lib/types.ts";

  const DEBOUNCE_MS = 150;
  const MAX_RESULTS = 8;
  /** Number of display-name lookups that run at the same time. */
  const LOOKUP_BATCH = 16;

  let {
    query,
    selfId,
    visible = false,
    onselect,
  }: {
    query: string;
    selfId: string;
    visible?: boolean;
    onselect: (pubkey: string) => void;
  } = $props();

  let results = $state<{ pubkey: string; name: string }[]>([]);
  let selectedIndex = $state(0);

  // Incremented whenever the query changes; a search assigns its results only
  // while its own value is still the latest.
  let searchSeq = 0;

  $effect(() => {
    const q = visible ? query : "";
    const seq = ++searchSeq;
    if (!q) {
      results = [];
      return;
    }
    const timer = setTimeout(() => searchUsers(q, seq), DEBOUNCE_MS);
    return () => clearTimeout(timer);
  });

  async function searchUsers(q: string, seq: number) {
    const lowerQ = q.toLowerCase();
    try {
      const [follows, followers] = await Promise.all([
        invoke<SocialGraphEntry[]>("get_follows"),
        invoke<SocialGraphEntry[]>("get_followers"),
      ]);
      if (seq !== searchSeq) return;
      const pubkeys = [
        ...new Set([...follows, ...followers].map((entry) => entry.pubkey)),
      ].filter((pubkey) => pubkey !== selfId);

      const items: { pubkey: string; name: string }[] = [];
      for (
        let start = 0;
        start < pubkeys.length && items.length < MAX_RESULTS;
        start += LOOKUP_BATCH
      ) {
        const batch = pubkeys.slice(start, start + LOOKUP_BATCH);
        const names = await Promise.all(
          batch.map((pubkey) => getDisplayName(pubkey, selfId)),
        );
        if (seq !== searchSeq) return;
        for (const [i, pubkey] of batch.entries()) {
          const name = names[i];
          if (
            items.length < MAX_RESULTS &&
            (name.toLowerCase().includes(lowerQ) ||
              pubkey.toLowerCase().startsWith(lowerQ))
          ) {
            items.push({ pubkey, name });
          }
        }
      }
      results = items;
      selectedIndex = 0;
    } catch (e) {
      console.error("Mention search failed:", e);
      if (seq === searchSeq) results = [];
    }
  }

  export function handleKey(e: KeyboardEvent): boolean {
    if (!visible || results.length === 0) return false;
    // A key press that belongs to an IME composition selects nothing.
    if (e.isComposing || e.keyCode === 229) return false;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selectedIndex = (selectedIndex + 1) % results.length;
      return true;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      selectedIndex = (selectedIndex - 1 + results.length) % results.length;
      return true;
    }
    if (e.key === "Enter" || e.key === "Tab") {
      const selected = results[selectedIndex];
      if (!selected) return false;
      e.preventDefault();
      onselect(selected.pubkey);
      return true;
    }
    if (e.key === "Escape") {
      results = [];
      return true;
    }
    return false;
  }
</script>

{#if visible && results.length > 0}
  <div class="mention-autocomplete">
    {#each results as result, i}
      <button
        class="mention-option"
        class:selected={i === selectedIndex}
        onclick={() => onselect(result.pubkey)}
        type="button"
      >
        <Avatar
          pubkey={result.pubkey}
          name={result.name}
          isSelf={false}
          ticket={getCachedAvatarTicket(result.pubkey)}
          size={24}
        />
        <span class="mention-name">{result.name}</span>
        <code class="mention-id">{shortId(result.pubkey)}</code>
      </button>
    {/each}
  </div>
{/if}

<style>
  .mention-autocomplete {
    position: absolute;
    bottom: 100%;
    left: 0;
    right: 0;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    max-height: 240px;
    overflow-y: auto;
    z-index: var(--z-dropdown);
    box-shadow: var(--shadow-md);
  }

  .mention-option {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    padding: 0.5rem 0.75rem;
    background: none;
    border: none;
    color: var(--text-primary);
    cursor: pointer;
    font-size: var(--text-base);
    text-align: left;
  }

  .mention-option:hover,
  .mention-option.selected {
    background: var(--bg-elevated);
  }

  .mention-name {
    font-weight: 600;
    color: var(--accent-light);
  }

  .mention-id {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    margin-left: auto;
  }
</style>
