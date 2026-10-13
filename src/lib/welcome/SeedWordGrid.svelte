<script lang="ts">
  let { words }: { words: string[] } = $props();
</script>

<ol class="seed-grid">
  {#each words as word, i}
    <li class="seed-word">
      <span class="seed-num">{i + 1}</span>
      <span class="seed-text">{word}</span>
    </li>
  {/each}
</ol>

<style>
  .seed-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 0.5rem;
    margin: 0 0 1rem;
    padding: 0;
    list-style: none;
    text-align: left;
  }

  .seed-word {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.4rem 0.6rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    min-width: 0;
  }

  .seed-num {
    color: var(--text-tertiary);
    font-size: var(--text-xs);
    min-width: 1.2rem;
    user-select: none;
  }

  .seed-text {
    color: var(--text-primary);
    overflow-wrap: anywhere;
  }
</style>
