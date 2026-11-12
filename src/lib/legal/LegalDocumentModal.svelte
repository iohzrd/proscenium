<script lang="ts">
  import Modal from "#lib/Modal.svelte";
  import {
    LEGAL_DOCUMENTS,
    type LegalDocumentId,
  } from "#lib/legal/documents.ts";

  let {
    docId,
    onclose,
  }: {
    docId: LegalDocumentId;
    onclose: () => void;
  } = $props();

  const blocks = $derived(LEGAL_DOCUMENTS[docId]);
  const title = $derived(
    blocks.find((b) => b.kind === "title")?.text ?? "Document",
  );
</script>

<Modal label={title} {onclose} maxWidth="40rem">
  <div class="legal-body">
    {#each blocks as block, i (i)}
      {#if block.kind === "title"}
        <h2>{block.text}</h2>
      {:else if block.kind === "heading"}
        <h3>{block.text}</h3>
      {:else if block.kind === "paragraph"}
        <p>{block.text}</p>
      {:else}
        <ul>
          {#each block.items as item, j (j)}
            <li>{item}</li>
          {/each}
        </ul>
      {/if}
    {/each}
  </div>
  <div class="modal-actions">
    <button class="modal-cancel" onclick={onclose}>Close</button>
  </div>
</Modal>

<style>
  .legal-body {
    max-height: 65vh;
    overflow-y: auto;
    margin-bottom: 1rem;
    text-align: left;
    color: var(--text-secondary);
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .legal-body h2 {
    margin: 0 0 0.75rem;
    font-size: var(--text-xl);
    color: var(--text-primary);
  }

  .legal-body h3 {
    margin: 1.25rem 0 0.5rem;
    font-size: var(--text-base);
    color: var(--text-primary);
  }

  .legal-body p {
    margin: 0 0 0.75rem;
    text-align: left;
  }

  .legal-body ul {
    margin: 0 0 0.75rem;
    padding-left: 1.25rem;
  }

  .legal-body li {
    margin-bottom: 0.35rem;
  }
</style>
