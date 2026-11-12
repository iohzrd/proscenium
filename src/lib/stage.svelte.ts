import { ensureMicrophonePermission } from "#lib/microphone.ts";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { SvelteMap } from "svelte/reactivity";
import type { StageAnnouncement, StageEvent, StageState } from "#lib/types.ts";
import { shortId, truncate } from "#lib/utils.ts";

export interface StageChatMessage {
  id: number;
  pubkey: string;
  name: string;
  text: string;
}

export interface StageReaction {
  id: number;
  emoji: string;
  x: number;
}

const MAX_CHAT_MESSAGES = 200;
const MAX_CHAT_CHARS = 500;
const MAX_REACTIONS = 30;
const MAX_REACTION_CHARS = 16;
const REACTION_LIFETIME_MS = 2000;

/**
 * State of the stage the local user is in. One instance per webview: the
 * layout registers the `stage-event` listener, so the state stays current on
 * every route.
 */
export const stageStore = $state<{
  stage: StageState | null;
  chat: StageChatMessage[];
  reactions: StageReaction[];
  /** Reason the last stage was left without a local request; empty otherwise. */
  notice: string;
  /** Problem of the current stage session shown in the room; cleared on leave. */
  warning: string;
}>({
  stage: null,
  chat: [],
  reactions: [],
  notice: "",
  warning: "",
});

let nextItemId = 0;
// True while a local leave_stage or end_stage call is pending.
let leaving = false;

/** Local microphone mute flag of the current snapshot; false without a stage. */
export function selfMuted(): boolean {
  return stageStore.stage?.self_muted ?? false;
}

/** Local raised-hand flag of the current snapshot; false without a stage. */
export function handRaised(): boolean {
  return stageStore.stage?.hand_raised ?? false;
}

/** True if a host or co-host muted the local user. */
export function hostMuted(): boolean {
  const stage = stageStore.stage;
  if (!stage) return false;
  return (
    stage.participants.find((p) => p.pubkey === stage.my_pubkey)?.host_muted ??
    false
  );
}

function clearStage() {
  stageStore.stage = null;
  stageStore.chat = [];
  stageStore.reactions = [];
  stageStore.warning = "";
}

function addReaction(emoji: string) {
  const id = ++nextItemId;
  const x = 20 + Math.random() * 60;
  stageStore.reactions = [
    ...stageStore.reactions,
    { id, emoji: truncate(emoji, MAX_REACTION_CHARS), x },
  ].slice(-MAX_REACTIONS);
  setTimeout(() => {
    stageStore.reactions = stageStore.reactions.filter((r) => r.id !== id);
  }, REACTION_LIFETIME_MS);
}

