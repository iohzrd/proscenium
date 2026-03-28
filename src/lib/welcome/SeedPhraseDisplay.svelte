<script lang="ts">
  import { copyToClipboard } from "$lib/utils";

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
  let seedCopyFeedback = $state(false);

  let seedWords = $derived(seedPhrase ? seedPhrase.split(" ") : []);

  async function copySeedPhrase() {
    await copyToClipboard(seedPhrase);
    seedCopyFeedback = true;
    setTimeout(() => (seedCopyFeedback = false), 1500);
  }
</script>

<div class="step">
  <h2>Back Up Your Recovery Phrase</h2>
  <p class="desc">
    This 24-word phrase is the only way to recover your identity. Write it down
    and store it somewhere safe. If you lose it, your identity cannot be
    recovered.
  </p>

  {#if seedRevealed}
    <div class="seed-grid">
      {#each seedWords as word, i}
        <div class="seed-word">
          <span class="seed-num">{i + 1}</span>
          <span class="seed-text">{word}</span>
        </div>
      {/each}
    </div>

    <div class="seed-actions">
      <button class="secondary-btn" onclick={copySeedPhrase}>
        {seedCopyFeedback ? "Copied!" : "Copy to Clipboard"}
      </button>
    </div>
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
    {#if seedRevealed}
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

  .seed-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 0.5rem;
    margin-bottom: 1rem;
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
  }

  .seed-num {
    color: var(--text-tertiary);
    font-size: var(--text-xs);
    min-width: 1.2rem;
  }

  .seed-text {
    color: var(--text-primary);
  }

  .seed-actions {
    margin-bottom: 1.5rem;
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
