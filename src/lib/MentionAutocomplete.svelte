<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "$lib/Avatar.svelte";
  import { getDisplayName, getCachedAvatarTicket, shortId } from "$lib/utils";
  import type { SocialGraphEntry } from "$lib/types";

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

  let debounceTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    if (!visible || !query) {
      results = [];
      return;
    }
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => searchUsers(query), 150);
  });

  async function searchUsers(q: string) {
    const lowerQ = q.toLowerCase();
    try {
      const follows: SocialGraphEntry[] = await invoke("get_follows");
      const followers: SocialGraphEntry[] = await invoke("get_followers");
      const seen = new Set<string>();
      const items: { pubkey: string; name: string }[] = [];

      for (const f of follows) {
        if (seen.has(f.pubkey) || f.pubkey === selfId) continue;
        seen.add(f.pubkey);
        const name = await getDisplayName(f.pubkey, selfId);
        if (
          name.toLowerCase().includes(lowerQ) ||
          f.pubkey.toLowerCase().startsWith(lowerQ)
        ) {
          items.push({ pubkey: f.pubkey, name });
        }
        if (items.length >= 8) break;
      }
      if (items.length < 8) {
        for (const f of followers) {
          if (seen.has(f.pubkey) || f.pubkey === selfId) continue;
          seen.add(f.pubkey);
          const name = await getDisplayName(f.pubkey, selfId);
          if (
            name.toLowerCase().includes(lowerQ) ||
            f.pubkey.toLowerCase().startsWith(lowerQ)
          ) {
            items.push({ pubkey: f.pubkey, name });
          }
          if (items.length >= 8) break;
        }
      }
      results = items;
      selectedIndex = 0;
    } catch {
      results = [];
    }
  }

  export function handleKey(e: KeyboardEvent): boolean {
    if (!visible || results.length === 0) return false;
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
      e.preventDefault();
      onselect(results[selectedIndex].pubkey);
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
