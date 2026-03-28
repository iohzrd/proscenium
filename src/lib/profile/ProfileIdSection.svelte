<script lang="ts">
  let {
    pubkey,
    transportNodeIds,
    copyFeedback,
    onCopy,
    onShowQr,
  }: {
    pubkey: string;
    transportNodeIds: string[];
    copyFeedback: string | null;
    onCopy: (text: string, key: string) => void;
    onShowQr: () => void;
  } = $props();
</script>

<div class="id-section">
  <div class="id-row">
    <span class="id-label">Public Key</span>
    <code>{pubkey}</code>
    <button
      class="btn-elevated copy-btn"
      onclick={() => onCopy(pubkey, "pubkey")}
    >
      {copyFeedback === "pubkey" ? "Copied!" : "Copy"}
    </button>
    <button class="btn-elevated copy-btn" onclick={onShowQr}>QR</button>
  </div>
  {#each transportNodeIds as nid, i}
    <div class="id-row">
      <span class="id-label"
        >Node ID{transportNodeIds.length > 1 ? ` ${i + 1}` : ""}</span
      >
      <code>{nid}</code>
      <button
        class="btn-elevated copy-btn"
        onclick={() => onCopy(nid, `transport-${i}`)}
      >
        {copyFeedback === `transport-${i}` ? "Copied!" : "Copy"}
      </button>
    </div>
  {/each}
</div>

<style>
  .id-section {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-bottom: 1rem;
  }

  .id-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .id-label {
    color: var(--text-secondary);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
    min-width: 5.5rem;
    flex-shrink: 0;
  }

  code {
    background: var(--bg-deep);
    padding: 0.5rem 0.75rem;
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    word-break: break-all;
    color: var(--color-link);
    flex: 1;
    font-family: var(--font-mono);
  }
</style>
