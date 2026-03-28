<script lang="ts">
  interface FloatingReaction {
    id: number;
    emoji: string;
    x: number;
  }

  let {
    reactions,
  }: {
    reactions: FloatingReaction[];
  } = $props();
</script>

{#each reactions as r (r.id)}
  <span class="floating-reaction" style="left:{r.x}%">{r.emoji}</span>
{/each}

<style>
  .floating-reaction {
    position: fixed;
    bottom: 6rem;
    font-size: 2rem;
    pointer-events: none;
    animation: float-up 2s ease-out forwards;
    z-index: 200;
  }

  @keyframes float-up {
    0% {
      transform: translateY(0);
      opacity: 1;
    }
    100% {
      transform: translateY(-120px);
      opacity: 0;
    }
  }
</style>
