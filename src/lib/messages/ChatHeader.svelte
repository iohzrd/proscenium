<script lang="ts">
  import Avatar from "$lib/Avatar.svelte";

  let {
    pubkey,
    peerName,
    avatarTicket,
  }: {
    pubkey: string;
    peerName: string;
    avatarTicket: string | null;
  } = $props();
</script>

<div class="chat-header">
  <a href="/messages" class="back-btn">&larr;</a>
  <Avatar {pubkey} name={peerName} ticket={avatarTicket} size={32} />
  <div class="header-info">
    <span class="header-name">{peerName}</span>
  </div>
</div>

<style>
  .chat-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
    background: var(--bg-base);
    flex-shrink: 0;
  }

  .back-btn {
    color: var(--accent-medium);
    text-decoration: none;
    font-size: var(--text-icon-lg);
    padding: 0.25rem;
  }

  .back-btn:hover {
    color: var(--accent-light);
  }

  .header-info {
    flex: 1;
    min-width: 0;
  }

  .header-name {
    font-weight: 600;
    font-size: var(--text-lg);
    color: var(--text-primary);
  }
</style>
