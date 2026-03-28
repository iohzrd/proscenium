<script lang="ts">
  interface ChatMessage {
    pubkey: string;
    name: string;
    text: string;
  }

  let {
    messages,
    chatInput = $bindable(""),
    onsend,
  }: {
    messages: ChatMessage[];
    chatInput: string;
    onsend: () => void;
  } = $props();

  let chatEl = $state<HTMLDivElement | null>(null);

  $effect(() => {
    // Auto-scroll when messages change
    if (messages.length > 0 && chatEl) {
      setTimeout(() => {
        if (chatEl) chatEl.scrollTop = chatEl.scrollHeight;
      }, 0);
    }
  });

  function handleChatKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      onsend();
    }
  }
</script>

<div class="room-sidebar">
  <div class="chat-messages" bind:this={chatEl}>
    {#each messages as msg}
      <div class="chat-msg">
        <span class="chat-author">{msg.name}</span>
        <span class="chat-text">{msg.text}</span>
      </div>
    {/each}
    {#if messages.length === 0}
      <div class="chat-empty">No messages yet</div>
    {/if}
  </div>
  <div class="chat-input-row">
    <input
      class="chat-input"
      type="text"
      placeholder="Chat..."
      bind:value={chatInput}
      onkeydown={handleChatKey}
    />
    <button class="btn-send" onclick={onsend} disabled={!chatInput.trim()}>
      Send
    </button>
  </div>
</div>

<style>
  .room-sidebar {
    width: 280px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-2xl);
    overflow: hidden;
  }

  .chat-messages {
    flex: 1;
    overflow-y: auto;
    padding: 0.85rem 0.85rem 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .chat-empty {
    color: var(--text-muted);
    font-size: var(--text-xs);
    text-align: center;
    padding: 1.5rem 1rem;
  }

  .chat-msg {
    font-size: var(--text-sm);
    word-break: break-word;
    line-height: 1.4;
  }

  .chat-author {
    font-weight: 700;
    color: var(--accent-medium);
    margin-right: 0.3rem;
  }

  .chat-text {
    color: var(--text-secondary);
  }

  .chat-input-row {
    display: flex;
    gap: 0.4rem;
    padding: 0.6rem;
    border-top: 1px solid var(--border);
  }

  .chat-input {
    flex: 1;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    color: var(--text-primary);
    font-size: var(--text-sm);
    padding: 0.4rem 0.75rem;
  }

  .chat-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn-send {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-full);
    padding: 0.4rem 0.9rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-fast);
    flex-shrink: 0;
  }

  .btn-send:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .btn-send:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  @media (max-width: 640px) {
    .room-sidebar {
      display: none;
    }
  }
</style>
