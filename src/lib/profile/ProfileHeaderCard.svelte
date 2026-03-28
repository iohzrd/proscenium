<script lang="ts">
  import Avatar from "$lib/Avatar.svelte";
  import type { Profile } from "$lib/types";

  let {
    pubkey,
    displayName,
    profile,
    isSelf,
    onEdit,
  }: {
    pubkey: string;
    displayName: string;
    profile: Profile | null;
    isSelf: boolean;
    onEdit: () => void;
  } = $props();
</script>

<div class="profile-header">
  <Avatar
    {pubkey}
    name={displayName}
    {isSelf}
    ticket={profile?.avatar_ticket}
    size={56}
  />
  <div class="profile-info">
    <h2>{displayName}</h2>
    {#if profile?.visibility && profile.visibility !== "public"}
      <span class="visibility-badge"
        >{profile.visibility === "private" ? "Private" : "Listed"}</span
      >
    {/if}
    {#if profile?.bio}
      <p class="bio">{profile.bio}</p>
    {/if}
  </div>
  {#if isSelf}
    <button class="btn-elevated edit-btn" onclick={onEdit}>Edit</button>
  {/if}
</div>

<style>
  .profile-header {
    display: flex;
    align-items: center;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  .profile-info {
    flex: 1;
    min-width: 0;
  }

  .profile-info h2 {
    margin: 0;
    color: var(--accent-medium);
    font-size: var(--text-xl);
  }

  .bio {
    margin: 0.25rem 0 0;
    color: var(--text-secondary);
    font-size: var(--text-base);
  }

  .visibility-badge {
    display: inline-block;
    font-size: var(--text-sm);
    color: var(--color-warning);
    border: 1px solid var(--color-warning-border);
    border-radius: var(--radius-sm);
    padding: 0.15rem 0.5rem;
    margin-top: 0.25rem;
  }

  .edit-btn {
    padding: 0.4rem 0.85rem;
    font-size: var(--text-base);
    font-weight: 500;
    flex-shrink: 0;
  }
</style>
