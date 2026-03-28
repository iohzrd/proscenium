<script lang="ts">
  import Icon from "$lib/Icon.svelte";

  let {
    createTitle = $bindable(""),
    joinTicket = $bindable(""),
    createdTicket,
    errorMsg,
    busy,
    oncreate,
    onjoin,
    oncopyticket,
  }: {
    createTitle: string;
    joinTicket: string;
    createdTicket: string | null;
    errorMsg: string;
    busy: boolean;
    oncreate: () => void;
    onjoin: () => void;
    oncopyticket: () => void;
  } = $props();
</script>

<div class="stage-landing">
  <h1 class="stage-heading">
    <Icon name="radio" size={28} />
    Stage
  </h1>
  <p class="stage-subheading">Live audio rooms for your community</p>

  {#if errorMsg}
    <div class="stage-error">{errorMsg}</div>
  {/if}

  <div class="stage-cards">
    <div class="stage-card">
      <h2>Create a Stage</h2>
      <input
        class="stage-input"
        type="text"
        placeholder="Stage title"
        bind:value={createTitle}
        disabled={busy}
        onkeydown={(e) => e.key === "Enter" && oncreate()}
      />
      <button
        class="btn-primary"
        onclick={oncreate}
        disabled={busy || !createTitle.trim()}
      >
        {busy ? "Creating..." : "Create Stage"}
      </button>
    </div>

    <div class="stage-divider">or</div>

    <div class="stage-card">
      <h2>Join a Stage</h2>
      <input
        class="stage-input"
        type="text"
        placeholder="Paste invite ticket"
        bind:value={joinTicket}
        disabled={busy}
        onkeydown={(e) => e.key === "Enter" && onjoin()}
      />
      <button
        class="btn-primary"
        onclick={onjoin}
        disabled={busy || !joinTicket.trim()}
      >
        {busy ? "Joining..." : "Join Stage"}
      </button>
    </div>
  </div>

  {#if createdTicket}
    <div class="ticket-share">
      <p class="ticket-label">Share this invite ticket:</p>
      <div class="ticket-row">
        <input
          class="ticket-input"
          type="text"
          readonly
          value={createdTicket}
          onclick={(e) => (e.target as HTMLInputElement).select()}
        />
        <button class="btn-icon" onclick={oncopyticket} title="Copy">
          <Icon name="copy" size={16} />
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .stage-landing {
    max-width: 640px;
    margin: 2rem auto;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
  }

  .stage-heading {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-2xl);
    font-weight: 700;
    color: var(--text-primary);
    margin: 0;
  }

  .stage-subheading {
    color: var(--text-muted);
    margin: 0;
  }

  .stage-error {
    background: var(--danger-bg);
    border: 1px solid var(--danger-border);
    color: var(--danger-text);
    padding: 0.75rem 1rem;
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
  }

  .stage-cards {
    display: flex;
    gap: 1rem;
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .stage-card {
    flex: 1;
    min-width: 220px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .stage-card h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--text-primary);
  }

  .stage-divider {
    align-self: center;
    color: var(--text-muted);
    font-size: var(--text-sm);
    padding: 0 0.5rem;
  }

  .stage-input {
    width: 100%;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-size: var(--text-base);
    padding: 0.6rem 0.75rem;
    box-sizing: border-box;
  }

  .stage-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn-primary {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.65rem 1.25rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-fast);
    width: 100%;
  }

  .btn-primary:hover:not(:disabled) {
    background: var(--accent-dark);
  }

  .btn-primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .ticket-share {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .ticket-label {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--text-muted);
    font-weight: 500;
  }

  .ticket-row {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }

  .ticket-input {
    flex: 1;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    padding: 0.5rem 0.75rem;
    font-family: monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: text;
  }

  .ticket-input:focus {
    outline: none;
  }

  .btn-icon {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.5rem;
    cursor: pointer;
    color: var(--text-secondary);
    display: flex;
    align-items: center;
    transition: background var(--transition-fast);
    flex-shrink: 0;
  }

  .btn-icon:hover {
    background: var(--bg-elevated-hover);
  }

  @media (max-width: 640px) {
    .stage-cards {
      flex-direction: column;
    }

    .stage-divider {
      align-self: stretch;
      text-align: center;
    }
  }
</style>
