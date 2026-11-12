<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "#lib/Avatar.svelte";
  import ToggleSwitch from "#lib/ToggleSwitch.svelte";
  import VisibilityPicker from "#lib/profile/VisibilityPicker.svelte";
  import type { Visibility } from "#lib/types.ts";
  import { DEFAULT_DISCOVERY_SERVER, uploadFiles } from "#lib/utils.ts";

  let {
    onback,
  }: {
    onback: () => void;
  } = $props();

  let displayName = $state("");
  let bio = $state("");
  let visibility = $state<Visibility>("public");
  let avatarHash = $state<string | null>(null);
  let avatarTicket = $state<string | null>(null);
  let saving = $state(false);
  // True while the registration with the discovery server runs.
  let registering = $state(false);
  let uploading = $state(false);

  const serverHost = new URL(DEFAULT_DISCOVERY_SERVER).host;
  // The user's switch position; null until the user changes it, so the option
  // follows the chosen visibility.
  let serverChoice = $state<boolean | null>(null);
  // Servers refuse Private registrations: the option is off and disabled then.
  let registerOnServer = $derived(
    visibility !== "private" && (serverChoice ?? true),
  );
  const SERVER_HINTS: Record<Visibility, string> = {
    public: `${serverHost} indexes your profile and posts for search, trending and its user directory.`,
    listed: `${serverHost} lists your profile in its user directory. Your posts stay with approved followers.`,
    private: "Servers do not list Private profiles.",
  };
  let errorMessage = $state("");
  let fileInput = $state<HTMLInputElement>(null!);

  async function handleAvatarUpload(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = "";
    if (!file) return;
    uploading = true;
    errorMessage = "";
    try {
      const [uploaded] = await uploadFiles([file]);
      // The preview is rendered by Avatar from the ticket.
      if (uploaded.previewUrl.startsWith("blob:")) {
        URL.revokeObjectURL(uploaded.previewUrl);
      }
      avatarHash = uploaded.hash;
      avatarTicket = uploaded.ticket;
    } catch (err) {
      errorMessage = `Upload failed: ${err instanceof Error ? err.message : err}`;
    }
    uploading = false;
  }

  /**
   * Save the profile, then add and register with the discovery server if the
   * option is on. If the registration fails, the form stays open with the
   * error: the profile is saved, and the user can try again or switch the
   * option off.
   */
  async function saveProfile() {
    if (!displayName.trim()) return;
    saving = true;
    errorMessage = "";
    try {
      await invoke("save_my_profile", {
        displayName: displayName.trim(),
        bio: bio.trim(),
        avatarHash,
        avatarTicket,
        visibility,
      });
    } catch (err) {
      errorMessage = `Failed to save profile: ${err}`;
      saving = false;
      return;
    }
    if (registerOnServer) {
      registering = true;
      try {
        await invoke("add_server", { url: DEFAULT_DISCOVERY_SERVER });
        await invoke("register_with_server", {
          url: DEFAULT_DISCOVERY_SERVER,
          visibility,
        });
      } catch (err) {
        errorMessage = `Your profile was saved, but registering with ${serverHost} failed: ${err}. Try again, or switch the option off to continue.`;
        registering = false;
        saving = false;
        return;
      }
      registering = false;
    }
    await goto("/");
    saving = false;
  }
</script>

<input
  type="file"
  accept="image/png,image/jpeg,image/gif,image/webp"
  class="hidden-input"
  bind:this={fileInput}
  onchange={handleAvatarUpload}
/>

<div class="step">
  <h2>Create Your Profile</h2>

  <div class="avatar-section">
    <button
      class="avatar-upload"
      onclick={() => fileInput?.click()}
      disabled={uploading}
      aria-label="Add a photo"
    >
      {#if avatarTicket}
        <Avatar pubkey="" name={displayName} ticket={avatarTicket} size={76} />
      {:else}
        <span class="avatar-placeholder">
          {uploading ? "..." : "+"}
        </span>
      {/if}
    </button>
    <span class="avatar-hint">Add a photo</span>
  </div>

  <label class="field">
    <span class="field-label">Display Name</span>
    <input
      class="input-base"
      type="text"
      bind:value={displayName}
      placeholder="Your name"
      maxlength="50"
    />
  </label>

  <label class="field">
    <span class="field-label">Bio</span>
    <textarea
      class="input-base"
      bind:value={bio}
      placeholder="Tell people about yourself (optional)"
      rows="3"
      maxlength="300"></textarea>
  </label>

  <div class="field">
    <span class="field-label">Visibility</span>
    <VisibilityPicker bind:value={visibility} />
  </div>

  <div class="server-option">
    <div class="server-info">
      <span class="server-label">List me on {serverHost}</span>
      <p class="server-hint">
        {SERVER_HINTS[visibility]} You can change this later on the Servers page.
      </p>
    </div>
    <ToggleSwitch
      checked={registerOnServer}
      label="List me on {serverHost}"
      disabled={visibility === "private" || saving}
      onchange={(checked) => (serverChoice = checked)}
    />
  </div>

  {#if errorMessage}
    <p class="error" role="alert">{errorMessage}</p>
  {/if}

  <div class="actions">
    <button class="secondary-btn" onclick={onback}>Back</button>
    <button
      class="btn-accent primary-btn"
      onclick={saveProfile}
      disabled={!displayName.trim() || saving || uploading}
    >
      {registering
        ? `Registering with ${serverHost}...`
        : saving
          ? "Saving..."
          : "Get Started"}
    </button>
  </div>
</div>

<style>
  .step {
    max-width: 420px;
    width: 100%;
    text-align: center;
  }

  h2 {
    font-size: var(--text-2xl);
    color: var(--text-primary);
    margin: 0 0 1.5rem;
  }

  .avatar-section {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 1.5rem;
  }

  .avatar-upload {
    width: 80px;
    height: 80px;
    border-radius: 50%;
    border: 2px dashed var(--border-hover);
    background: var(--bg-surface);
    cursor: pointer;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .avatar-upload:hover {
    border-color: var(--accent);
  }

  .avatar-placeholder {
    color: var(--text-tertiary);
    font-size: var(--text-icon-xl);
  }

  .avatar-hint {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
  }

  .field {
    display: block;
    text-align: left;
  }

  .field textarea {
    resize: vertical;
    min-height: 60px;
  }

  .server-option {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    margin: 0.75rem 0 1rem;
    text-align: left;
  }

  .server-info {
    flex: 1;
  }

  .server-label {
    font-weight: 600;
    font-size: var(--text-sm);
    color: var(--text-primary);
  }

  .server-hint {
    color: var(--text-secondary);
    font-size: var(--text-xs);
    margin: 0.25rem 0 0;
    line-height: 1.4;
  }

  .error {
    color: var(--color-error);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
  }

  .actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.5rem;
  }

  .primary-btn {
    flex: 1;
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
  }

  .secondary-btn {
    background: none;
    border: 1px solid var(--border-hover);
    color: var(--text-secondary);
    border-radius: var(--radius-lg);
    padding: 0.7rem 1.5rem;
    font-size: var(--text-lg);
    cursor: pointer;
    transition:
      color var(--transition-fast),
      border-color var(--transition-fast);
  }

  .secondary-btn:hover {
    color: var(--accent-light);
    border-color: var(--accent);
  }
</style>
