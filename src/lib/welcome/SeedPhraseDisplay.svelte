<script lang="ts">
  import SeedWordGrid from "./SeedWordGrid.svelte";

  let {
    seedPhrase,
    onback,
    onverify,
    onskip,
  }: {
    seedPhrase: string;
    onback: () => void;
    onverify: () => void;
    onskip: () => void;
  } = $props();

  let seedRevealed = $state(false);

  let seedWords = $derived(seedPhrase ? seedPhrase.split(" ") : []);
</script>

<div class="step">
  <h2>Back Up Your Recovery Phrase</h2>
  <p class="desc">
    This 24-word phrase is the only way to recover your identity. Write it down
    on paper and store it somewhere safe. If you lose it, your identity cannot
    be recovered. Do not copy it to the clipboard, a screenshot or a file: other
    applications can read those.
  </p>

  {#if seedWords.length === 0}
    <div class="seed-hidden">
      <p class="seed-warning">
        The recovery phrase is not loaded. Go back and continue again.
      </p>
    </div>
  {:else if seedRevealed}
    <SeedWordGrid words={seedWords} />
  {:else}
    <div class="seed-hidden">
      <p class="seed-warning">
        Make sure no one is looking at your screen before revealing.
      </p>
      <button
        class="btn-accent primary-btn"
        onclick={() => (seedRevealed = true)}
      >
        Reveal Recovery Phrase
      </button>
    </div>
  {/if}

  <div class="actions">
    <button class="secondary-btn" onclick={onback}>Back</button>
    {#if seedRevealed && seedWords.length > 0}
      <button class="btn-accent primary-btn" onclick={onverify}>
        I've Written It Down
      </button>
    {/if}
  </div>

  <button class="skip-btn" onclick={onskip}>
    Skip for now (not recommended)
  </button>
</div>

<style>
  .step {
    max-width: 420px;
    width: 100%;
    text-align: center;
  }

  h2 {
    font-size: var(--text-2xl);
    color: var(--text-primary);
    margin: 0 0 1.5rem;
  }

  .desc {
    color: var(--text-secondary);
    font-size: var(--text-base);
    line-height: 1.6;
    margin: 0 0 1.5rem;
  }

  .seed-hidden {
    padding: 2rem 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    margin-bottom: 1.5rem;
  }

  .seed-warning {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
  }

  .actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.5rem;
  }

  .primary-btn {
    flex: 1;
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
  }

  .secondary-btn {
    background: none;
    border: 1px solid var(--border-hover);
    color: var(--text-secondary);
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
    cursor: pointer;
    transition:
      color var(--transition-fast),
      border-color var(--transition-fast);
  }

  .secondary-btn:hover {
    color: var(--accent-light);
    border-color: var(--accent);
  }

  .skip-btn {
    display: block;
    margin: 1rem auto 0;
    background: none;
    border: none;
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    cursor: pointer;
    text-decoration: underline;
  }

  .skip-btn:hover {
    color: var(--text-secondary);
  }
</style>
