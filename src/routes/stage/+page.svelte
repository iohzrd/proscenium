<script lang="ts">
  import { page } from "$app/state";
  import { invoke } from "@tauri-apps/api/core";
  import Avatar from "#lib/Avatar.svelte";
  import Icon from "#lib/Icon.svelte";
  import { useToast } from "#lib/composables/index.ts";
  import type { StageParticipant } from "#lib/types.ts";
  import { copyToClipboard, shortId } from "#lib/utils.ts";
  import {
    stageStore,
    selfMuted,
    handRaised,
    hostMuted,
    createStage as createStageCommand,
    joinStage as joinStageCommand,
    leaveStage as leaveStageCommand,
    endStage as endStageCommand,
    toggleStageMute,
    toggleStageHand,
  } from "#lib/stage.svelte.ts";
  import StageLanding from "#lib/stage/StageLanding.svelte";
  import StageChat from "#lib/stage/StageChat.svelte";
  import StageControlBar from "#lib/stage/StageControlBar.svelte";
  import ParticipantPopover from "#lib/stage/ParticipantPopover.svelte";
  import FloatingReactions from "#lib/stage/FloatingReactions.svelte";

  // --- state ---

  // Stage state, chat and reactions live in the store that the layout keeps
  // current; this page holds only form and selection state.
  let stage = $derived(stageStore.stage);

  // landing form
  let createTitle = $state("");
  let joinTicket = $state("");
  let createdTicket = $state<string | null>(null);
  let errorMsg = $state("");
  let busy = $state(false);

  let chatInput = $state("");

  // host controls
  let selectedParticipant = $state<string | null>(null);

  const toast = useToast();

  // derived
  let speakers = $derived(
    stage?.participants.filter(
      (p) => p.role === "Speaker" || p.role === "Host" || p.role === "CoHost",
    ) ?? [],
  );
  let listeners = $derived(
    stage?.participants.filter((p) => p.role === "Listener") ?? [],
  );
  let raisedHands = $derived(
    stage?.participants.filter((p) => p.hand_raised && p.role === "Listener") ??
      [],
  );
  let isHost = $derived(stage?.my_role === "Host");
  let isCoHost = $derived(
    stage?.my_role === "CoHost" || stage?.my_role === "Host",
  );
  let isSpeaker = $derived(
    stage?.my_role === "Speaker" ||
      stage?.my_role === "Host" ||
      stage?.my_role === "CoHost",
  );

  // Prefill the join field from `?ticket=` (set by a stage card) on every URL
  // change. Joining still needs a click.
  $effect(() => {
    const ticket = page.url.searchParams.get("ticket");
    if (ticket) joinTicket = ticket;
  });

  // --- participant name helper ---

  function displayName(p: StageParticipant): string {
    return p.display_name ?? shortId(p.pubkey);
  }

  // --- commands ---

  async function createStage() {
    const title = createTitle.trim();
    if (!title || busy) return;
    busy = true;
    errorMsg = "";
    try {
      createdTicket = await createStageCommand(title);
      createTitle = "";
    } catch (e) {
      errorMsg = String(e);
    }
    busy = false;
  }

  async function joinStage() {
    const ticket = joinTicket.trim();
    if (!ticket || busy) return;
    busy = true;
    errorMsg = "";
    try {
      await joinStageCommand(ticket);
      joinTicket = "";
      createdTicket = null;
    } catch (e) {
      errorMsg = String(e);
    }
    busy = false;
  }

  /** Run a stage command; show `failure` and the error as a toast if it rejects. */
  async function run(action: () => Promise<unknown>, failure: string) {
    try {
      await action();
      return true;
    } catch (e) {
      toast.show(`${failure}: ${e}`);
      console.error(`${failure}:`, e);
      return false;
    }
  }

  async function leaveStage() {
    if (await run(leaveStageCommand, "Failed to leave the stage")) {
      createdTicket = null;
    }
  }

  async function endStage() {
    if (await run(endStageCommand, "Failed to end the stage")) {
      createdTicket = null;
    }
  }

  function toggleMute() {
    return run(toggleStageMute, "Failed to change mute state");
  }

  function toggleHand() {
    return run(toggleStageHand, "Failed to change hand state");
  }

  function sendReaction(emoji: string) {
    return run(
      () => invoke("stage_send_reaction", { emoji }),
      "Reaction not sent",
    );
  }

  async function sendChat() {
    const text = chatInput.trim();
    if (!text) return;
    chatInput = "";
    const sent = await run(
      () => invoke("stage_send_chat", { text }),
      "Message not sent",
    );
    // Put the text back unless the user typed something else meanwhile.
    if (!sent && !chatInput) chatInput = text;
  }

  /** Invoke a host or co-host command for `pubkey`, then close the popover. */
  async function moderate(command: string, pubkey: string, failure: string) {
    await run(() => invoke(command, { pubkey }), failure);
    selectedParticipant = null;
  }

  const promoteSpeaker = (pubkey: string) =>
    moderate("stage_promote_speaker", pubkey, "Promote failed");
  const demoteSpeaker = (pubkey: string) =>
    moderate("stage_demote_speaker", pubkey, "Demote failed");
  const muteSpeaker = (pubkey: string) =>
    moderate("stage_mute_speaker", pubkey, "Mute failed");
  const kickParticipant = (pubkey: string) =>
    moderate("stage_kick", pubkey, "Kick failed");
  const banParticipant = (pubkey: string) =>
    moderate("stage_ban", pubkey, "Ban failed");

  async function copyText(text: string | null | undefined) {
    if (!text) return;
    if (await run(() => copyToClipboard(text), "Copy failed")) {
      toast.show("Copied", "success");
    }
  }

  const QUICK_REACTIONS = [
    "\u{1F44F}",
    "\u{2764}\u{FE0F}",
    "\u{1F525}",
    "\u{1F602}",
    "\u{1F4AF}",
    "\u{1F389}",
  ];
