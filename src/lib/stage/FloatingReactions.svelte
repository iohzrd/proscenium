<script lang="ts">
  import { truncate } from "$lib/utils";

  interface FloatingReaction {
    id: number;
    emoji: string;
    x: number;
  }

  // Bounds applied on render: reaction strings come from other participants.
  const MAX_RENDERED_REACTIONS = 30;
  const MAX_REACTION_CHARS = 16;

  let {
    reactions,
  }: {
    reactions: FloatingReaction[];
  } = $props();

  let visible = $derived(reactions.slice(-MAX_RENDERED_REACTIONS));
</script>

{#each visible as r (r.id)}
  <span
    class="floating-reaction"
    style:left="{Math.min(Math.max(r.x, 0), 100)}%"
    aria-hidden="true">{truncate(r.emoji, MAX_REACTION_CHARS)}</span
  >
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
