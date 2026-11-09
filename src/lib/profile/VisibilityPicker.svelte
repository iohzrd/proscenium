<script lang="ts">
  import type { Visibility } from "#lib/types.ts";

  let { value = $bindable() }: { value: Visibility } = $props();

  // Name of the radio group: this component can be mounted more than once.
  const uid = $props.id();

  const OPTIONS: { value: Visibility; label: string; hint: string }[] = [
    {
      value: "public",
      label: "Public",
      hint: "Anyone can see your posts and sync your profile.",
    },
    {
      value: "listed",
      label: "Listed",
      hint: "Your profile is discoverable, but posts are only shared with approved followers.",
    },
    {
      value: "private",
      label: "Private",
      hint: "Only mutual follows can see your posts. You are invisible to servers.",
    },
  ];
</script>

<div class="visibility-options">
  {#each OPTIONS as opt}
    <label class="visibility-option" class:selected={value === opt.value}>
      <input
        type="radio"
        name="{uid}-visibility"
        value={opt.value}
        bind:group={value}
      />
      <span class="visibility-label">{opt.label}</span>
      <span class="visibility-hint">{opt.hint}</span>
    </label>
  {/each}
</div>

<style>
  .visibility-options {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .visibility-option {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
    padding: 0.6rem 0.75rem;
    background: var(--bg-deep);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    cursor: pointer;
    text-align: left;
    transition:
      border-color var(--transition-normal),
      background var(--transition-normal);
  }

  .visibility-option:hover {
    border-color: var(--border-hover);
  }

  .visibility-option.selected {
    border-color: var(--accent);
    background: var(--bg-elevated);
  }

  .visibility-option input[type="radio"] {
    accent-color: var(--accent);
    margin: 0;
  }

  .visibility-label {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--text-primary);
  }

  .visibility-hint {
    width: 100%;
    font-size: var(--text-sm);
    color: var(--text-tertiary);
    padding-left: 1.5rem;
  }
</style>
