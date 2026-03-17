<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onMount } from "svelte";
  import Avatar from "$lib/Avatar.svelte";
  import Icon from "$lib/Icon.svelte";
  import type {
    StageState,
    StageParticipant,
    StageRole,
    StageEvent,
  } from "$lib/types";
  import { shortId } from "$lib/utils";

  // --- state ---

  let stage = $state<StageState | null>(null);
  let selfMuted = $state(false);
  let handRaised = $state(false);

  // landing form
  let createTitle = $state("");
  let joinTicket = $state("");
  let createdTicket = $state<string | null>(null);
  let errorMsg = $state("");
  let busy = $state(false);

  // chat
  interface ChatMessage {
    pubkey: string;
    name: string;
    text: string;
  }
  let chatMessages = $state<ChatMessage[]>([]);
  let chatInput = $state("");
  let chatEl = $state<HTMLDivElement | null>(null);

  // reactions
  interface FloatingReaction {
    id: number;
    emoji: string;
    x: number;
  }
  let reactions = $state<FloatingReaction[]>([]);
  let reactionCounter = 0;

  // host controls
  let selectedParticipant = $state<string | null>(null);

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

  // --- participant name helper ---

  function displayName(p: StageParticipant): string {
    return p.display_name ?? shortId(p.pubkey);
  }

  // --- event handling ---

  function applyEvent(ev: StageEvent) {
    if (ev.type === "state_snapshot") {
      const { type: _t, ...rest } = ev as { type: string } & StageState;
      stage = rest as StageState;
      // sync derived mute/hand state
      const me = stage.participants.find((p) => p.pubkey === stage!.my_pubkey);
      if (me) {
        selfMuted = me.self_muted;
        handRaised = me.hand_raised;
      }
      return;
    }
    if (!stage) return;

    switch (ev.type) {
      case "participant_joined":
        if (!stage.participants.find((p) => p.pubkey === ev.pubkey)) {
          stage.participants = [
            ...stage.participants,
            {
              pubkey: ev.pubkey,
              role: ev.role,
              display_name: null,
              avatar_hash: null,
              hand_raised: false,
              self_muted: false,
              host_muted: false,
            },
          ];
        }
        break;
      case "participant_left":
        stage.participants = stage.participants.filter(
          (p) => p.pubkey !== ev.pubkey,
        );
        break;
      case "role_changed":
        stage.participants = stage.participants.map((p) =>
          p.pubkey === ev.pubkey ? { ...p, role: ev.role } : p,
        );
        if (ev.pubkey === stage.my_pubkey) {
          stage.my_role = ev.role;
        }
        break;
      case "mute_changed":
        stage.participants = stage.participants.map((p) =>
          p.pubkey === ev.pubkey
            ? { ...p, self_muted: ev.self_muted, host_muted: ev.host_muted }
            : p,
        );
        if (ev.pubkey === stage.my_pubkey) {
          selfMuted = ev.self_muted;
        }
        break;
      case "hand_raised":
        stage.participants = stage.participants.map((p) =>
          p.pubkey === ev.pubkey ? { ...p, hand_raised: true } : p,
        );
        if (ev.pubkey === stage.my_pubkey) handRaised = true;
        break;
      case "hand_lowered":
        stage.participants = stage.participants.map((p) =>
          p.pubkey === ev.pubkey ? { ...p, hand_raised: false } : p,
        );
        if (ev.pubkey === stage.my_pubkey) handRaised = false;
        break;
      case "reaction":
        spawnReaction(ev.emoji);
        break;
      case "chat": {
        const sender = stage.participants.find((p) => p.pubkey === ev.pubkey);
        chatMessages = [
          ...chatMessages,
          {
            pubkey: ev.pubkey,
            name: sender?.display_name ?? shortId(ev.pubkey),
            text: ev.text,
          },
        ];
        setTimeout(() => {
          if (chatEl) chatEl.scrollTop = chatEl.scrollHeight;
        }, 0);
        break;
      }
      case "ended":
        stage = null;
        chatMessages = [];
        break;
      case "kicked":
        errorMsg = "You were kicked from the stage.";
        stage = null;
        chatMessages = [];
        break;
      case "auth_failed":
        console.warn("Stage auth failed:", ev.reason, "from", ev.source);
        break;
    }
  }

  function spawnReaction(emoji: string) {
    const id = ++reactionCounter;
    const x = 20 + Math.random() * 60;
    reactions = [...reactions, { id, emoji, x }];
    setTimeout(() => {
      reactions = reactions.filter((r) => r.id !== id);
    }, 2000);
  }

  // --- commands ---

  async function createStage() {
    if (!createTitle.trim()) return;
    busy = true;
    errorMsg = "";
    try {
      const ticket: string = await invoke("create_stage", {
        title: createTitle.trim(),
      });
      createdTicket = ticket;
      stage = await invoke("get_stage_state");
      createTitle = "";
    } catch (e) {
      errorMsg = String(e);
    }
    busy = false;
  }

  async function joinStage() {
    if (!joinTicket.trim()) return;
    busy = true;
    errorMsg = "";
    try {
      await invoke("join_stage", { ticket: joinTicket.trim() });
      stage = await invoke("get_stage_state");
      joinTicket = "";
      createdTicket = null;
    } catch (e) {
      errorMsg = String(e);
    }
    busy = false;
  }

  async function leaveStage() {
    try {
      await invoke("leave_stage");
    } catch (e) {
      console.error(e);
    }
    stage = null;
    chatMessages = [];
    createdTicket = null;
  }

  async function endStage() {
    try {
      await invoke("end_stage");
    } catch (e) {
      console.error(e);
    }
    stage = null;
    chatMessages = [];
    createdTicket = null;
  }

  async function toggleMute() {
    try {
      selfMuted = await invoke("stage_toggle_mute");
    } catch (e) {
      console.error(e);
    }
  }

  async function toggleHand() {
    try {
      if (handRaised) {
        await invoke("stage_lower_hand");
        handRaised = false;
      } else {
        await invoke("stage_raise_hand");
        handRaised = true;
      }
    } catch (e) {
      console.error(e);
    }
  }

  async function sendReaction(emoji: string) {
    try {
      await invoke("stage_send_reaction", { emoji });
    } catch (e) {
      console.error(e);
    }
  }

  async function sendChat() {
    const text = chatInput.trim();
    if (!text) return;
    chatInput = "";
    try {
      await invoke("stage_send_chat", { text });
    } catch (e) {
      console.error(e);
    }
  }

  async function promoteSpeaker(pubkey: string) {
    try {
      await invoke("stage_promote_speaker", { pubkey });
    } catch (e) {
      console.error(e);
    }
    selectedParticipant = null;
  }

  async function demoteSpeaker(pubkey: string) {
    try {
      await invoke("stage_demote_speaker", { pubkey });
    } catch (e) {
      console.error(e);
    }
    selectedParticipant = null;
  }

  async function copyTicket() {
    if (!createdTicket) return;
    try {
      await navigator.clipboard.writeText(createdTicket);
    } catch {
      // fallback: select text in input
    }
  }

  function handleChatKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendChat();
    }
  }

  // --- lifecycle ---

  onMount(() => {
    const unlisteners: Promise<UnlistenFn>[] = [];

    // Prefill join ticket from ?ticket= URL param (e.g. navigated from a StageCard)
    const params = new URLSearchParams(window.location.search);
    const ticketParam = params.get("ticket");
    if (ticketParam) {
      joinTicket = ticketParam;
    }

    // Restore existing stage state on mount
    invoke<StageState | null>("get_stage_state")
      .then((s) => {
        if (s) stage = s;
      })
      .catch(() => {});

    unlisteners.push(
      listen<StageEvent>("stage-event", (event) => {
        applyEvent(event.payload);
      }),
    );

    return () => {
      unlisteners.forEach((p) => p.then((fn) => fn()));
    };
  });

  const QUICK_REACTIONS = ["👏", "❤️", "🔥", "😂", "💯", "🎉"];
