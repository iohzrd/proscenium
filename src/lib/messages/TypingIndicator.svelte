<script lang="ts">
  let {
    peerName,
  }: {
    peerName: string;
  } = $props();
</script>

<div class="typing-indicator">
  <span class="typing-name">{peerName}</span> is typing
  <span class="typing-dots">
    <span class="dot"></span>
    <span class="dot"></span>
    <span class="dot"></span>
  </span>
</div>

<style>
  .typing-indicator {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.3rem 0;
    color: var(--text-secondary);
    font-size: var(--text-sm);
    font-style: italic;
  }

  .typing-name {
    color: var(--accent-medium);
    font-style: normal;
    font-weight: 600;
  }

  .typing-dots {
    display: inline-flex;
    gap: 2px;
    margin-left: 2px;
  }

  .dot {
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--text-secondary);
    animation: bounce 1.2s infinite;
  }

  .dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .dot:nth-child(3) {
    animation-delay: 0.4s;
  }

  @keyframes bounce {
    0%,
    60%,
    100% {
      transform: translateY(0);
    }
    30% {
      transform: translateY(-4px);
    }
  }
</style>