export function applyStageEvent(ev: StageEvent) {
  if (ev.type === "state_snapshot") {
    const { type: _type, ...snapshot } = ev;
    if (stageStore.stage && stageStore.stage.stage_id !== snapshot.stage_id) {
      stageStore.chat = [];
      stageStore.reactions = [];
    }
    stageStore.stage = snapshot;
    stageStore.notice = "";
    return;
  }
  const stage = stageStore.stage;
  if (!stage) return;

  switch (ev.type) {
    case "participant_joined":
      if (!stage.participants.some((p) => p.pubkey === ev.pubkey)) {
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
      if (ev.pubkey === stage.my_pubkey) stage.my_role = ev.role;
      break;
    case "mute_changed":
      stage.participants = stage.participants.map((p) =>
        p.pubkey === ev.pubkey
          ? { ...p, self_muted: ev.self_muted, host_muted: ev.host_muted }
          : p,
      );
      if (ev.pubkey === stage.my_pubkey) stage.self_muted = ev.self_muted;
      break;
    case "hand_raised":
    case "hand_lowered": {
      const raised = ev.type === "hand_raised";
      stage.participants = stage.participants.map((p) =>
        p.pubkey === ev.pubkey ? { ...p, hand_raised: raised } : p,
      );
      if (ev.pubkey === stage.my_pubkey) stage.hand_raised = raised;
      break;
    }
    case "reaction":
      addReaction(ev.emoji);
      break;
    case "chat": {
      const sender = stage.participants.find((p) => p.pubkey === ev.pubkey);
      stageStore.chat = [
        ...stageStore.chat,
        {
          id: ++nextItemId,
          pubkey: ev.pubkey,
          name: sender?.display_name ?? shortId(ev.pubkey),
          text: truncate(ev.text, MAX_CHAT_CHARS),
        },
      ].slice(-MAX_CHAT_MESSAGES);
      break;
    }
    case "ended":
      if (ev.stage_id !== stage.stage_id) break;
      if (!leaving) stageStore.notice = "The stage ended.";
      clearStage();
      break;
    case "kicked":
      stageStore.notice = "You were removed from the stage.";
      clearStage();
      break;
    case "auth_failed":
      console.warn("Stage auth failed:", ev.reason, "from", ev.source);
      break;
    case "microphone_unavailable":
      stageStore.warning = `Your microphone could not be opened, so the stage does not hear you: ${ev.reason}`;
      break;
  }
}

/** Replace the store's stage with the backend's current state. */
export async function refreshStage(): Promise<void> {
  const state = await invoke<StageState | null>("get_stage_state");
  if (state) {
    stageStore.stage = state;
  } else if (stageStore.stage) {
    clearStage();
  }
}

/** Create a stage and load its state. Returns the invite ticket. */
export async function createStage(title: string): Promise<string> {
  // The host speaks from the start.
  await ensureMicrophonePermission();
  const ticket = await invoke<string>("create_stage", { title });
  stageStore.notice = "";
  await refreshStage();
  return ticket;
}

export async function joinStage(ticket: string): Promise<void> {
  await invoke("join_stage", { ticket });
  stageStore.notice = "";
  await refreshStage();
}

/**
 * Invoke `command` and clear the store only if it succeeded. On failure the
 * store is reloaded from the backend; the error is rethrown if the backend
 * still reports a stage.
 */
async function exitStage(command: "leave_stage" | "end_stage"): Promise<void> {
  leaving = true;
  try {
    await invoke(command);
    clearStage();
  } catch (e) {
    await refreshStage().catch(() => {});
    if (stageStore.stage) throw e;
  } finally {
    leaving = false;
  }
}

export function leaveStage(): Promise<void> {
  return exitStage("leave_stage");
}

export function endStage(): Promise<void> {
  return exitStage("end_stage");
}

export async function toggleStageMute(): Promise<void> {
  const muted = await invoke<boolean>("stage_toggle_mute");
  const stage = stageStore.stage;
  if (!stage) return;
  stage.self_muted = muted;
  stage.participants = stage.participants.map((p) =>
    p.pubkey === stage.my_pubkey ? { ...p, self_muted: muted } : p,
  );
}

export async function toggleStageHand(): Promise<void> {
  const stage = stageStore.stage;
  if (!stage) return;
  const raise = !stage.hand_raised;
  // A raised hand asks to speak: capture starts on promotion.
  if (raise) await ensureMicrophonePermission();
  await invoke(raise ? "stage_raise_hand" : "stage_lower_hand");
  if (stageStore.stage) stageStore.stage.hand_raised = raise;
}

/**
 * Register the `stage-event` listener and load the current stage state.
 * Returns a function that removes the listener.
 */
export function startStageSync(): () => void {
  const unlisten: Promise<UnlistenFn> = listen<StageEvent>(
    "stage-event",
    (event) => applyStageEvent(event.payload),
  );
  refreshStage().catch(() => {
    // Node not ready; the first state_snapshot event sets the state.
  });
  return () => {
    unlisten.then((fn) => fn());
  };
}

// The host repeats its announcement every 30 s. An entry that was not
// repeated within this time is removed.
const ANNOUNCEMENT_TTL_MS = 120_000;
const ANNOUNCEMENT_SWEEP_MS = 15_000;
const MAX_LIVE_STAGES = 50;

/** Announced stages of followed users, keyed by stage id. */
export const liveStages = new SvelteMap<string, StageAnnouncement>();
const announcementSeenAt = new Map<string, number>();

function removeLiveStage(stageId: string) {
  announcementSeenAt.delete(stageId);
  liveStages.delete(stageId);
}

function recordAnnouncement(ann: StageAnnouncement) {
  if (typeof ann?.stage_id !== "string" || typeof ann.ticket !== "string") {
    return;
  }
  announcementSeenAt.set(ann.stage_id, Date.now());
  liveStages.set(ann.stage_id, ann);
  if (liveStages.size > MAX_LIVE_STAGES) {
    let oldestId = "";
    let oldestAt = Infinity;
    for (const [id, at] of announcementSeenAt) {
      if (at < oldestAt) {
        oldestAt = at;
        oldestId = id;
      }
    }
    removeLiveStage(oldestId);
  }
}

function removeExpiredAnnouncements() {
  const cutoff = Date.now() - ANNOUNCEMENT_TTL_MS;
  for (const [id, at] of announcementSeenAt) {
    if (at < cutoff) removeLiveStage(id);
  }
}

/**
 * Register the `stage-announced` and `stage-ended-remote` listeners and the
 * expiry timer for `liveStages`. Returns a function that removes them.
 */
export function startLiveStageTracking(): () => void {
  const unlisteners: Promise<UnlistenFn>[] = [
    listen<StageAnnouncement>("stage-announced", (event) =>
      recordAnnouncement(event.payload),
    ),
    listen<string>("stage-ended-remote", (event) =>
      removeLiveStage(event.payload),
    ),
  ];
  const sweep = setInterval(removeExpiredAnnouncements, ANNOUNCEMENT_SWEEP_MS);
  return () => {
    clearInterval(sweep);
    unlisteners.forEach((p) => p.then((fn) => fn()));
  };
}
