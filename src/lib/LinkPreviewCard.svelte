<script lang="ts">
  import type { LinkPreview } from "$lib/types";

  let { preview }: { preview: LinkPreview } = $props();
</script>

<a
  href={preview.url}
  target="_blank"
  rel="noopener noreferrer"
  class="link-preview"
>
  {#if preview.image}
    <img src={preview.image} alt="" class="preview-image" />
  {/if}
  <div class="preview-text">
    {#if preview.site_name}
      <span class="preview-site">{preview.site_name}</span>
    {/if}
    {#if preview.title}
      <span class="preview-title">{preview.title}</span>
    {/if}
    {#if preview.description}
      <span class="preview-desc">{preview.description}</span>
    {/if}
  </div>
</a>

<style>
  .link-preview {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    overflow: hidden;
    text-decoration: none;
    color: inherit;
    margin-top: 0.5rem;
    transition: border-color var(--transition-fast);
  }

  .link-preview:hover {
    border-color: var(--border-hover);
  }

  .preview-image {
    width: 100%;
    max-height: 200px;
    object-fit: cover;
  }

  .preview-text {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    padding: 0.6rem 0.75rem;
  }

  .preview-site {
    font-size: var(--text-xs);
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.02em;
  }

  .preview-title {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-primary);
    display: -webkit-box;
    line-clamp: 2;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .preview-desc {
    font-size: var(--text-sm);
    color: var(--text-secondary);
    display: -webkit-box;
    line-clamp: 2;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    line-height: 1.4;
  }
</style>
