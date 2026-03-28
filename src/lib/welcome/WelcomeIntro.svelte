<script lang="ts">
  import { copyToClipboard } from "$lib/utils";

  let {
    nodeId,
    masterPubkey,
    oncontinue,
    onrecover,
  }: {
    nodeId: string;
    masterPubkey: string;
    oncontinue: () => void;
    onrecover: () => void;
  } = $props();

  let copyFeedback = $state(false);

  async function copyNodeId() {
    await copyToClipboard(nodeId);
    copyFeedback = true;
    setTimeout(() => (copyFeedback = false), 1500);
  }

  async function copyPubkey() {
    await copyToClipboard(masterPubkey);
    copyFeedback = true;
    setTimeout(() => (copyFeedback = false), 1500);
  }
</script>

<div class="step">
  <h1>Welcome</h1>
  <p class="subtitle">
    A peer-to-peer social network. No servers, no middlemen.
  </p>
  <p class="desc">
    Your identity is a cryptographic key pair stored on your device. You own
    your data.
  </p>
  {#if nodeId}
    <div class="node-id-section">
      <p class="label">Your Node ID (transport address)</p>
      <button class="node-id" onclick={copyNodeId} title="Copy Node ID">
        {nodeId.slice(0, 16)}...{nodeId.slice(-8)}
      </button>
      {#if copyFeedback}
        <span class="copied">Copied!</span>
      {/if}
    </div>
  {/if}
  {#if masterPubkey}
    <div class="node-id-section">
      <p class="label">Your Public Key (permanent identity)</p>
      <button class="node-id" onclick={copyPubkey} title="Copy Public Key">
        {masterPubkey.slice(0, 16)}...{masterPubkey.slice(-8)}
      </button>
    </div>
  {/if}
  <button class="btn-accent primary-btn" onclick={oncontinue}>
    Continue
  </button>
  <button class="skip-btn" onclick={onrecover}>
    Recover existing identity
  </button>
</div>

<style>
  .step {
    max-width: 420px;
    width: 100%;
    text-align: center;
  }

  h1 {
    font-size: var(--text-3xl);
    color: var(--text-primary);
    margin: 0 0 0.5rem;
  }

  .subtitle {
    color: var(--accent-medium);
    font-size: var(--text-lg);
    margin: 0 0 1rem;
  }

  .desc {
    color: var(--text-secondary);
    font-size: var(--text-base);
    line-height: 1.6;
    margin: 0 0 1.5rem;
  }

  .node-id-section {
    margin-bottom: 2rem;
  }

  .label {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 0 0 0.3rem;
  }

  .node-id {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.4rem 0.8rem;
    color: var(--accent-light);
    font-family: var(--font-mono);
    font-size: var(--text-base);
    cursor: pointer;
  }

  .node-id:hover {
    background: var(--bg-elevated);
  }

  .copied {
    display: block;
    color: var(--color-success);
    font-size: var(--text-sm);
    margin-top: 0.3rem;
  }

  .primary-btn {
    flex: 1;
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
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
