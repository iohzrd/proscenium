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

  @media (min-width: 1150px) {
    .right-sidebar {
      display: flex;
    }
  }

  .panel {
    padding: 0;
  }

  .panel-title {
    font-family: var(--font-body);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.12em;
    margin: 0 0 var(--space-md);
    padding-bottom: var(--space-sm);
    border-bottom: 1px solid var(--border);
  }
</style>
