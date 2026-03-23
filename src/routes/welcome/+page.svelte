<script lang="ts">
  import { goto } from "$app/navigation";
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { copyToClipboard, detectImageMime } from "$lib/utils";
  import type { Visibility } from "$lib/types";

  let step = $state(0);
  let nodeId = $state("");
  let masterPubkey = $state("");
  let displayName = $state("");
  let bio = $state("");
  let visibility = $state<Visibility>("public");
  let avatarPreview = $state<string | null>(null);
  let avatarHash = $state<string | null>(null);
  let avatarTicket = $state<string | null>(null);
  let saving = $state(false);
  let uploading = $state(false);
  let copyFeedback = $state(false);
  let fileInput = $state<HTMLInputElement>(null!);

  // Seed phrase state
  let seedPhrase = $state("");
  let seedRevealed = $state(false);
  let seedCopyFeedback = $state(false);
  let verifyIndices = $state<number[]>([]);
  let verifyInputs = $state<string[]>(["", "", ""]);
  let verifyError = $state("");
  let verifying = $state(false);

  // Recovery state
  let recoveryPhrase = $state("");
  let recovering = $state(false);
  let recoveryError = $state("");
  let recoveryComplete = $state(false);

  const visibilityOptions: {
    value: Visibility;
    label: string;
    hint: string;
  }[] = [
    {
      value: "public",
      label: "Public",
      hint: "Anyone can see your posts and sync your profile.",
    },
    {
      value: "listed",
      label: "Listed",
      hint: "Profile discoverable, but posts only shared with approved followers.",
    },
    {
      value: "private",
      label: "Private",
      hint: "Only mutual follows can see your posts. Invisible to servers.",
    },
  ];

  onMount(async () => {
    try {
      [nodeId, masterPubkey] = await Promise.all([
        invoke<string>("get_node_id"),
        invoke<string>("get_pubkey"),
      ]);
      const profile = await invoke("get_my_profile");
      if (profile) {
        goto("/");
        return;
      }
      // Check if seed phrase was already backed up (returning user or recovery)
      const backedUp = await invoke<boolean>("is_seed_phrase_backed_up");
      if (backedUp) {
        // Skip straight to profile creation
        step = 3;
      }
    } catch {
      setTimeout(() => location.reload(), 500);
    }
  });

  async function copyNodeId() {
    await copyToClipboard(nodeId);
    copyFeedback = true;
    setTimeout(() => (copyFeedback = false), 1500);
  }

  async function loadSeedPhrase() {
    seedPhrase = await invoke<string>("get_seed_phrase");
  }

  async function copySeedPhrase() {
    await copyToClipboard(seedPhrase);
    seedCopyFeedback = true;
    setTimeout(() => (seedCopyFeedback = false), 1500);
  }

  function pickVerifyIndices() {
    const indices: number[] = [];
    while (indices.length < 3) {
      const i = Math.floor(Math.random() * 24);
      if (!indices.includes(i)) indices.push(i);
    }
    indices.sort((a, b) => a - b);
    verifyIndices = indices;
    verifyInputs = ["", "", ""];
    verifyError = "";
  }

  async function goToVerify() {
    pickVerifyIndices();
    step = 2;
  }

  async function verifySeedPhrase() {
    verifying = true;
    verifyError = "";
    try {
      const checks: [number, string][] = verifyIndices.map((idx, i) => [
        idx,
        verifyInputs[i].trim().toLowerCase(),
      ]);
      const valid = await invoke<boolean>("verify_seed_phrase_words", {
        checks,
      });
      if (valid) {
        await invoke("mark_seed_phrase_backed_up");
        step = 3;
      } else {
        verifyError = "One or more words are incorrect. Please try again.";
      }
    } catch (e) {
      verifyError = `Verification failed: ${e}`;
    }
    verifying = false;
  }

  function skipBackup() {
    step = 3;
  }

  async function recoverFromPhrase() {
    const trimmed = recoveryPhrase.trim();
    const words = trimmed.split(/\s+/);
    if (words.length !== 24) {
      recoveryError = "Recovery phrase must be exactly 24 words.";
      return;
    }
    recovering = true;
    recoveryError = "";
    try {
      await invoke("recover_from_seed_phrase", { phrase: trimmed });
      recoveryComplete = true;
    } catch (e) {
      recoveryError = `Recovery failed: ${e}`;
    }
    recovering = false;
  }

  async function handleAvatarUpload(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    uploading = true;
    try {
      const buffer = await file.arrayBuffer();
      const data = Array.from(new Uint8Array(buffer));
      const result: { hash: string; ticket: string } = await invoke(
        "add_blob_bytes",
        { data },
      );
      avatarHash = result.hash;
      avatarTicket = result.ticket;
      const blob = new Blob([new Uint8Array(buffer)], {
        type: detectImageMime(new Uint8Array(buffer)),
      });
      avatarPreview = URL.createObjectURL(blob);
    } catch (e) {
      console.error("Failed to upload avatar:", e);
    }
    uploading = false;
    input.value = "";
  }

  async function saveProfile() {
    if (!displayName.trim()) return;
    saving = true;
    try {
      await invoke("save_my_profile", {
        displayName: displayName.trim(),
        bio: bio.trim(),
        avatarHash,
        avatarTicket,
        visibility,
      });
      goto("/");
    } catch (e) {
      console.error("Failed to save profile:", e);
    }
    saving = false;
  }

  let seedWords = $derived(seedPhrase ? seedPhrase.split(" ") : []);
