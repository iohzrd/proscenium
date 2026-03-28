<script lang="ts">
  let {
    mediaFilter,
    onFilterChange,
  }: {
    mediaFilter: string;
    onFilterChange: (value: string) => void;
  } = $props();

  const FILTERS = [
    { value: "all", label: "All" },
    { value: "images", label: "Images" },
    { value: "videos", label: "Videos" },
    { value: "audio", label: "Audio" },
    { value: "files", label: "Files" },
    { value: "text", label: "Text" },
  ] as const;
</script>

<div class="filter-bar">
  {#each FILTERS as f (f.value)}
    <button
      class="filter-chip"
      class:active={mediaFilter === f.value}
      onclick={() => onFilterChange(f.value)}
    >
      {f.label}
    </button>
  {/each}
</div>

<style>
  .filter-bar {
    margin-bottom: 0.75rem;
  }
</style>
