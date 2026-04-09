<script lang="ts">
  import StageCard from "$lib/StageCard.svelte";
  import type { StageAnnouncement } from "$lib/types";

  interface Props {
    liveStages: Map<string, StageAnnouncement>;
  }

  let { liveStages }: Props = $props();
</script>

<aside class="right-sidebar">
  {#if liveStages.size > 0}
    <section class="panel">
      <h3 class="panel-title">Live Stages</h3>
      {#each [...liveStages.values()] as ann (ann.stage_id)}
        <StageCard announcement={ann} />
      {/each}
    </section>
  {/if}
</aside>

<style>
  .right-sidebar {
    display: none;
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: var(--right-sidebar-width);
    background: var(--bg-deep);
    border-left: 1px solid var(--border);
    padding: var(--space-xl) var(--space-lg);
    overflow-y: auto;
    z-index: var(--z-sidebar);
    flex-direction: column;
    gap: var(--space-xl);
  }

  /* Pillar inner-edge glow */
  .right-sidebar::before {
    content: "";
    position: absolute;
    top: 0;
    left: 0;
    bottom: 0;
    width: 32px;
    background: linear-gradient(
      to right,
      rgba(var(--accent-rgb), 0.25),
      transparent
    );
    pointer-events: none;
  }

  @media (min-width: 1150px) {
    .right-sidebar {
      display: flex;
    }
  }

  .panel {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-2xl);
    padding: var(--space-lg);
  }

  .panel-title {
    font-size: var(--text-base);
    font-weight: 700;
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin: 0 0 var(--space-md);
  }
</style>