</script>

<input
  type="file"
  accept="image/*"
  class="hidden-input"
  bind:this={fileInput}
  onchange={handleAvatarUpload}
/>

<div class="welcome">
  {#if step === 0}
    <div class="step">
      <h1>Welcome</h1>
      <p class="subtitle">
        A peer-to-peer social network. No servers, no middlemen.
      </p>
      <p class="desc">
        Your identity is a cryptographic key pair stored on your device. You own
        your data.
      </p>
      {#if nodeId}
        <div class="node-id-section">
          <p class="label">Your Node ID (transport address)</p>
          <button class="node-id" onclick={copyNodeId} title="Copy Node ID">
            {nodeId.slice(0, 16)}...{nodeId.slice(-8)}
          </button>
          {#if copyFeedback}
            <span class="copied">Copied!</span>
          {/if}
        </div>
      {/if}
      {#if masterPubkey}
        <div class="node-id-section">
          <p class="label">Your Public Key (permanent identity)</p>
          <button
            class="node-id"
            onclick={async () => {
              await copyToClipboard(masterPubkey);
              copyFeedback = true;
              setTimeout(() => (copyFeedback = false), 1500);
            }}
            title="Copy Public Key"
          >
            {masterPubkey.slice(0, 16)}...{masterPubkey.slice(-8)}
          </button>
        </div>
      {/if}
      <button
        class="btn-accent primary-btn"
        onclick={async () => {
          await loadSeedPhrase();
          step = 1;
        }}
      >
        Continue
      </button>
      <button class="skip-btn" onclick={() => (step = -1)}>
        Recover existing identity
      </button>
    </div>
  {:else if step === -1}
    <div class="step">
      {#if recoveryComplete}
        <h2>Identity Recovered</h2>
        <p class="desc">
          Your identity has been restored. Set up your profile to get started.
        </p>
        <button class="btn-accent primary-btn" onclick={() => (step = 3)}>
          Create Profile
        </button>
      {:else}
        <h2>Recover Your Identity</h2>
        <p class="desc">
          Enter your 24-word recovery phrase to restore your identity on this
          device. This will replace any existing identity.
        </p>

        <label class="field">
          <span class="field-label">Recovery Phrase</span>
          <textarea
            class="input-base recovery-textarea"
            bind:value={recoveryPhrase}
            placeholder="Enter your 24 words separated by spaces"
            rows="4"
            autocapitalize="none"
            autocomplete="off"
            spellcheck="false"
          ></textarea>
        </label>

        {#if recoveryError}
          <p class="error">{recoveryError}</p>
        {/if}

        <div class="actions">
          <button class="secondary-btn" onclick={() => (step = 0)}>Back</button>
          <button
            class="btn-accent primary-btn"
            onclick={recoverFromPhrase}
            disabled={!recoveryPhrase.trim() || recovering}
          >
            {recovering ? "Recovering..." : "Recover Identity"}
          </button>
        </div>
      {/if}
    </div>
  {:else if step === 1}
    <div class="step">
      <h2>Back Up Your Recovery Phrase</h2>
      <p class="desc">
        This 24-word phrase is the only way to recover your identity. Write it
        down and store it somewhere safe. If you lose it, your identity cannot
        be recovered.
      </p>

      {#if seedRevealed}
        <div class="seed-grid">
          {#each seedWords as word, i}
            <div class="seed-word">
              <span class="seed-num">{i + 1}</span>
              <span class="seed-text">{word}</span>
            </div>
          {/each}
        </div>

        <div class="seed-actions">
          <button class="secondary-btn" onclick={copySeedPhrase}>
            {seedCopyFeedback ? "Copied!" : "Copy to Clipboard"}
          </button>
        </div>
      {:else}
        <div class="seed-hidden">
          <p class="seed-warning">
            Make sure no one is looking at your screen before revealing.
          </p>
          <button
            class="btn-accent primary-btn"
            onclick={() => (seedRevealed = true)}
          >
            Reveal Recovery Phrase
          </button>
        </div>
      {/if}

      <div class="actions">
        <button class="secondary-btn" onclick={() => (step = 0)}>Back</button>
        {#if seedRevealed}
          <button class="btn-accent primary-btn" onclick={goToVerify}>
            I've Written It Down
          </button>
        {/if}
      </div>

      <button class="skip-btn" onclick={skipBackup}>
        Skip for now (not recommended)
      </button>
    </div>
  {:else if step === 2}
    <div class="step">
      <h2>Verify Your Phrase</h2>
      <p class="desc">
        Enter the following words from your recovery phrase to confirm you've
        saved it correctly.
      </p>

      <div class="verify-fields">
        {#each verifyIndices as wordIdx, i}
          <label class="verify-field">
            <span class="verify-label">Word #{wordIdx + 1}</span>
            <input
              class="input-base"
              type="text"
              bind:value={verifyInputs[i]}
              placeholder="Enter word"
              autocapitalize="none"
              autocomplete="off"
            />
          </label>
        {/each}
      </div>

      {#if verifyError}
        <p class="error">{verifyError}</p>
      {/if}

      <div class="actions">
        <button class="secondary-btn" onclick={() => (step = 1)}>Back</button>
        <button
          class="btn-accent primary-btn"
          onclick={verifySeedPhrase}
          disabled={verifyInputs.some((w) => !w.trim()) || verifying}
        >
          {verifying ? "Verifying..." : "Verify"}
        </button>
      </div>
    </div>
  {:else}
    <div class="step">
      <h2>Create Your Profile</h2>

      <div class="avatar-section">
        <button
          class="avatar-upload"
          onclick={() => fileInput?.click()}
          disabled={uploading}
        >
          {#if avatarPreview}
            <img src={avatarPreview} alt="Avatar" />
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
          maxlength="300"
        ></textarea>
      </label>

      <div class="field">
        <span class="field-label">Visibility</span>
        <div class="visibility-options">
          {#each visibilityOptions as opt}
            <label
              class="visibility-option"
              class:selected={visibility === opt.value}
            >
              <input
                type="radio"
                name="visibility"
                value={opt.value}
                bind:group={visibility}
              />
              <span class="visibility-label">{opt.label}</span>
              <span class="visibility-hint">{opt.hint}</span>
            </label>
          {/each}
        </div>
      </div>

      <div class="actions">
        <button class="secondary-btn" onclick={() => (step = 1)}>Back</button>
        <button
          class="btn-accent primary-btn"
          onclick={saveProfile}
          disabled={!displayName.trim() || saving}
        >
          {saving ? "Saving..." : "Get Started"}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .welcome {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 70vh;
    padding: 2rem 1rem;
  }

  .step {
    max-width: 420px;
    width: 100%;
    text-align: center;
  }

  h1 {
    font-size: var(--text-3xl);
    color: var(--text-primary);
    margin: 0 0 0.5rem;
  }

  h2 {
    font-size: var(--text-2xl);
    color: var(--text-primary);
    margin: 0 0 1.5rem;
  }

  .subtitle {
    color: var(--accent-medium);
    font-size: var(--text-lg);
    margin: 0 0 1rem;
  }

  .desc {
    color: var(--text-secondary);
    font-size: var(--text-base);
    line-height: 1.6;
    margin: 0 0 1.5rem;
  }

  .node-id-section {
    margin-bottom: 2rem;
  }

  .label {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 0 0 0.3rem;
  }

  .node-id {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.4rem 0.8rem;
    color: var(--accent-light);
    font-family: var(--font-mono);
    font-size: var(--text-base);
    cursor: pointer;
  }

  .node-id:hover {
    background: var(--bg-elevated);
  }

  .copied {
    display: block;
    color: var(--color-success);
    font-size: var(--text-sm);
    margin-top: 0.3rem;
  }

  /* Seed phrase grid */
  .seed-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 0.5rem;
    margin-bottom: 1rem;
    text-align: left;
  }

  .seed-word {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.4rem 0.6rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
  }

  .seed-num {
    color: var(--text-tertiary);
    font-size: var(--text-xs);
    min-width: 1.2rem;
  }

  .seed-text {
    color: var(--text-primary);
  }

  .seed-actions {
    margin-bottom: 1.5rem;
  }

  .seed-hidden {
    padding: 2rem 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    margin-bottom: 1.5rem;
  }

  .seed-warning {
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
  }

  .skip-btn {
    display: block;
    margin: 1rem auto 0;
    background: none;
    border: none;
    color: var(--text-tertiary);
    font-size: var(--text-sm);
    cursor: pointer;
    text-decoration: underline;
  }

  .skip-btn:hover {
    color: var(--text-secondary);
  }

  .recovery-textarea {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    resize: vertical;
    min-height: 80px;
  }

  /* Verify */
  .verify-fields {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    margin-bottom: 1rem;
    text-align: left;
  }

  .verify-field {
    display: block;
  }

  .verify-label {
    display: block;
    color: var(--text-secondary);
    font-size: var(--text-sm);
    font-weight: 600;
    margin-bottom: 0.25rem;
  }

  .error {
    color: var(--color-danger);
    font-size: var(--text-sm);
    margin: 0 0 1rem;
  }

  /* Shared */
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

  .avatar-upload img {
    width: 100%;
    height: 100%;
    object-fit: cover;
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
    background: var(--bg-surface);
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
