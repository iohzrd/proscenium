<script lang="ts">
  import type { Snippet } from "svelte";
  import MentionAutocomplete from "$lib/MentionAutocomplete.svelte";
  import { useMentionAutocomplete, autogrow } from "$lib/composables";
  import {
    MAX_CONTENT_BYTES,
    isMobilePlatform,
    isSubmitEnter,
    utf8Length,
  } from "$lib/messages/compose";

  let {
    pubkey,
    placeholder,
    submitLabel,
    allowEmpty = false,
    create,
    onsubmitted,
    oncancel,
    header,
  }: {
    pubkey: string;
    placeholder: string;
    submitLabel: string;
    /** If true, an empty text can be submitted. */
    allowEmpty?: boolean;
    /** Creates the post from the trimmed text. A rejection is shown as the error. */
    create: (content: string) => Promise<unknown>;
    onsubmitted?: () => void;
    oncancel?: () => void;
    /** Rendered above the textarea. */
    header?: Snippet;
  } = $props();

  const uid = $props.id();
  const isMobile = isMobilePlatform();

  let content = $state("");
  let posting = $state(false);
  let errorMessage = $state("");
  let mentionAutocomplete = $state<MentionAutocomplete>();

  const mention = useMentionAutocomplete(
    () => content,
    (v) => (content = v),
    `#${uid}`,
  );

  let trimmed = $derived(content.trim());
  let contentBytes = $derived(utf8Length(trimmed));
  let overLimit = $derived(contentBytes > MAX_CONTENT_BYTES);
  let canSubmit = $derived(
    !posting && !overLimit && (allowEmpty || trimmed.length > 0),
  );

  async function submit() {
    if (!canSubmit) return;
    posting = true;
    errorMessage = "";
    try {
      await create(trimmed);
      content = "";
      onsubmitted?.();
    } catch (e) {
      errorMessage = String(e);
    }
    posting = false;
  }

  function handleKey(e: KeyboardEvent) {
    if (mentionAutocomplete?.handleKey(e)) return;
    if (isSubmitEnter(e, isMobile)) {
      e.preventDefault();
      submit();
    } else if (e.key === "Escape") {
      oncancel?.();
    }
  }
</script>

<div class="composer">
  {@render header?.()}
  <MentionAutocomplete
    bind:this={mentionAutocomplete}
    query={mention.query}
    selfId={pubkey}
    visible={mention.active}
    onselect={mention.insertMention}
  />
  <textarea
    id={uid}
    class="textarea-base"
    bind:value={content}
    {placeholder}
    rows="1"
    maxlength={MAX_CONTENT_BYTES}
    onkeydown={handleKey}
    oninput={mention.handleInput}
    use:autogrow></textarea>
  {#if overLimit}
    <p class="composer-error">
      {contentBytes}/{MAX_CONTENT_BYTES} bytes
    </p>
  {/if}
  {#if errorMessage}
    <p class="composer-error" role="alert">{errorMessage}</p>
  {/if}
  <div class="composer-actions">
    <button class="btn-cancel" onclick={oncancel}>Cancel</button>
    <button
      class="btn-accent composer-submit"
      onclick={submit}
      disabled={!canSubmit}
    >
      {posting ? "Posting..." : submitLabel}
    </button>
  </div>
</div>

<style>
  .composer-error {
    color: var(--color-error-light);
    font-size: var(--text-sm);
    margin: 0.25rem 0 0;
  }
</style>
