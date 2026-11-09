<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy } from "svelte";
  import Avatar from "#lib/Avatar.svelte";
  import VisibilityPicker from "#lib/profile/VisibilityPicker.svelte";
  import type { Profile, Visibility } from "#lib/types.ts";
  import { uploadFiles } from "#lib/utils.ts";

  let {
    pubkey,
    profile,
    onsaved,
    oncancel,
  }: {
    pubkey: string;
    profile: Profile;
    onsaved: () => void;
    oncancel: () => void;
  } = $props();

  // Form fields. Each starts at the stored value and returns to it when the
  // `profile` prop changes.
  let editDisplayName = $derived(profile.display_name);
  let editBio = $derived(profile.bio);
  let editAvatarHash = $derived<string | null>(profile.avatar_hash);
  let editAvatarTicket = $derived<string | null>(profile.avatar_ticket);
  let editVisibility = $derived<Visibility>(profile.visibility);

  let saving = $state(false);
  let uploading = $state(false);
  let fileInput = $state<HTMLInputElement>(null!);
  let errorMessage = $state("");
  let errorTimer: ReturnType<typeof setTimeout> | undefined;

  let isDirty = $derived(
    editDisplayName !== profile.display_name ||
      editBio !== profile.bio ||
      editAvatarHash !== profile.avatar_hash ||
      editVisibility !== profile.visibility,
  );

  function showError(message: string) {
    errorMessage = message;
    clearTimeout(errorTimer);
    errorTimer = setTimeout(() => (errorMessage = ""), 4000);
  }

  async function handleAvatarFile(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    uploading = true;
    try {
      const [uploaded] = await uploadFiles([file]);
      // The preview is rendered by Avatar from the ticket.
      if (uploaded.previewUrl.startsWith("blob:")) {
        URL.revokeObjectURL(uploaded.previewUrl);
      }
      editAvatarHash = uploaded.hash;
      editAvatarTicket = uploaded.ticket;
    } catch (err) {
      showError(`Upload failed: ${err instanceof Error ? err.message : err}`);
    }
    uploading = false;
  }

  function removeAvatar() {
    editAvatarHash = null;
    editAvatarTicket = null;
  }

  async function saveProfile() {
    saving = true;
    editDisplayName = editDisplayName.trim();
    editBio = editBio.trim();
    try {
      await invoke("save_my_profile", {
        displayName: editDisplayName,
        bio: editBio,
        avatarHash: editAvatarHash,
        avatarTicket: editAvatarTicket,
        visibility: editVisibility,
      });
      onsaved();
    } catch (err) {
      showError(`Error: ${err}`);
    }
    saving = false;
  }

  onDestroy(() => clearTimeout(errorTimer));
</script>

<h2 class="edit-heading">Edit Profile</h2>
<div class="edit-form">
  <div class="field">
    <span class="field-label">Avatar</span>
    <div class="avatar-row">
      <Avatar
        {pubkey}
        name={editDisplayName || "You"}
        isSelf={!editDisplayName}
        ticket={editAvatarTicket}
        size={56}
      />
      <div class="avatar-actions">
        <button
          class="avatar-btn"
          onclick={() => fileInput.click()}
          disabled={uploading}
        >
          {uploading ? "Uploading..." : editAvatarHash ? "Change" : "Upload"}
        </button>
        {#if editAvatarHash}
          <button class="avatar-btn remove" onclick={removeAvatar}
            >Remove</button
          >
        {/if}
      </div>
      <input
        bind:this={fileInput}
        type="file"
        accept="image/png,image/jpeg,image/gif,image/webp"
        onchange={handleAvatarFile}
        hidden
      />
    </div>
  </div>

  <label class="field">
    <span class="field-label">Display Name</span>
    <input bind:value={editDisplayName} placeholder="Anonymous" />
  </label>

  <label class="field">
    <span class="field-label">Bio</span>
    <textarea
      bind:value={editBio}
      placeholder="Tell the world about yourself..."
      rows="3"></textarea>
  </label>

  <div class="field">
    <span class="field-label">Visibility</span>
    <VisibilityPicker bind:value={editVisibility} />
  </div>

  {#if errorMessage}
    <p class="edit-error" role="alert">{errorMessage}</p>
  {/if}

  <div class="edit-actions">
    <button class="btn-cancel cancel-btn" onclick={oncancel}>Cancel</button>
    <button
      class="btn-accent save-btn"
      onclick={saveProfile}
      disabled={saving || uploading || !isDirty}
    >
      {saving ? "Saving..." : "Save"}
    </button>
  </div>
</div>

<style>
  .edit-heading {
    color: var(--accent-medium);
    margin: 0 0 1rem;
    font-size: var(--text-xl);
  }

  .edit-form {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1.25rem;
    margin-bottom: 1rem;
  }

  .field {
    display: block;
    margin-bottom: 1rem;
  }

  .avatar-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .avatar-actions {
    display: flex;
    gap: 0.5rem;
  }

  .avatar-btn {
    background: var(--bg-elevated);
    color: var(--accent-light);
    border: none;
    border-radius: var(--radius-sm);
    padding: 0.3rem 0.75rem;
    font-size: var(--text-base);
    cursor: pointer;
  }

  .avatar-btn:hover:not(:disabled) {
    background: var(--bg-elevated-hover);
  }

  .avatar-btn.remove {
    color: var(--color-error-light);
  }

  .avatar-btn.remove:hover {
    background: var(--color-error-light-bg);
  }

  .edit-form input:not([type="checkbox"]):not([type="radio"]),
  .edit-form textarea {
    width: 100%;
    background: var(--bg-deep);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.6rem 0.75rem;
    color: var(--text-primary);
    font-size: var(--text-base);
    outline: none;
    transition: border-color var(--transition-normal);
    resize: vertical;
  }

  .edit-form input:not([type="checkbox"]):not([type="radio"]):focus,
  .edit-form textarea:focus {
    border-color: var(--accent-medium);
  }

  .edit-error {
    color: var(--color-error-light);
    font-size: var(--text-sm);
    margin: 0 0 0.5rem;
  }

  .edit-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.25rem;
  }

  .cancel-btn {
    flex: 1;
    padding: 0.6rem;
    font-size: var(--text-base);
  }

  .save-btn {
    flex: 1;
    padding: 0.6rem;
    font-size: var(--text-base);
  }
</style>