</script>

<div class="stage-page">
  {#if !stage}
    <!-- Landing -->
    <div class="stage-landing">
      <h1 class="stage-heading">
        <Icon name="radio" size={28} />
        Stage
      </h1>
      <p class="stage-subheading">Live audio rooms for your community</p>

      {#if errorMsg}
        <div class="stage-error">{errorMsg}</div>
      {/if}

      <div class="stage-cards">
        <div class="stage-card">
          <h2>Create a Stage</h2>
          <input
            class="stage-input"
            type="text"
            placeholder="Stage title"
            bind:value={createTitle}
            disabled={busy}
            onkeydown={(e) => e.key === "Enter" && createStage()}
          />
          <button
            class="btn-primary"
            onclick={createStage}
            disabled={busy || !createTitle.trim()}
          >
            {busy ? "Creating..." : "Create Stage"}
          </button>
        </div>

        <div class="stage-divider">or</div>

        <div class="stage-card">
          <h2>Join a Stage</h2>
          <input
            class="stage-input"
            type="text"
            placeholder="Paste invite ticket"
            bind:value={joinTicket}
            disabled={busy}
            onkeydown={(e) => e.key === "Enter" && joinStage()}
          />
          <button
            class="btn-primary"
            onclick={joinStage}
            disabled={busy || !joinTicket.trim()}
          >
            {busy ? "Joining..." : "Join Stage"}
          </button>
        </div>
      </div>

      {#if createdTicket}
        <div class="ticket-share">
          <p class="ticket-label">Share this invite ticket:</p>
          <div class="ticket-row">
            <input
              class="ticket-input"
              type="text"
              readonly
              value={createdTicket}
              onclick={(e) => (e.target as HTMLInputElement).select()}
            />
            <button class="btn-icon" onclick={copyTicket} title="Copy">
              <Icon name="copy" size={16} />
            </button>
          </div>
        </div>
      {/if}
    </div>
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
              onclick={() => navigator.clipboard.writeText(stage!.ticket!)}
              title="Copy invite ticket"
            >
              <Icon name="copy" size={14} />
              Invite
            </button>
          {/if}
        </div>
        <div class="room-meta">
          {stage.participants.length} participant{stage.participants.length !==
          1
            ? "s"
            : ""}
          &middot;
          <span class="my-role-badge role-{stage.my_role.toLowerCase()}"
            >{stage.my_role}</span
          >
        </div>
      </div>

      <div class="room-body">
        <!-- Speaker grid -->
        <div class="room-main">
          <section class="speaker-section">
            <h2 class="section-label">On Stage ({speakers.length})</h2>
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
                  <Avatar pubkey={p.pubkey} name={displayName(p)} size={56} />
                  <span class="speaker-name">{displayName(p)}</span>
                  {#if p.self_muted || p.host_muted}
                    <span class="mute-badge"
                      ><Icon name="mic-off" size={12} /></span
                    >
                  {/if}
                  {#if p.role === "Host"}
                    <span class="role-pip host">H</span>
                  {:else if p.role === "CoHost"}
                    <span class="role-pip cohost">C</span>
                  {/if}
                </button>
              {/each}
            </div>
          </section>

          {#if raisedHands.length > 0}
            <section class="hands-section">
              <h2 class="section-label">Raised Hands ({raisedHands.length})</h2>
              <div class="hands-list">
                {#each raisedHands as p (p.pubkey)}
                  <div class="hand-item">
                    <Avatar pubkey={p.pubkey} name={displayName(p)} size={32} />
                    <span class="hand-name">{displayName(p)}</span>
                    {#if isCoHost}
                      <button
                        class="btn-sm btn-promote"
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
              <h2 class="section-label">Listeners ({listeners.length})</h2>
              <div class="listeners-list">
                {#each listeners as p (p.pubkey)}
                  <button
                    class="listener-item"
                    onclick={() =>
                      (selectedParticipant =
                        selectedParticipant === p.pubkey ? null : p.pubkey)}
                  >
                    <Avatar pubkey={p.pubkey} name={displayName(p)} size={28} />
                    <span class="listener-name">{displayName(p)}</span>
                    {#if p.hand_raised}
                      <span class="hand-icon"
                        ><Icon name="hand" size={14} /></span
                      >
                    {/if}
                  </button>
                {/each}
              </div>
            </section>
          {/if}
        </div>

        <!-- Chat panel -->
        <div class="room-sidebar">
          <div class="chat-messages" bind:this={chatEl}>
            {#each chatMessages as msg}
              <div class="chat-msg">
                <span class="chat-author">{msg.name}</span>
                <span class="chat-text">{msg.text}</span>
              </div>
            {/each}
            {#if chatMessages.length === 0}
              <div class="chat-empty">No messages yet</div>
            {/if}
          </div>
          <div class="chat-input-row">
            <input
              class="chat-input"
              type="text"
              placeholder="Chat..."
              bind:value={chatInput}
              onkeydown={handleChatKey}
            />
            <button
              class="btn-send"
              onclick={sendChat}
              disabled={!chatInput.trim()}
            >
              Send
            </button>
          </div>
        </div>
      </div>

      <!-- Bottom controls -->
      <div class="room-controls">
        <div class="controls-left">
          {#if isSpeaker}
            <button
              class="ctrl-btn"
              class:ctrl-muted={selfMuted}
              onclick={toggleMute}
              title={selfMuted ? "Unmute" : "Mute"}
            >
              <Icon name={selfMuted ? "mic-off" : "mic"} size={20} />
              <span>{selfMuted ? "Unmute" : "Mute"}</span>
            </button>
          {:else}
            <button
              class="ctrl-btn"
              class:ctrl-active={handRaised}
              onclick={toggleHand}
              title={handRaised ? "Lower hand" : "Raise hand"}
            >
              <Icon name="hand" size={20} />
              <span>{handRaised ? "Lower Hand" : "Raise Hand"}</span>
            </button>
          {/if}
        </div>

        <div class="reactions-bar">
          {#each QUICK_REACTIONS as emoji}
            <button class="reaction-btn" onclick={() => sendReaction(emoji)}
              >{emoji}</button
            >
          {/each}
        </div>

        <div class="controls-right">
          {#if isHost}
            <button class="ctrl-btn ctrl-danger" onclick={endStage}>
              <Icon name="log-out" size={20} />
              <span>End Stage</span>
            </button>
          {:else}
            <button class="ctrl-btn ctrl-danger" onclick={leaveStage}>
              <Icon name="log-out" size={20} />
              <span>Leave</span>
            </button>
          {/if}
        </div>
      </div>
    </div>

    <!-- Participant action popover -->
    {#if selectedParticipant && isCoHost}
      {@const target = stage.participants.find(
        (p) => p.pubkey === selectedParticipant,
      )}
      {#if target && target.pubkey !== stage.my_pubkey}
        <button
          class="popover-backdrop"
          onclick={() => (selectedParticipant = null)}
          aria-label="Close"
        ></button>
        <div class="participant-popover">
          <div class="popover-header">
            <Avatar
              pubkey={target.pubkey}
              name={displayName(target)}
              size={40}
            />
            <span class="popover-name">{displayName(target)}</span>
          </div>
          <div class="popover-actions">
            {#if target.role === "Listener"}
              <button
                class="popover-btn"
                onclick={() => promoteSpeaker(target.pubkey)}
              >
                Promote to Speaker
              </button>
            {:else if target.role === "Speaker"}
              <button
                class="popover-btn"
                onclick={() => demoteSpeaker(target.pubkey)}
              >
                Demote to Listener
              </button>
            {/if}
          </div>
          <button
            class="popover-close"
            onclick={() => (selectedParticipant = null)}
            aria-label="Close"
          >
            <Icon name="x" size={16} />
          </button>
        </div>
      {/if}
    {/if}

    <!-- Floating reactions -->
    {#each reactions as r (r.id)}
      <span class="floating-reaction" style="left:{r.x}%">{r.emoji}</span>
    {/each}
  {/if}
</div>

<style>
  .stage-page {
    min-height: 80vh;
    display: flex;
    flex-direction: column;
  }

  /* Landing */

  .stage-landing {
    max-width: 640px;
    margin: 2rem auto;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
  }

  .stage-heading {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: var(--text-2xl);
    font-weight: 700;
    color: var(--text-primary);
    margin: 0;
  }

  .stage-subheading {
    color: var(--text-muted);
    margin: 0;
  }

  .stage-error {
    background: var(--danger-bg);
    border: 1px solid var(--danger-border);
    color: var(--danger-text);
    padding: 0.75rem 1rem;
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
  }

  .stage-cards {
    display: flex;
    gap: 1rem;
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .stage-card {
    flex: 1;
    min-width: 220px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .stage-card h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--text-primary);
  }

  .stage-divider {
    align-self: center;
    color: var(--text-muted);
    font-size: var(--text-sm);
    padding: 0 0.5rem;
  }

  .stage-input {
    width: 100%;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-size: var(--text-base);
    padding: 0.6rem 0.75rem;
    box-sizing: border-box;
  }

  .stage-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn-primary {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.65rem 1.25rem;
    font-size: var(--text-base);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-fast);
    width: 100%;
  }

  .btn-primary:hover:not(:disabled) {
    background: var(--accent-dark);
  }

  .btn-primary:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .ticket-share {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 1rem 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .ticket-label {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--text-muted);
    font-weight: 500;
  }

  .ticket-row {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }

  .ticket-input {
    flex: 1;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    padding: 0.5rem 0.75rem;
    font-family: monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: text;
  }

  .ticket-input:focus {
    outline: none;
  }

  .btn-icon {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.5rem;
    cursor: pointer;
    color: var(--text-secondary);
    display: flex;
    align-items: center;
    transition: background var(--transition-fast);
    flex-shrink: 0;
  }

  .btn-icon:hover {
    background: var(--bg-elevated-hover);
  }

  /* Room */

  .stage-room {
    display: flex;
    flex-direction: column;
    height: calc(100vh - var(--bottom-nav-height, 60px) - 3rem);
    min-height: 0;
    position: relative;
  }

  .room-header {
    padding: 0.75rem 0;
    border-bottom: 1px solid var(--border);
    margin-bottom: 1rem;
    flex-shrink: 0;
  }

  .room-title-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .btn-invite {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.3rem 0.65rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-fast);
    margin-left: 0.5rem;
  }

  .btn-invite:hover {
    background: var(--bg-elevated-hover);
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
      opacity: 0.4;
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
    margin-top: 0.25rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .my-role-badge {
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 0.15rem 0.5rem;
    border-radius: var(--radius-full);
    background: var(--bg-elevated);
    color: var(--text-secondary);
  }

  .my-role-badge.role-host,
  .my-role-badge.role-cohost {
    background: var(--accent);
    color: var(--text-on-accent);
  }

  .my-role-badge.role-speaker {
    background: var(--color-success);
    color: white;
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
    gap: 1rem;
  }

  .section-label {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 0 0 0.5rem 0;
  }

  .speaker-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
  }

  .speaker-card {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
    padding: 1rem 0.75rem 0.75rem;
    background: var(--bg-surface);
    border: 2px solid var(--border);
    border-radius: var(--radius-xl);
    cursor: pointer;
    transition:
      border-color var(--transition-fast),
      background var(--transition-fast);
    min-width: 90px;
  }

  .speaker-card:hover {
    background: var(--bg-elevated);
    border-color: var(--accent);
  }

  .speaker-card.self {
    border-color: var(--accent);
  }

  .speaker-card.muted {
    opacity: 0.65;
  }

  .speaker-name {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--text-secondary);
    max-width: 80px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mute-badge {
    position: absolute;
    bottom: 6px;
    right: 6px;
    background: var(--color-error);
    color: white;
    border-radius: 50%;
    width: 18px;
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .role-pip {
    position: absolute;
    top: 4px;
    right: 4px;
    font-size: 9px;
    font-weight: 700;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .role-pip.host {
    background: var(--accent);
    color: var(--text-on-accent);
  }

  .role-pip.cohost {
    background: var(--color-warning, #f59e0b);
    color: #1a1a2e;
  }

  /* Hands section */

  .hands-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 0.75rem 1rem;
  }

  .hands-list {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .hand-item {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .hand-name {
    flex: 1;
    font-size: var(--text-sm);
    color: var(--text-secondary);
  }

  .btn-sm {
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 0.25rem 0.6rem;
    border: none;
    border-radius: var(--radius-md);
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .btn-promote {
    background: var(--color-success);
    color: white;
  }

  .btn-promote:hover {
    background: #16a34a;
  }

  /* Listeners section */

  .listeners-section {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 0.75rem 1rem;
  }

  .listeners-list {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .listener-item {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.25rem 0.5rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .listener-item:hover {
    background: var(--bg-elevated-hover);
  }

  .listener-name {
    font-size: var(--text-xs);
    color: var(--text-secondary);
  }

  .hand-icon {
    color: var(--color-warning, #f59e0b);
  }

  /* Chat */

  .room-sidebar {
    width: 260px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    overflow: hidden;
  }

  .chat-messages {
    flex: 1;
    overflow-y: auto;
    padding: 0.75rem;
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .chat-empty {
    color: var(--text-muted);
    font-size: var(--text-xs);
    text-align: center;
    padding: 1rem;
  }

  .chat-msg {
    font-size: var(--text-sm);
    word-break: break-word;
  }

  .chat-author {
    font-weight: 600;
    color: var(--accent-medium);
    margin-right: 0.3rem;
  }

  .chat-text {
    color: var(--text-secondary);
  }

  .chat-input-row {
    display: flex;
    gap: 0.4rem;
    padding: 0.5rem;
    border-top: 1px solid var(--border);
  }

  .chat-input {
    flex: 1;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-size: var(--text-sm);
    padding: 0.4rem 0.6rem;
  }

  .chat-input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn-send {
    background: var(--accent);
    color: var(--text-on-accent);
    border: none;
    border-radius: var(--radius-md);
    padding: 0.4rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .btn-send:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .btn-send:hover:not(:disabled) {
    background: var(--accent-dark);
  }

  /* Controls bar */

  .room-controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.75rem 0;
    border-top: 1px solid var(--border);
    margin-top: 0.5rem;
    flex-shrink: 0;
    gap: 1rem;
  }

  .controls-left,
  .controls-right {
    flex: 0 0 auto;
  }

  .reactions-bar {
    display: flex;
    gap: 0.25rem;
    flex-wrap: wrap;
    justify-content: center;
  }

  .reaction-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    padding: 0.3rem 0.5rem;
    font-size: 1.1rem;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      transform var(--transition-fast);
  }

  .reaction-btn:hover {
    background: var(--bg-elevated-hover);
    transform: scale(1.15);
  }

  .ctrl-btn {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.5rem 1rem;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    color: var(--text-primary);
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast);
  }

  .ctrl-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .ctrl-btn.ctrl-muted {
    background: var(--color-warning, #f59e0b);
    color: #1a1a2e;
    border-color: transparent;
  }

  .ctrl-btn.ctrl-active {
    background: var(--accent);
    color: var(--text-on-accent);
    border-color: transparent;
  }

  .ctrl-btn.ctrl-danger {
    background: var(--color-error);
    color: white;
    border-color: transparent;
  }

  .ctrl-btn.ctrl-danger:hover {
    background: var(--color-error-dark, #b91c1c);
  }

  /* Popover */

  .popover-backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
    background: transparent;
    border: none;
    padding: 0;
    cursor: default;
  }

  .participant-popover {
    position: fixed;
    bottom: 5rem;
    left: 50%;
    transform: translateX(-50%);
    z-index: 101;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    padding: 1rem 1.25rem;
    box-shadow: var(--shadow-md);
    min-width: 220px;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .popover-header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }

  .popover-name {
    font-weight: 600;
    color: var(--text-primary);
    font-size: var(--text-base);
  }

  .popover-actions {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .popover-btn {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 0.5rem 0.75rem;
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: pointer;
    text-align: left;
    color: var(--text-primary);
    transition: background var(--transition-fast);
  }

  .popover-btn:hover {
    background: var(--bg-elevated-hover);
  }

  .popover-close {
    position: absolute;
    top: 0.5rem;
    right: 0.5rem;
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-muted);
    padding: 0.25rem;
    display: flex;
    align-items: center;
  }

  /* Floating reactions */

  .floating-reaction {
    position: fixed;
    bottom: 6rem;
    font-size: 2rem;
    pointer-events: none;
    animation: float-up 2s ease-out forwards;
    z-index: 200;
  }

  @keyframes float-up {
    0% {
      transform: translateY(0);
      opacity: 1;
    }
    100% {
      transform: translateY(-120px);
      opacity: 0;
    }
  }

  @media (max-width: 640px) {
    .room-sidebar {
      display: none;
    }

    .stage-cards {
      flex-direction: column;
    }

    .stage-divider {
      align-self: stretch;
      text-align: center;
    }
  }
</style>