</script>

<div class="stage-page">
  {#if !stage}
    <StageLanding
      bind:createTitle
      bind:joinTicket
      {createdTicket}
      errorMsg={errorMsg || stageStore.notice}
      {busy}
      oncreate={createStage}
      onjoin={joinStage}
      oncopyticket={() => copyText(createdTicket)}
    />
  {:else}
    <!-- Room view -->
    <div class="stage-room">
      <div class="room-header">
        <div class="room-title-row">
          <span class="live-dot"></span>
          <h1 class="room-title">{stage.title}</h1>
          {#if isHost && stage.ticket}
            <button
              class="btn-invite"
              onclick={() => copyText(stage?.ticket)}
              title="Copy invite ticket"
            >
              <Icon name="link" size={13} />
              Invite
            </button>
          {/if}
        </div>
        <div class="room-meta">
          <span class="participant-count"
            >{stage.participants.length} participant{stage.participants
              .length !== 1
              ? "s"
              : ""}</span
          >
          <span class="meta-sep">&middot;</span>
          <span class="my-role-badge role-{stage.my_role.toLowerCase()}"
            >{stage.my_role}</span
          >
        </div>
      </div>

      <div class="room-body">
        <!-- Main: speakers + listeners -->
        <div class="room-main">
          <section class="speaker-section">
            <p class="section-label">On Stage ({speakers.length})</p>
            <div class="speaker-grid">
              {#each speakers as p (p.pubkey)}
                <button
                  class="speaker-card"
                  class:self={p.pubkey === stage.my_pubkey}
                  class:muted={p.self_muted || p.host_muted}
                  onclick={() =>
                    (selectedParticipant =
                      selectedParticipant === p.pubkey ? null : p.pubkey)}
                >
                  <div class="avatar-wrap">
                    <Avatar pubkey={p.pubkey} name={displayName(p)} size={72} />
                    {#if p.self_muted || p.host_muted}
                      <span class="mute-badge"
                        ><Icon name="mic-off" size={11} /></span
                      >
                    {/if}
                    {#if p.role === "Host"}
                      <span class="role-pip host">H</span>
                    {:else if p.role === "CoHost"}
                      <span class="role-pip cohost">C</span>
                    {/if}
                  </div>
                  <span class="speaker-name">{displayName(p)}</span>
                </button>
              {/each}
            </div>
          </section>

          {#if raisedHands.length > 0}
            <section class="hands-section">
              <p class="section-label">
                Raised Hands ({raisedHands.length})
              </p>
              <div class="hands-list">
                {#each raisedHands as p (p.pubkey)}
                  <div class="hand-item">
                    <Avatar pubkey={p.pubkey} name={displayName(p)} size={28} />
                    <span class="hand-name">{displayName(p)}</span>
                    {#if isCoHost}
                      <button
                        class="btn-promote"
                        onclick={() => promoteSpeaker(p.pubkey)}
                      >
                        Promote
                      </button>
                    {/if}
                  </div>
                {/each}
              </div>
            </section>
          {/if}

          {#if listeners.length > 0}
            <section class="listeners-section">
              <p class="section-label">Listeners ({listeners.length})</p>
              <div class="listeners-list">
                {#each listeners as p (p.pubkey)}
                  <button
                    class="listener-item"
                    onclick={() =>
                      (selectedParticipant =
                        selectedParticipant === p.pubkey ? null : p.pubkey)}
                  >
                    <Avatar pubkey={p.pubkey} name={displayName(p)} size={24} />
                    <span class="listener-name">{displayName(p)}</span>
                    {#if p.hand_raised}
                      <span class="hand-icon"
                        ><Icon name="hand" size={12} /></span
                      >
                    {/if}
                  </button>
                {/each}
              </div>
            </section>
          {/if}
        </div>

        <!-- Chat panel -->
        <StageChat
          messages={stageStore.chat}
          bind:chatInput
          onsend={sendChat}
        />
      </div>

      <!-- Bottom controls -->
      <StageControlBar
        {isSpeaker}
        {isHost}
        selfMuted={selfMuted()}
        hostMuted={hostMuted()}
        handRaised={handRaised()}
        quickReactions={QUICK_REACTIONS}
        ontoggleMute={toggleMute}
        ontoggleHand={toggleHand}
        onsendReaction={sendReaction}
        onleave={leaveStage}
        onend={endStage}
      />
    </div>

    <!-- Participant action popover -->
    {#if selectedParticipant && isCoHost}
      {@const target = stage.participants.find(
        (p) => p.pubkey === selectedParticipant,
      )}
      {#if target && target.pubkey !== stage.my_pubkey}
        <ParticipantPopover
          {target}
          onpromote={promoteSpeaker}
          ondemote={demoteSpeaker}
          onmute={muteSpeaker}
          onkick={kickParticipant}
          onban={banParticipant}
          onclose={() => (selectedParticipant = null)}
        />
      {/if}
    {/if}

    <!-- Floating reactions -->
    <FloatingReactions reactions={stageStore.reactions} />
  {/if}
</div>

{#if toast.message}
  <div class="toast" class:error={toast.type === "error"}>
    {toast.message}
  </div>
{/if}

<style>
  .stage-page {
    min-height: 80vh;
    display: flex;
    flex-direction: column;
  }

  /* Room */

  .stage-room {
    display: flex;
    flex-direction: column;
    height: calc(100vh - 3rem);
    min-height: 0;
    position: relative;
  }

  .room-header {
    padding: 0.75rem 0 1rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 1rem;
    flex-shrink: 0;
  }

  .room-title-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }

  .btn-invite {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.3rem 0.7rem;
    background: transparent;
    border: 1px solid var(--accent-medium);
    border-radius: var(--radius-full);
    color: var(--accent-medium);
    font-size: var(--text-xs);
    font-weight: 600;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      color var(--transition-fast);
    margin-left: 0.25rem;
  }

  .btn-invite:hover {
    background: var(--accent-light-hover-bg);
    color: var(--accent-light);
  }

  .live-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--color-error);
    flex-shrink: 0;
    animation: pulse-dot 1.5s infinite;
  }

  @keyframes pulse-dot {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.3;
    }
  }

  .room-title {
    margin: 0;
    font-size: var(--text-xl);
    font-weight: 700;
    color: var(--text-primary);
  }

  .room-meta {
    font-size: var(--text-sm);
    color: var(--text-muted);
    margin-top: 0.35rem;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .participant-count {
    color: var(--text-muted);
  }

  .meta-sep {
    color: var(--border-hover);
  }

  .my-role-badge {
    font-size: var(--text-xs);
    font-weight: 700;
    padding: 0.15rem 0.55rem;
    border-radius: var(--radius-full);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    letter-spacing: 0.02em;
  }

  .my-role-badge.role-host,
  .my-role-badge.role-cohost {
    background: var(--accent);
    color: var(--text-on-accent);
  }

  .my-role-badge.role-speaker {
    background: var(--color-success);
    color: var(--text-on-accent);
  }

  .room-body {
    display: flex;
    gap: 1rem;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .room-main {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    padding-right: 0.25rem;
  }

  .section-label {
    font-size: var(--text-xs);
    font-weight: 700;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin: 0 0 0.75rem;
  }

  /* Speaker grid */

  .speaker-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem;
  }

  .speaker-card {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.55rem;
    padding: 1.25rem 1rem 1rem;
    background: var(--bg-surface);
    border: 2px solid transparent;
    border-radius: var(--radius-2xl);
    cursor: pointer;
    width: 120px;
    transition:
      border-color var(--transition-fast),
      background var(--transition-fast),
      box-shadow var(--transition-fast);
  }

  .speaker-card:hover {
    background: var(--bg-elevated);
    border-color: var(--accent);
  }

  .speaker-card.self {
    border-color: var(--accent-medium);
    box-shadow: 0 0 0 3px var(--accent-light-faint);
  }

  .speaker-card.muted {
    opacity: 0.6;
  }

  .avatar-wrap {
    position: relative;
    display: inline-flex;
  }

  .speaker-name {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--text-secondary);
    max-width: 96px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: center;
  }

  .mute-badge {
    position: absolute;
    bottom: -2px;
    right: -2px;
    background: var(--color-error);
    color: var(--text-on-accent);
    border-radius: 50%;
    width: 20px;
    height: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 2px solid var(--bg-surface);
  }

  .role-pip {
    position: absolute;
    top: -3px;
    right: -3px;
    font-size: 9px;
    font-weight: 800;
    width: 17px;
    height: 17px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 2px solid var(--bg-deep);
  }

  .role-pip.host {
    background: var(--accent);
    color: var(--text-on-accent);
  }

  .role-pip.cohost {
    background: var(--color-warning);
    color: var(--bg-base);
  }

  /* Raised hands */

  .hands-section {
    padding: 0.75rem 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--color-warning-border);
    border-radius: var(--radius-xl);
  }

  .hands-list {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .hand-item {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }

  .hand-name {
    flex: 1;
    font-size: var(--text-sm);
    color: var(--text-secondary);
  }

  .btn-promote {
    font-size: var(--text-xs);
    font-weight: 700;
    padding: 0.25rem 0.65rem;
    border: none;
    border-radius: var(--radius-full);
    cursor: pointer;
    transition: background var(--transition-fast);
    background: var(--color-success);
    color: var(--text-on-accent);
  }

  .btn-promote:hover {
    background: var(--color-success-hover);
  }

  /* Listeners section */

  .listeners-section {
    padding: 0;
  }

  .listeners-list {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .listener-item {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0.2rem 0.55rem 0.2rem 0.3rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .listener-item:hover {
    background: var(--bg-elevated);
  }

  .listener-name {
    font-size: var(--text-xs);
    color: var(--text-secondary);
  }

  .hand-icon {
    color: var(--color-warning, #f59e0b);
    display: flex;
    align-items: center;
  }

  @media (max-width: 640px) {
    .stage-room {
      height: calc(100vh - var(--bottom-nav-height) - 2rem);
    }

    .speaker-card {
      width: 100px;
    }
  }
</style>
