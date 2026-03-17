pub mod auth;
pub mod control;
pub mod fanout;
pub mod mixer;
pub mod relay;
pub mod topology;

use crate::audio::{
    AudioCapture, AudioPlayback, EchoCanceller, OpusDecoder, OpusEncoder, SAMPLES_PER_FRAME,
    TAG_NORMAL, read_audio_frame, write_audio_frame,
};
use crate::error::AppError;
use crate::gossip::GossipService;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use control::ControlPlane;
use fanout::Fanout;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::Gossip;
use iroh_social_types::{
    STAGE_ALPN, SignedStageControl, StageControl, StageEvent, StageRole, StageState, StageTicket,
    now_millis, short_id, sign_stage_control,
};
use mixer::{MixerHandle, spawn_mixer};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use topology::TopologyManager;

/// Connection type byte: sent as first byte of the bi-stream by the initiating side.
const CONN_TYPE_SPEAKER: u8 = 0x01;
const CONN_TYPE_LISTENER: u8 = 0x02;

// ---- Participant tracking -----------------------------------------------

/// Live participant entry, updated by Presence heartbeats.
struct Participant {
    pubkey: String,
    role: StageRole,
    hand_raised: bool,
    self_muted: bool,
    host_muted: bool,
    last_seen_ms: u64,
    /// Transport NodeId of this participant, populated from Presence heartbeats.
    node_id: Option<String>,
}

// ---- SFU hub (host-only) ------------------------------------------------

/// Host-only shared state for speaker SFU forwarding.
///
/// When a speaker connects, the host creates a `Fanout` for their mic stream
/// and opens uni-streams on all other speaker connections to subscribe them.
/// Wrapped in `Arc<Mutex>` so the connection handler task can update it without
/// going through the actor command channel.
struct SfuHub {
    /// QUIC node-id → their mic Fanout (raw Opus forwarded to other speakers).
    fanouts: std::collections::HashMap<String, Arc<Fanout>>,
    /// QUIC node-id → their live Connection (for opening new uni-streams).
    connections: std::collections::HashMap<String, iroh::endpoint::Connection>,
}

// ---- Speaker-side PCM mixer (speaker role only) -------------------------

/// Per-stream buffer cap: 5 frames (~100ms).
const MAX_STREAM_BUFFER: usize = crate::audio::FRAME_SIZE * 5;

enum SpeakerMixerCmd {
    AddStream {
        id: u32,
        rx: mpsc::Receiver<Vec<f32>>,
    },
    RemoveStream(u32),
    /// Replace the AEC far-end reference channel (called on each reconnect).
    SetFarEndTx(mpsc::Sender<Vec<f32>>),
}

/// Cheap-to-clone handle to the SpeakerMixer actor.
#[derive(Clone)]
struct SpeakerMixerHandle {
    cmd_tx: mpsc::Sender<SpeakerMixerCmd>,
}

impl SpeakerMixerHandle {
    async fn add_stream(&self, id: u32, rx: mpsc::Receiver<Vec<f32>>) {
        let _ = self
            .cmd_tx
            .send(SpeakerMixerCmd::AddStream { id, rx })
            .await;
    }

    async fn remove_stream(&self, id: u32) {
        let _ = self.cmd_tx.send(SpeakerMixerCmd::RemoveStream(id)).await;
    }

    async fn set_far_end_tx(&self, tx: mpsc::Sender<Vec<f32>>) {
        let _ = self.cmd_tx.send(SpeakerMixerCmd::SetFarEndTx(tx)).await;
    }
}

/// Spawn the speaker-side PCM mixer actor.
///
/// Drains per-stream sample buffers on a 20 ms tick, sums them (clamp to [-1,1]),
/// and pushes the mix to `AudioPlayback`. The AEC far-end reference channel is
/// wired via `SpeakerMixerHandle::set_far_end_tx` (called each reconnect).
fn spawn_speaker_mixer(cancel: CancellationToken) -> SpeakerMixerHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    tokio::spawn(run_speaker_mixer(cmd_rx, cancel));
    SpeakerMixerHandle { cmd_tx }
}

async fn run_speaker_mixer(mut cmd_rx: mpsc::Receiver<SpeakerMixerCmd>, cancel: CancellationToken) {
    let (mut prod, _playback) = match AudioPlayback::start() {
        Ok(p) => p,
        Err(e) => {
            log::error!("[stage-speaker-mixer] failed to start playback: {e}");
            return;
        }
    };

    let mut buffers: HashMap<u32, Vec<f32>> = HashMap::new();
    let mut streams: Vec<(u32, mpsc::Receiver<Vec<f32>>)> = Vec::new();
    let mut far_end_tx: Option<mpsc::Sender<Vec<f32>>> = None;
    let mut frames: u32 = 0;

    let mut mix_interval = tokio::time::interval(std::time::Duration::from_millis(20));
    mix_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SpeakerMixerCmd::AddStream { id, rx }) => {
                        buffers.insert(id, Vec::new());
                        streams.push((id, rx));
                    }
                    Some(SpeakerMixerCmd::RemoveStream(id)) => {
                        buffers.remove(&id);
                        streams.retain(|(i, _)| *i != id);
                    }
                    Some(SpeakerMixerCmd::SetFarEndTx(tx)) => {
                        far_end_tx = Some(tx);
                    }
                    None => break,
                }
            }

            _ = mix_interval.tick() => {
                // Drain all streams into per-stream buffers.
                let mut dead: Vec<u32> = Vec::new();
                for (id, rx) in &mut streams {
                    loop {
                        match rx.try_recv() {
                            Ok(samples) => {
                                if let Some(buf) = buffers.get_mut(id) {
                                    buf.extend_from_slice(&samples);
                                    if buf.len() > MAX_STREAM_BUFFER {
                                        let excess = buf.len() - MAX_STREAM_BUFFER;
                                        buf.drain(..excess);
                                    }
                                }
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                dead.push(*id);
                                break;
                            }
                        }
                    }
                }
                for id in dead {
                    buffers.remove(&id);
                    streams.retain(|(i, _)| *i != id);
                }

                // Sum contributions and clamp.
                let mut mix = vec![0.0f32; crate::audio::FRAME_SIZE];
                for buf in buffers.values_mut() {
                    let available = buf.len().min(crate::audio::FRAME_SIZE);
                    for (i, s) in buf.drain(..available).enumerate() {
                        mix[i] += s;
                    }
                }
                let mix: Vec<f32> = mix.iter().map(|s| s.clamp(-1.0, 1.0)).collect();

                if let Some(ref tx) = far_end_tx {
                    let _ = tx.try_send(mix.clone());
                }
                prod.push(&mix);
                frames = frames.wrapping_add(1);
            }
        }
    }

    log::info!("[stage-speaker-mixer] stopped ({frames} frames mixed)");
}

// ---- Command enum -------------------------------------------------------

#[allow(dead_code)]
pub enum StageCommand {
    // Lifecycle
    CreateStage {
        title: String,
        reply: oneshot::Sender<Result<StageTicket, AppError>>,
    },
    JoinStage {
        ticket: StageTicket,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    LeaveStage {
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    EndStage {
        reply: oneshot::Sender<Result<(), AppError>>,
    },

    // Incoming transport connection from a remote peer
    IncomingConnection(Connection),

    // Incoming control message from the gossip receive task
    ControlReceived(SignedStageControl),

    // Periodic presence expiry sweep (sent by a background timer task)
    SweepPresence,

    // Host moderation
    PromoteSpeaker {
        pubkey: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    DemoteSpeaker {
        pubkey: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    MuteSpeaker {
        pubkey: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    Kick {
        pubkey: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    Ban {
        pubkey: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },

    // Participant actions
    RaiseHand {
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    LowerHand {
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    ToggleSelfMute {
        reply: oneshot::Sender<Result<bool, AppError>>,
    },
    SendReaction {
        emoji: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    SendChat {
        text: String,
        reply: oneshot::Sender<Result<(), AppError>>,
    },

    // Relay: volunteer this node as a relay for the current stage
    VolunteerAsRelay {
        capacity: u32,
        reply: oneshot::Sender<Result<(), AppError>>,
    },

    // Query
    GetState {
        reply: oneshot::Sender<Option<StageState>>,
    },
}

// ---- Actor handle -------------------------------------------------------

/// Cheap-to-clone handle to the StageActor command channel.
#[derive(Clone)]
pub struct StageActorHandle {
    cmd_tx: mpsc::Sender<StageCommand>,
}

impl StageActorHandle {
    pub async fn create_stage(&self, title: String) -> Result<StageTicket, AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::CreateStage { title, reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn join_stage(&self, ticket: StageTicket) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::JoinStage { ticket, reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn leave_stage(&self) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::LeaveStage { reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn end_stage(&self) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::EndStage { reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn promote_speaker(&self, pubkey: String) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::PromoteSpeaker { pubkey, reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn demote_speaker(&self, pubkey: String) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::DemoteSpeaker { pubkey, reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn get_state(&self) -> Option<StageState> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(StageCommand::GetState { reply: tx })
            .await
            .is_err()
        {
            return None;
        }
        rx.await.unwrap_or(None)
    }

    pub async fn toggle_self_mute(&self) -> Result<bool, AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::ToggleSelfMute { reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn raise_hand(&self) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::RaiseHand { reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn lower_hand(&self) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::LowerHand { reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn send_reaction(&self, emoji: String) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::SendReaction { emoji, reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn send_chat(&self, text: String) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::SendChat { text, reply: tx })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn volunteer_as_relay(&self, capacity: u32) -> Result<(), AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(StageCommand::VolunteerAsRelay {
                capacity,
                reply: tx,
            })
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    async fn incoming_connection(&self, conn: Connection) {
        let _ = self
            .cmd_tx
            .send(StageCommand::IncomingConnection(conn))
            .await;
    }
}

// ---- ActiveStage --------------------------------------------------------

/// All live state for an active Stage session.
struct ActiveStage {
    stage_id: String,
    title: String,
    host_pubkey: String,
    my_pubkey: String,
    my_role: StageRole,
    self_muted: bool,
    started_at: u64,
    /// Live participant list, keyed by master pubkey.
    participants: HashMap<String, Participant>,
    /// Gossip control plane for this room.
    control_plane: ControlPlane,
    /// Cancellation token for this stage session's background tasks.
    cancel: CancellationToken,
    /// Host/co-host only: the audio mixer actor handle.
    mixer_handle: Option<MixerHandle>,
    /// Host/co-host only: fanout for distributing the mixed stream to subscribers.
    fanout: Option<Arc<Fanout>>,
    /// Host only: listener assignment topology manager.
    #[allow(dead_code)]
    topology: Option<TopologyManager>,
    /// Set when this node is acting as a relay: handle to the relay actor.
    relay_handle: Option<relay::RelayHandle>,
    /// Invite ticket (host only; `None` for joiners).
    ticket: Option<StageTicket>,
    /// Transport NodeId we're currently receiving audio from (host or an upstream relay).
    listener_upstream_id: Option<String>,
    /// Cancellation token for the current listener audio pipeline.
    /// Cancelled on relay reassignment to restart the pipeline pointing at the new upstream.
    listener_pipeline_cancel: Option<CancellationToken>,
    /// Speaker-role only: PCM mixer actor handle. Each accepted uni-stream is registered
    /// as a separate input; the actor sums them on a 20 ms tick and drives AudioPlayback.
    speaker_mixer: Option<SpeakerMixerHandle>,
    /// Host-only: SFU hub — per-speaker Fanouts and Connections for raw Opus forwarding.
    sfu_hub: Option<Arc<tokio::sync::Mutex<SfuHub>>>,
    /// Host-only: Fanout for the host's own voice, fed by the mixer each tick.
    /// Speakers subscribe to this via uni-streams opened on their connection.
    host_sfu_fanout: Option<Arc<Fanout>>,
    /// The host's stable transport NodeId. Set from ticket on join, or own NodeId on create.
    /// Used by the speaker pipeline to dial the host directly — never a relay's NodeId.
    host_node_id: String,
    /// Transport NodeIds of banned participants. Checked at connection time.
    banned_node_ids: std::collections::HashSet<String>,
}

// ---- StageActor ---------------------------------------------------------

struct StageActor {
    cmd_rx: mpsc::Receiver<StageCommand>,
    /// Cloned sender to ourselves, used to inject ControlReceived messages.
    self_tx: mpsc::Sender<StageCommand>,
    endpoint: Endpoint,
    gossip: Gossip,
    identity: SharedIdentity,
    #[allow(dead_code)]
    storage: Arc<Storage>,
    gossip_service: GossipService,
    app_handle: AppHandle,
    active: Option<ActiveStage>,
}

impl StageActor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        cmd_rx: mpsc::Receiver<StageCommand>,
        self_tx: mpsc::Sender<StageCommand>,
        endpoint: Endpoint,
        gossip: Gossip,
        identity: SharedIdentity,
        storage: Arc<Storage>,
        gossip_service: GossipService,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            cmd_rx,
            self_tx,
            endpoint,
            gossip,
            identity,
            storage,
            gossip_service,
            app_handle,
            active: None,
        }
    }

    async fn run(mut self) {
        while let Some(cmd) = self.cmd_rx.recv().await {
            self.handle(cmd).await;
        }
    }

    async fn handle(&mut self, cmd: StageCommand) {
        match cmd {
            StageCommand::CreateStage { title, reply } => {
                let _ = reply.send(self.create_stage(title).await);
            }
            StageCommand::JoinStage { ticket, reply } => {
                let _ = reply.send(self.join_stage(ticket).await);
            }
            StageCommand::LeaveStage { reply } => {
                let _ = reply.send(self.leave_stage().await);
            }
            StageCommand::EndStage { reply } => {
                let _ = reply.send(self.end_stage().await);
            }
            StageCommand::IncomingConnection(conn) => {
                if let Some(active) = &self.active {
                    let my_role = active.my_role;
                    let fanout = active.fanout.clone();
                    let mixer_handle = active.mixer_handle.clone();
                    let relay_handle = active.relay_handle.clone();
                    let sfu_hub = active.sfu_hub.clone();
                    let host_sfu_fanout = active.host_sfu_fanout.clone();
                    let banned_node_ids = active.banned_node_ids.clone();
                    let conn_cancel = active.cancel.child_token();
                    tokio::spawn(async move {
                        handle_incoming_connection(
                            conn,
                            my_role,
                            fanout,
                            mixer_handle,
                            relay_handle,
                            sfu_hub,
                            host_sfu_fanout,
                            banned_node_ids,
                            conn_cancel,
                        )
                        .await;
                    });
                } else {
                    let remote = conn.remote_id().to_string();
                    log::debug!(
                        "[stage] closing connection from {} (not in stage)",
                        short_id(&remote)
                    );
                    conn.close(0u32.into(), b"not in a stage");
                }
            }
            StageCommand::ControlReceived(signed) => {
                self.handle_control(signed).await;
            }
            StageCommand::PromoteSpeaker { pubkey, reply } => {
                let _ = reply.send(self.promote_speaker(pubkey).await);
            }
            StageCommand::DemoteSpeaker { pubkey, reply } => {
                let _ = reply.send(self.demote_speaker(pubkey).await);
            }
            StageCommand::MuteSpeaker { pubkey, reply } => {
                let _ = reply.send(self.mute_speaker(pubkey).await);
            }
            StageCommand::Kick { pubkey, reply } => {
                let _ = reply.send(self.kick(pubkey).await);
            }
            StageCommand::Ban { pubkey, reply } => {
                let _ = reply.send(self.ban(pubkey).await);
            }
            StageCommand::SweepPresence => {
                self.sweep_presence();
            }
            StageCommand::VolunteerAsRelay { capacity, reply } => {
                let _ = reply.send(self.volunteer_as_relay(capacity).await);
            }
            StageCommand::RaiseHand { reply } => {
                let _ = reply.send(self.raise_hand().await);
            }
            StageCommand::LowerHand { reply } => {
                let _ = reply.send(self.lower_hand().await);
            }
            StageCommand::ToggleSelfMute { reply } => {
                let _ = reply.send(self.toggle_self_mute().await);
            }
            StageCommand::SendReaction { emoji, reply } => {
                let _ = reply.send(self.send_reaction(emoji).await);
            }
            StageCommand::SendChat { text, reply } => {
                let _ = reply.send(self.send_chat(text).await);
            }
            StageCommand::GetState { reply } => {
                let _ = reply.send(self.build_state());
            }
        }
    }

    // ---- Lifecycle ---------------------------------------------------------

    async fn create_stage(&mut self, title: String) -> Result<StageTicket, AppError> {
        if self.active.is_some() {
            return Err(AppError::Other("already in a stage".into()));
        }

        let stage_id = crate::util::generate_id();
        let id = self.identity.read().await;
        let my_pubkey = id.master_pubkey.clone();
        let node_id = id.transport_node_id.clone();
        let signing_key = id.signing_key.clone();
        drop(id);

        // host_pubkey in the ticket must be the *signing* key's public key —
        // that is the key that signs audio stream checkpoints. master_pubkey
        // is a different key and would cause InvalidSignature on every checkpoint.
        let ticket = StageTicket {
            stage_id: stage_id.clone(),
            host_pubkey: signing_key.public().to_string(),
            host_node_id: node_id.clone(),
            title: title.clone(),
        };

        let cancel = CancellationToken::new();
        let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<SignedStageControl>(64);

        let control_plane = ControlPlane::start(
            &self.gossip,
            &stage_id,
            vec![],
            my_pubkey.clone(),
            StageRole::Host,
            signing_key.clone(),
            Some(node_id.clone()),
            ctrl_tx,
            cancel.child_token(),
        )
        .await?;

        // Broadcast Announce so followers can discover the room via user feed gossip
        // (Phase 3: also broadcast on user feed topic via GossipService)
        let announce = sign_stage_control(
            StageControl::Announce {
                stage_id: stage_id.clone(),
                title: title.clone(),
                host_pubkey: my_pubkey.clone(),
                started_at: now_millis(),
            },
            &signing_key.public().to_string(),
            &signing_key,
            now_millis(),
        );
        let _ = control_plane.broadcast(&announce).await;

        // Host SFU fanout: carries host's own voice to all connected speakers.
        // Created before the mixer so it can be passed in at spawn time.
        let host_sfu_fanout = Arc::new(Fanout::new());
        let sfu_hub = Arc::new(tokio::sync::Mutex::new(SfuHub {
            fanouts: HashMap::new(),
            connections: HashMap::new(),
        }));

        // Start the host audio mixer (owns encoders, auth state, per-speaker PCM buffers)
        let (mixer_handle, fanout) = spawn_mixer(
            signing_key.clone(),
            host_sfu_fanout.clone(),
            cancel.child_token(),
        )?;

        // Add host's own microphone as a speaker input so the host's voice is included in the mix.
        // Also wire a direct PCM channel for mix-minus local playback (host hears others only).
        // AEC: raw mic → AEC bridge (uses mix-minus as far-end reference) → mixer.
        let (cap_tx, mut cap_raw_rx) = mpsc::channel::<Vec<f32>>(32);
        let (aec_out_tx, aec_out_rx) = mpsc::channel::<Vec<f32>>(32);
        let (far_end_tx, mut far_end_rx) = mpsc::channel::<Vec<f32>>(20);
        let (host_pcm_tx, host_pcm_rx) = mpsc::channel::<Vec<f32>>(32);
        match mixer_handle
            .add_speaker(my_pubkey.clone(), aec_out_rx)
            .await
        {
            Ok(()) => {
                // Register mix-minus channel: host only hears speakers other than themselves.
                let _ = mixer_handle
                    .set_host_speaker(my_pubkey.clone(), host_pcm_tx)
                    .await;
                let host_pb_cancel = cancel.child_token();
                tokio::spawn(async move {
                    run_host_playback(host_pcm_rx, far_end_tx, host_pb_cancel).await;
                });
                // AEC bridge: dedicated std thread owns VoipAec3 (which is !Send).
                // Uses tokio's blocking_recv/try_recv so no async runtime needed.
                std::thread::spawn(move || {
                    let mut aec = match EchoCanceller::new() {
                        Ok(a) => a,
                        Err(e) => {
                            log::error!("[stage-host] failed to create AEC: {e}");
                            return;
                        }
                    };
                    while let Some(s) = cap_raw_rx.blocking_recv() {
                        while let Ok(r) = far_end_rx.try_recv() {
                            aec.render(&r);
                        }
                        let cleaned = aec.process_capture(&s);
                        if !cleaned.is_empty() && aec_out_tx.blocking_send(cleaned).is_err() {
                            break;
                        }
                    }
                });
                let mic_cancel = cancel.child_token();
                tokio::spawn(async move {
                    let _capture = match AudioCapture::start(cap_tx) {
                        Ok(c) => c,
                        Err(e) => {
                            log::warn!("[stage-host] host mic capture failed: {e}");
                            return;
                        }
                    };
                    mic_cancel.cancelled().await;
                });
            }
            Err(e) => {
                log::warn!("[stage-host] failed to register host mic in mixer: {e}");
            }
        }

        let now = now_millis();
        let mut participants = HashMap::new();
        participants.insert(
            my_pubkey.clone(),
            Participant {
                pubkey: my_pubkey.clone(),
                role: StageRole::Host,
                hand_raised: false,
                self_muted: false,
                host_muted: false,
                last_seen_ms: now,
                node_id: Some(node_id.clone()),
            },
        );

        let topology = TopologyManager::new(node_id.clone(), 15);

        self.active = Some(ActiveStage {
            stage_id: stage_id.clone(),
            title,
            host_pubkey: my_pubkey.clone(),
            my_pubkey: my_pubkey.clone(),
            my_role: StageRole::Host,
            self_muted: false,
            started_at: now,
            participants,
            control_plane,
            cancel: cancel.clone(),
            mixer_handle: Some(mixer_handle),
            fanout: Some(fanout),
            topology: Some(topology),
            relay_handle: None,
            ticket: Some(ticket.clone()),
            listener_upstream_id: None,
            listener_pipeline_cancel: None,
            speaker_mixer: None,
            sfu_hub: Some(sfu_hub),
            host_sfu_fanout: Some(host_sfu_fanout),
            host_node_id: node_id.clone(),
            banned_node_ids: std::collections::HashSet::new(),
        });

        // Spawn periodic presence expiry sweep.
        {
            let sweep_tx = self.cmd_rx_forwarder();
            let sweep_cancel = cancel.child_token();
            tokio::spawn(async move {
                let interval = control::PRESENCE_EXPIRY;
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                ticker.tick().await; // skip immediate first tick
                loop {
                    tokio::select! {
                        _ = sweep_cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            if sweep_tx.send(StageCommand::SweepPresence).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Forward incoming control messages to the actor via the command channel
        let cmd_tx = self.cmd_rx_forwarder();
        tokio::spawn(async move {
            while let Some(msg) = ctrl_rx.recv().await {
                if cmd_tx
                    .send(StageCommand::ControlReceived(msg))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Announce the stage on our user feed so followers can discover and join.
        // Re-broadcast every 30 s so followers who connect after the initial
        // announcement still see it within a short window.
        {
            let gs = self.gossip_service.clone();
            let ann_stage_id = stage_id.clone();
            let ann_title = self.active.as_ref().unwrap().title.clone();
            let ann_ticket = ticket.clone();
            let ann_pubkey = my_pubkey.clone();
            let ann_cancel = cancel.child_token();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        _ = ann_cancel.cancelled() => break,
                        _ = interval.tick() => {
                            if let Err(e) = gs
                                .broadcast_stage_announcement(
                                    ann_stage_id.clone(),
                                    ann_title.clone(),
                                    ann_ticket.clone(),
                                    ann_pubkey.clone(),
                                    now,
                                )
                                .await
                            {
                                log::warn!("[stage] failed to broadcast stage announcement: {e}");
                            }
                        }
                    }
                }
            });
        }

        log::info!("[stage] created stage {}", short_id(&stage_id));
        self.emit_state_snapshot();
        Ok(ticket)
    }

    async fn join_stage(&mut self, ticket: StageTicket) -> Result<(), AppError> {
        if self.active.is_some() {
            return Err(AppError::Other("already in a stage".into()));
        }

        let stage_id = ticket.stage_id.clone();
        let id = self.identity.read().await;
        let my_pubkey = id.master_pubkey.clone();
        let my_node_id = id.transport_node_id.clone();
        let signing_key = id.signing_key.clone();
        drop(id);

        // Resolve the host's transport NodeId so gossip can bootstrap
        let host_node_ids: Vec<EndpointId> = ticket.host_node_id.parse().ok().into_iter().collect();

        let cancel = CancellationToken::new();
        let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<SignedStageControl>(64);

        let control_plane = ControlPlane::start(
            &self.gossip,
            &stage_id,
            host_node_ids,
            my_pubkey.clone(),
            StageRole::Listener,
            signing_key,
            Some(my_node_id),
            ctrl_tx,
            cancel.child_token(),
        )
        .await?;

        let now = now_millis();
        // Dedicated cancel token for the listener pipeline — can be replaced on relay reassignment
        let listener_cancel = cancel.child_token();
        self.active = Some(ActiveStage {
            stage_id: stage_id.clone(),
            title: ticket.title.clone(),
            host_pubkey: ticket.host_pubkey.clone(),
            my_pubkey: my_pubkey.clone(),
            my_role: StageRole::Listener,
            self_muted: false,
            started_at: now,
            participants: HashMap::new(),
            control_plane,
            cancel: cancel.clone(),
            mixer_handle: None,
            fanout: None,
            topology: None,
            relay_handle: None,
            ticket: None,
            listener_upstream_id: Some(ticket.host_node_id.clone()),
            listener_pipeline_cancel: Some(listener_cancel.clone()),
            speaker_mixer: None,
            sfu_hub: None,
            host_sfu_fanout: None,
            host_node_id: ticket.host_node_id.clone(),
            banned_node_ids: std::collections::HashSet::new(),
        });

        // Spawn periodic presence expiry sweep.
        {
            let sweep_tx = self.cmd_rx_forwarder();
            let sweep_cancel = cancel.child_token();
            tokio::spawn(async move {
                let interval = control::PRESENCE_EXPIRY;
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                ticker.tick().await; // skip immediate first tick
                loop {
                    tokio::select! {
                        _ = sweep_cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            if sweep_tx.send(StageCommand::SweepPresence).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Forward incoming control messages to the actor
        let cmd_tx = self.cmd_rx_forwarder();
        tokio::spawn(async move {
            while let Some(msg) = ctrl_rx.recv().await {
                if cmd_tx
                    .send(StageCommand::ControlReceived(msg))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Start the listener audio pipeline (connect to host, receive mixed stream)
        let endpoint = self.endpoint.clone();
        let host_node_id = ticket.host_node_id.clone();
        let host_signing_pubkey = ticket.host_pubkey.clone();
        let app_handle_clone = self.app_handle.clone();
        tokio::spawn(async move {
            start_listener_pipeline(
                endpoint,
                host_node_id,
                host_signing_pubkey,
                app_handle_clone,
                listener_cancel,
            )
            .await;
        });

        log::info!("[stage] joined stage {}", short_id(&stage_id));
        self.emit_state_snapshot();
        Ok(())
    }

    async fn leave_stage(&mut self) -> Result<(), AppError> {
        let stage = self
            .active
            .take()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;

        let stage_id = stage.stage_id.clone();
        stage.cancel.cancel();
        stage.control_plane.shutdown();

        log::info!("[stage] left stage {}", short_id(&stage_id));
        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::Ended { stage_id });
        Ok(())
    }

    async fn end_stage(&mut self) -> Result<(), AppError> {
        {
            let stage = self
                .active
                .as_ref()
                .ok_or_else(|| AppError::Other("not in a stage".into()))?;
            if !matches!(stage.my_role, StageRole::Host) {
                return Err(AppError::Other("only the host can end the stage".into()));
            }
        }

        // Broadcast End before taking the stage so we still have the sender
        self.broadcast_control(|stage_id, pubkey, signing_key| {
            sign_stage_control(
                StageControl::End {
                    stage_id: stage_id.to_string(),
                },
                pubkey,
                signing_key,
                now_millis(),
            )
        })
        .await;

        let stage = self.active.take().unwrap();
        let stage_id = stage.stage_id.clone();
        stage.cancel.cancel();
        stage.control_plane.shutdown();

        // Broadcast ended notification on our user feed so followers know the room closed
        let gs = self.gossip_service.clone();
        let ended_stage_id = stage_id.clone();
        tokio::spawn(async move {
            if let Err(e) = gs.broadcast_stage_ended(ended_stage_id).await {
                log::warn!("[stage] failed to broadcast stage ended: {e}");
            }
        });

        log::info!("[stage] ended stage {}", short_id(&stage_id));
        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::Ended { stage_id });
        Ok(())
    }

    // ---- Participant actions ------------------------------------------------

    async fn raise_hand(&mut self) -> Result<(), AppError> {
        self.require_active("raise hand")?;
        let stage_id = self.active.as_ref().unwrap().stage_id.clone();
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::RaiseHand {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::HandRaised { pubkey: my_pubkey });
        let _ = stage_id; // used in closure above
        Ok(())
    }

    async fn lower_hand(&mut self) -> Result<(), AppError> {
        self.require_active("lower hand")?;
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::LowerHand {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::HandLowered { pubkey: my_pubkey });
        Ok(())
    }

    async fn toggle_self_mute(&mut self) -> Result<bool, AppError> {
        let stage = self
            .active
            .as_mut()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;

        stage.self_muted = !stage.self_muted;
        let muted = stage.self_muted;
        let my_pubkey = stage.my_pubkey.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::SelfMuteToggle {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                    muted,
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::MuteChanged {
                pubkey: my_pubkey,
                self_muted: muted,
                host_muted: false,
            },
        );
        Ok(muted)
    }

    async fn send_reaction(&mut self, emoji: String) -> Result<(), AppError> {
        self.require_active("send reaction")?;
        // Truncate to 16 chars
        let emoji: String = emoji.chars().take(16).collect();
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();
        let emoji_clone = emoji.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::Reaction {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                    emoji: emoji_clone.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::Reaction {
                pubkey: my_pubkey,
                emoji,
            },
        );
        Ok(())
    }

    async fn send_chat(&mut self, text: String) -> Result<(), AppError> {
        self.require_active("send chat")?;
        // Truncate to 500 chars
        let text: String = text.chars().take(500).collect();
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();
        let text_clone = text.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::Chat {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                    text: text_clone.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::Chat {
                pubkey: my_pubkey,
                text,
            },
        );
        Ok(())
    }

    // ---- Host moderation ---------------------------------------------------

    async fn promote_speaker(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "promote")?;

        if let Some(p) = self.active.as_mut().unwrap().participants.get_mut(&pubkey) {
            p.role = StageRole::Speaker;
        }

        let pubkey_for_bcast = pubkey.clone();
        self.broadcast_control(move |sid, pk, sk| {
            sign_stage_control(
                StageControl::PromoteSpeaker {
                    stage_id: sid.to_string(),
                    pubkey: pubkey_for_bcast.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::RoleChanged {
                pubkey,
                role: StageRole::Speaker,
            },
        );
        Ok(())
    }

    async fn demote_speaker(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "demote")?;

        if let Some(p) = self.active.as_mut().unwrap().participants.get_mut(&pubkey) {
            p.role = StageRole::Listener;
        }

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::DemoteSpeaker {
                    stage_id: sid.to_string(),
                    pubkey: pubkey.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::RoleChanged {
                pubkey,
                role: StageRole::Listener,
            },
        );
        Ok(())
    }

    async fn mute_speaker(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "mute speaker")?;

        if let Some(p) = self.active.as_mut().unwrap().participants.get_mut(&pubkey) {
            p.host_muted = true;
        }

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::MuteSpeaker {
                    stage_id: sid.to_string(),
                    pubkey: pubkey.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::MuteChanged {
                pubkey,
                self_muted: false,
                host_muted: true,
            },
        );
        Ok(())
    }

    async fn kick(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "kick")?;

        self.active.as_mut().unwrap().participants.remove(&pubkey);

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::Kick {
                    stage_id: sid.to_string(),
                    pubkey: pubkey.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::ParticipantLeft { pubkey });
        Ok(())
    }

    async fn ban(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "ban")?;

        let stage = self.active.as_mut().unwrap();
        // Record the transport NodeId (if known) for connection-time enforcement.
        if let Some(node_id) = stage
            .participants
            .get(&pubkey)
            .and_then(|p| p.node_id.clone())
        {
            stage.banned_node_ids.insert(node_id);
        }
        stage.participants.remove(&pubkey);

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::Ban {
                    stage_id: sid.to_string(),
                    pubkey: pubkey.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::ParticipantLeft { pubkey });
        Ok(())
    }

    fn sweep_presence(&mut self) {
        let stage = match self.active.as_mut() {
            Some(s) => s,
            None => return,
        };
        let cutoff = now_millis().saturating_sub(control::PRESENCE_EXPIRY.as_millis() as u64);
        let expired: Vec<String> = stage
            .participants
            .values()
            .filter(|p| p.last_seen_ms < cutoff && p.pubkey != stage.my_pubkey)
            .map(|p| p.pubkey.clone())
            .collect();
        for pubkey in expired {
            stage.participants.remove(&pubkey);
            log::debug!("[stage] participant {} timed out", short_id(&pubkey));
            let _ = self
                .app_handle
                .emit("stage-event", StageEvent::ParticipantLeft { pubkey });
        }
    }

    // ---- Incoming control message handler ----------------------------------

    async fn handle_control(&mut self, signed: SignedStageControl) {
        let stage = match self.active.as_mut() {
            Some(s) => s,
            None => return,
        };

        // Signature verification: we need the sender's signing key.
        // For Phase 2, we skip full verification (no cached delegation lookup yet)
        // and just check that the sender_pubkey is consistent with the message.
        // Phase 3 will add full delegation-based verification.
        log::debug!(
            "[stage-ctrl] received {:?} from {}",
            std::mem::discriminant(&signed.control),
            short_id(&signed.sender_pubkey)
        );

        match &signed.control {
            StageControl::Presence {
                pubkey,
                role,
                timestamp,
                node_id,
                ..
            } => {
                let now = now_millis();
                let is_new = !stage.participants.contains_key(pubkey.as_str());
                let entry =
                    stage
                        .participants
                        .entry(pubkey.clone())
                        .or_insert_with(|| Participant {
                            pubkey: pubkey.clone(),
                            role: *role,
                            hand_raised: false,
                            self_muted: false,
                            host_muted: false,
                            last_seen_ms: *timestamp,
                            node_id: node_id.clone(),
                        });
                entry.last_seen_ms = now;
                entry.role = *role;
                if node_id.is_some() {
                    entry.node_id = node_id.clone();
                }
                if is_new {
                    let _ = self.app_handle.emit(
                        "stage-event",
                        StageEvent::ParticipantJoined {
                            pubkey: pubkey.clone(),
                            role: *role,
                        },
                    );
                }
            }
            StageControl::End { .. } => {
                // Only accept End from the host
                if signed.sender_pubkey == stage.host_pubkey {
                    let stage_id = stage.stage_id.clone();
                    stage.cancel.cancel();
                    let cp = self.active.take().unwrap().control_plane;
                    cp.shutdown();
                    log::info!("[stage] room ended by host: {}", short_id(&stage_id));
                    let _ = self
                        .app_handle
                        .emit("stage-event", StageEvent::Ended { stage_id });
                }
            }
            StageControl::RaiseHand { pubkey, .. } => {
                if let Some(p) = stage.participants.get_mut(pubkey.as_str()) {
                    p.hand_raised = true;
                }
                let _ = self.app_handle.emit(
                    "stage-event",
                    StageEvent::HandRaised {
                        pubkey: pubkey.clone(),
                    },
                );
            }
            StageControl::LowerHand { pubkey, .. } => {
                if let Some(p) = stage.participants.get_mut(pubkey.as_str()) {
                    p.hand_raised = false;
                }
                let _ = self.app_handle.emit(
                    "stage-event",
                    StageEvent::HandLowered {
                        pubkey: pubkey.clone(),
                    },
                );
            }
            StageControl::PromoteSpeaker { pubkey, .. } => {
                if is_moderator(stage, &signed.sender_pubkey) {
                    if let Some(p) = stage.participants.get_mut(pubkey.as_str()) {
                        p.role = StageRole::Speaker;
                    }

                    if pubkey == &stage.my_pubkey {
                        // Self-promotion: start the speaker audio pipeline.
                        stage.my_role = StageRole::Speaker;

                        // Cancel the existing listener pipeline.
                        if let Some(old_cancel) = stage.listener_pipeline_cancel.take() {
                            old_cancel.cancel();
                        }

                        // Speaker-side PCM mixer: one actor, one AudioPlayback, N input streams.
                        // The mixer sums all incoming speaker streams on a 20 ms tick.
                        // AEC far-end wiring and reconnect are managed inside start_speaker_pipeline.
                        let pb_cancel = stage.cancel.child_token();
                        let speaker_mixer = spawn_speaker_mixer(pb_cancel);
                        stage.speaker_mixer = Some(speaker_mixer.clone());

                        // Connect to host: send mic, receive N uni-streams (one per other speaker).
                        // Uses stage.host_node_id — never listener_upstream_id (which may be a relay).
                        let endpoint = self.endpoint.clone();
                        let host_node_id = stage.host_node_id.clone();
                        let spk_cancel = stage.cancel.child_token();
                        tokio::spawn(async move {
                            start_speaker_pipeline(
                                endpoint,
                                host_node_id,
                                speaker_mixer,
                                spk_cancel,
                            )
                            .await;
                        });
                    }
                    // No action needed when another speaker is promoted:
                    // the host will automatically open a new uni-stream on our
                    // connection when that speaker connects.

                    let _ = self.app_handle.emit(
                        "stage-event",
                        StageEvent::RoleChanged {
                            pubkey: pubkey.clone(),
                            role: StageRole::Speaker,
                        },
                    );
                }
            }
            StageControl::DemoteSpeaker { pubkey, .. } => {
                if is_moderator(stage, &signed.sender_pubkey) {
                    if let Some(p) = stage.participants.get_mut(pubkey.as_str()) {
                        p.role = StageRole::Listener;
                    }
                    if pubkey == &stage.my_pubkey {
                        stage.my_role = StageRole::Listener;
                    }
                    let _ = self.app_handle.emit(
                        "stage-event",
                        StageEvent::RoleChanged {
                            pubkey: pubkey.clone(),
                            role: StageRole::Listener,
                        },
                    );
                }
            }
            StageControl::MuteSpeaker { pubkey, .. } => {
                if is_moderator(stage, &signed.sender_pubkey) {
                    if let Some(p) = stage.participants.get_mut(pubkey.as_str()) {
                        p.host_muted = true;
                    }
                    let self_muted = stage
                        .participants
                        .get(pubkey.as_str())
                        .map(|p| p.self_muted)
                        .unwrap_or(false);
                    let _ = self.app_handle.emit(
                        "stage-event",
                        StageEvent::MuteChanged {
                            pubkey: pubkey.clone(),
                            self_muted,
                            host_muted: true,
                        },
                    );
                }
            }
            StageControl::SelfMuteToggle { pubkey, muted, .. } => {
                if let Some(p) = stage.participants.get_mut(pubkey.as_str()) {
                    p.self_muted = *muted;
                }
                let host_muted = stage
                    .participants
                    .get(pubkey.as_str())
                    .map(|p| p.host_muted)
                    .unwrap_or(false);
                let _ = self.app_handle.emit(
                    "stage-event",
                    StageEvent::MuteChanged {
                        pubkey: pubkey.clone(),
                        self_muted: *muted,
                        host_muted,
                    },
                );
            }
            StageControl::Kick { pubkey, .. } => {
                if is_moderator(stage, &signed.sender_pubkey) {
                    stage.participants.remove(pubkey.as_str());
                    if pubkey == &stage.my_pubkey {
                        // We were kicked
                        let stage_id = stage.stage_id.clone();
                        stage.cancel.cancel();
                        let cp = self.active.take().unwrap().control_plane;
                        cp.shutdown();
                        log::info!("[stage] kicked from stage {}", short_id(&stage_id));
                        let _ = self.app_handle.emit("stage-event", StageEvent::Kicked);
                    } else {
                        let _ = self.app_handle.emit(
                            "stage-event",
                            StageEvent::ParticipantLeft {
                                pubkey: pubkey.clone(),
                            },
                        );
                    }
                }
            }
            StageControl::Reaction { pubkey, emoji, .. } => {
                let _ = self.app_handle.emit(
                    "stage-event",
                    StageEvent::Reaction {
                        pubkey: pubkey.clone(),
                        emoji: emoji.clone(),
                    },
                );
            }
            StageControl::Chat { pubkey, text, .. } => {
                let _ = self.app_handle.emit(
                    "stage-event",
                    StageEvent::Chat {
                        pubkey: pubkey.clone(),
                        text: text.clone(),
                    },
                );
            }
            StageControl::RelayVolunteer {
                relay_pubkey,
                relay_node_id,
                capacity,
                ..
            } => {
                // Only the host processes volunteer messages and assigns listeners to relays
                if matches!(stage.my_role, StageRole::Host)
                    && let Some(ref mut topology) = stage.topology
                {
                    topology.add_relay(relay_node_id.clone(), *capacity);

                    // Reassign listeners that the topology manager routes to this new relay
                    let listeners: Vec<String> = stage
                        .participants
                        .values()
                        .filter(|p| matches!(p.role, StageRole::Listener))
                        .map(|p| p.pubkey.clone())
                        .collect();

                    let mut relay_listeners: Vec<String> = Vec::new();
                    for pk in &listeners {
                        if let Some(assignment) = topology.assign_listener(pk)
                            && assignment.source_endpoint_id == *relay_node_id
                        {
                            relay_listeners.push(pk.clone());
                        }
                    }

                    if !relay_listeners.is_empty() {
                        let rp = relay_pubkey.clone();
                        let rn = relay_node_id.clone();
                        let sid = stage.stage_id.clone();
                        let listeners_for_task = relay_listeners;
                        let control_plane = stage.control_plane.clone();
                        let identity = self.identity.clone();
                        tokio::spawn(async move {
                            let id = identity.read().await;
                            let signed = sign_stage_control(
                                StageControl::RelayAssignment {
                                    stage_id: sid,
                                    relay_pubkey: rp,
                                    relay_node_id: rn,
                                    listener_pubkeys: listeners_for_task,
                                },
                                &id.signing_key.public().to_string(),
                                &id.signing_key,
                                now_millis(),
                            );
                            drop(id);
                            if let Err(e) = control_plane.broadcast(&signed).await {
                                log::warn!("[stage-host] relay assignment broadcast failed: {e}");
                            }
                        });
                    }
                }
            }
            StageControl::RelayAssignment {
                relay_node_id,
                listener_pubkeys,
                ..
            } => {
                // Only accept relay assignments from the host
                if signed.sender_pubkey == stage.host_pubkey {
                    let my_pubkey = stage.my_pubkey.clone();
                    if listener_pubkeys.contains(&my_pubkey) {
                        // Cancel the current listener pipeline and restart at the new relay
                        if let Some(old_cancel) = stage.listener_pipeline_cancel.take() {
                            old_cancel.cancel();
                        }

                        let new_cancel = stage.cancel.child_token();
                        stage.listener_pipeline_cancel = Some(new_cancel.clone());
                        stage.listener_upstream_id = Some(relay_node_id.clone());

                        let endpoint = self.endpoint.clone();
                        let relay_id = relay_node_id.clone();
                        let host_signing_pubkey = stage.host_pubkey.clone();
                        let app_handle = self.app_handle.clone();
                        tokio::spawn(async move {
                            start_listener_pipeline(
                                endpoint,
                                relay_id,
                                host_signing_pubkey,
                                app_handle,
                                new_cancel,
                            )
                            .await;
                        });
                        log::info!(
                            "[stage-listener] reassigned to relay {}",
                            short_id(relay_node_id)
                        );
                    }
                }
            }
            StageControl::Ban { pubkey, .. } => {
                if signed.sender_pubkey == stage.host_pubkey {
                    if let Some(node_id) = stage
                        .participants
                        .get(pubkey.as_str())
                        .and_then(|p| p.node_id.clone())
                    {
                        stage.banned_node_ids.insert(node_id);
                    }
                    stage.participants.remove(pubkey.as_str());
                    if pubkey == &stage.my_pubkey {
                        let stage_id = stage.stage_id.clone();
                        stage.cancel.cancel();
                        let cp = self.active.take().unwrap().control_plane;
                        cp.shutdown();
                        log::info!("[stage] banned from stage {}", short_id(&stage_id));
                        let _ = self.app_handle.emit("stage-event", StageEvent::Kicked);
                    } else {
                        let _ = self.app_handle.emit(
                            "stage-event",
                            StageEvent::ParticipantLeft {
                                pubkey: pubkey.clone(),
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }

    // ---- Helpers -----------------------------------------------------------

    fn require_active(&self, action: &str) -> Result<(), AppError> {
        if self.active.is_none() {
            return Err(AppError::Other(format!("cannot {action}: not in a stage")));
        }
        Ok(())
    }

    fn require_host_active(&self, _pubkey: &str, action: &str) -> Result<(), AppError> {
        match &self.active {
            None => Err(AppError::Other(format!("cannot {action}: not in a stage"))),
            Some(s) if !matches!(s.my_role, StageRole::Host | StageRole::CoHost) => {
                Err(AppError::Other(format!("cannot {action}: not the host")))
            }
            _ => Ok(()),
        }
    }

    fn build_state(&self) -> Option<StageState> {
        let s = self.active.as_ref()?;
        let participants = s
            .participants
            .values()
            .map(|p| iroh_social_types::StageParticipant {
                pubkey: p.pubkey.clone(),
                role: p.role,
                display_name: None,
                avatar_hash: None,
                hand_raised: p.hand_raised,
                self_muted: p.self_muted,
                host_muted: p.host_muted,
            })
            .collect();

        Some(StageState {
            stage_id: s.stage_id.clone(),
            title: s.title.clone(),
            host_pubkey: s.host_pubkey.clone(),
            my_pubkey: s.my_pubkey.clone(),
            my_role: s.my_role,
            participants,
            started_at: s.started_at,
            ticket: s.ticket.as_ref().map(|t| t.to_string()),
        })
    }

    fn emit_state_snapshot(&self) {
        if let Some(state) = self.build_state() {
            let _ = self
                .app_handle
                .emit("stage-event", StageEvent::StateSnapshot(state));
        }
    }

    /// Helper to build + broadcast a control message using current identity.
    ///
    /// The closure receives `(stage_id, my_pubkey_str, signing_key)` and returns
    /// the `SignedStageControl` to broadcast.
    async fn broadcast_control<F>(&self, f: F)
    where
        F: FnOnce(&str, &str, &iroh::SecretKey) -> SignedStageControl,
    {
        let Some(stage) = self.active.as_ref() else {
            return;
        };
        let id = self.identity.read().await;
        let signing_pubkey = id.signing_key.public().to_string();
        let signed = f(&stage.stage_id, &signing_pubkey, &id.signing_key);
        drop(id);
        if let Err(e) = stage.control_plane.broadcast(&signed).await {
            log::warn!("[stage] failed to broadcast control: {e}");
        }
    }

    /// Return a clone of our own cmd_tx so background tasks can forward messages.
    fn cmd_rx_forwarder(&self) -> mpsc::Sender<StageCommand> {
        self.self_tx.clone()
    }

    async fn volunteer_as_relay(&mut self, capacity: u32) -> Result<(), AppError> {
        let stage = self
            .active
            .as_mut()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;

        let upstream_id = stage
            .listener_upstream_id
            .clone()
            .ok_or_else(|| AppError::Other("no upstream audio source to relay from".into()))?;

        let relay_cancel = stage.cancel.child_token();
        let relay_handle =
            start_relay_pipeline(self.endpoint.clone(), upstream_id, relay_cancel).await?;
        stage.relay_handle = Some(relay_handle);

        let id = self.identity.read().await;
        let relay_node_id = id.transport_node_id.clone();
        let signing_pubkey = id.signing_key.public().to_string();
        let signing_key = id.signing_key.clone();
        drop(id);

        let signed = sign_stage_control(
            StageControl::RelayVolunteer {
                stage_id: stage.stage_id.clone(),
                relay_pubkey: stage.my_pubkey.clone(),
                relay_node_id: relay_node_id.clone(),
                capacity,
            },
            &signing_pubkey,
            &signing_key,
            now_millis(),
        );

        if let Err(e) = stage.control_plane.broadcast(&signed).await {
            log::warn!("[stage-relay] relay volunteer broadcast failed: {e}");
        }

        log::info!(
            "[stage] volunteered as relay (cap={capacity}, node={})",
            short_id(&relay_node_id)
        );
        Ok(())
    }
}

// ---- Free helpers -------------------------------------------------------

/// Returns true if `sender_pubkey` is allowed to issue moderation actions
/// (promote/demote/mute/kick/ban) in this stage.
fn is_moderator(stage: &ActiveStage, sender_pubkey: &str) -> bool {
    sender_pubkey == stage.host_pubkey
        || stage
            .participants
            .get(sender_pubkey)
            .is_some_and(|p| p.role == StageRole::CoHost)
}

// ---- StageHandler (ProtocolHandler) -------------------------------------

/// Protocol handler that accepts STAGE_ALPN connections and routes them
/// to the StageActor.
#[derive(Clone)]
pub struct StageHandler {
    handle: StageActorHandle,
}

impl StageHandler {
    pub fn new(
        endpoint: Endpoint,
        gossip: Gossip,
        identity: SharedIdentity,
        storage: Arc<Storage>,
        gossip_service: GossipService,
        app_handle: AppHandle,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let actor = StageActor::new(
            cmd_rx,
            cmd_tx.clone(),
            endpoint,
            gossip,
            identity,
            storage,
            gossip_service,
            app_handle,
        );
        tokio::spawn(actor.run());
        Self {
            handle: StageActorHandle { cmd_tx },
        }
    }

    pub fn actor_handle(&self) -> StageActorHandle {
        self.handle.clone()
    }
}

impl std::fmt::Debug for StageHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageHandler").finish_non_exhaustive()
    }
}

impl ProtocolHandler for StageHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id().to_string();
        log::info!("[stage] incoming connection from {}", short_id(&remote));
        self.handle.incoming_connection(conn).await;
        Ok(())
    }
}

// ---- Audio pipeline free functions --------------------------------------

/// Handle an incoming STAGE_ALPN connection based on our current role.
///
/// Called from a spawned task so it can do async I/O without blocking the actor.
#[allow(clippy::too_many_arguments)]
async fn handle_incoming_connection(
    conn: Connection,
    my_role: StageRole,
    fanout: Option<Arc<Fanout>>,
    mixer_handle: Option<MixerHandle>,
    relay_handle: Option<relay::RelayHandle>,
    sfu_hub: Option<Arc<tokio::sync::Mutex<SfuHub>>>,
    host_sfu_fanout: Option<Arc<Fanout>>,
    banned_node_ids: std::collections::HashSet<String>,
    cancel: CancellationToken,
) {
    let remote = conn.remote_id().to_string();

    if banned_node_ids.contains(&remote) {
        log::info!(
            "[stage] rejected connection from banned node {}",
            short_id(&remote)
        );
        conn.close(1u32.into(), b"banned");
        return;
    }

    // If we are acting as a relay, accept downstream listener connections.
    if let Some(relay_handle) = relay_handle {
        let (send, recv) =
            match tokio::time::timeout(std::time::Duration::from_secs(5), conn.accept_bi()).await {
                Ok(Ok(pair)) => pair,
                Ok(Err(e)) => {
                    log::warn!(
                        "[stage-relay] failed to accept bi-stream from {}: {e}",
                        short_id(&remote)
                    );
                    return;
                }
                Err(_) => {
                    log::warn!(
                        "[stage-relay] timeout accepting bi-stream from {}",
                        short_id(&remote)
                    );
                    return;
                }
            };

        let mut type_buf = [0u8; 1];
        let mut recv = recv;
        if tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv.read_exact(&mut type_buf),
        )
        .await
        .is_err()
        {
            log::warn!(
                "[stage-relay] timeout reading conn type from {}",
                short_id(&remote)
            );
            return;
        }
        drop(recv);

        match type_buf[0] {
            CONN_TYPE_LISTENER => {
                log::info!(
                    "[stage-relay] downstream listener connected: {}",
                    short_id(&remote)
                );
                if let Err(e) = relay_handle.add_downstream(send).await {
                    log::warn!("[stage-relay] failed to add downstream listener: {e}");
                }
            }
            unknown => {
                log::warn!(
                    "[stage-relay] unexpected conn type {unknown:#x} from {}",
                    short_id(&remote)
                );
                conn.close(0u32.into(), b"relay only accepts listeners");
            }
        }
        return;
    }

    match my_role {
        StageRole::Host => {
            let (mut send, mut recv) =
                match tokio::time::timeout(std::time::Duration::from_secs(5), conn.accept_bi())
                    .await
                {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => {
                        log::warn!(
                            "[stage] failed to accept bi-stream from {}: {e}",
                            short_id(&remote)
                        );
                        return;
                    }
                    Err(_) => {
                        log::warn!(
                            "[stage] timeout accepting bi-stream from {}",
                            short_id(&remote)
                        );
                        return;
                    }
                };

            let mut type_buf = [0u8; 1];
            if tokio::time::timeout(
                std::time::Duration::from_secs(5),
                recv.read_exact(&mut type_buf),
            )
            .await
            .is_err()
            {
                log::warn!(
                    "[stage] timeout reading conn type from {}",
                    short_id(&remote)
                );
                return;
            }

            match type_buf[0] {
                CONN_TYPE_SPEAKER => {
                    log::info!("[stage-host] speaker connected: {}", short_id(&remote));
                    let _ = send.finish(); // no return bi-stream; host uses SFU uni-streams

                    if let (Some(mixer), Some(hub)) = (mixer_handle, sfu_hub) {
                        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<f32>>(32);
                        if mixer.add_speaker(remote.clone(), pcm_rx).await.is_err() {
                            return;
                        }

                        // Create this speaker's SFU fanout.
                        let sfu_fanout = Arc::new(Fanout::new());

                        // Atomically register in hub; snapshot existing peers.
                        let (snap_fanouts, snap_conns) = {
                            let mut locked = hub.lock().await;
                            let sf = locked.fanouts.clone();
                            let sc = locked.connections.clone();
                            locked.fanouts.insert(remote.clone(), sfu_fanout.clone());
                            locked.connections.insert(remote.clone(), conn.clone());
                            (sf, sc)
                        };

                        // Subscribe new speaker to each existing speaker's audio,
                        // and subscribe each existing speaker to the new speaker's audio.
                        for (e_id, e_fanout) in &snap_fanouts {
                            if let Ok(uni) = conn.open_uni().await {
                                e_fanout.add_subscriber(uni, &cancel);
                            }
                            if let Some(e_conn) = snap_conns.get(e_id)
                                && let Ok(uni) = e_conn.open_uni().await
                            {
                                sfu_fanout.add_subscriber(uni, &cancel);
                            }
                        }

                        // Subscribe new speaker to host's voice.
                        if let Some(hf) = host_sfu_fanout
                            && let Ok(uni) = conn.open_uni().await
                        {
                            hf.add_subscriber(uni, &cancel);
                        }

                        tokio::spawn(speaker_recv_sfu_loop(
                            recv, remote, pcm_tx, sfu_fanout, hub, cancel,
                        ));
                    }
                }
                CONN_TYPE_LISTENER => {
                    log::info!("[stage-host] listener connected: {}", short_id(&remote));
                    if let Some(f) = fanout {
                        f.add_subscriber(send, &cancel);
                    }
                }
                unknown => {
                    log::warn!(
                        "[stage] unknown conn type {unknown:#x} from {}",
                        short_id(&remote)
                    );
                    conn.close(0u32.into(), b"unknown connection type");
                }
            }
        }
        _ => {
            log::debug!(
                "[stage] rejecting incoming from {} (role {:?})",
                short_id(&remote),
                my_role
            );
            conn.close(0u32.into(), b"not accepting connections in this role");
        }
    }
}

/// Receive Opus frames from a speaker's QUIC stream.
/// Tees each raw frame to the speaker's SFU fanout (for forwarding to other speakers)
/// and decodes PCM to the mixer channel.
/// Removes the speaker from the SFU hub when the stream closes.
async fn speaker_recv_sfu_loop(
    mut recv: iroh::endpoint::RecvStream,
    node_id: String,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    sfu_fanout: Arc<Fanout>,
    sfu_hub: Arc<tokio::sync::Mutex<SfuHub>>,
    cancel: CancellationToken,
) {
    let mut decoder = match OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!(
                "[stage-host] failed to create decoder for {}: {e}",
                short_id(&node_id)
            );
            return;
        }
    };

    let mut frames: u32 = 0;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = read_audio_frame(&mut recv) => {
                match result {
                    Ok(Some((seq, ts, tag, payload))) => {
                        // Decode first (borrows payload), then forward raw bytes.
                        let decoded = decoder.decode(&payload);
                        sfu_fanout.send_frame(seq, ts, tag, payload);
                        match decoded {
                            Ok(samples) => {
                                if pcm_tx.send(samples).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "[stage-host] decode error from {}: {e}",
                                    short_id(&node_id)
                                );
                            }
                        }
                        frames += 1;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!(
                            "[stage-host] recv error from {}: {e}",
                            short_id(&node_id)
                        );
                        break;
                    }
                }
            }
        }
    }

    // Clean up SFU hub entry so future speakers don't try to subscribe to a dead fanout.
    {
        let mut hub = sfu_hub.lock().await;
        hub.fanouts.remove(&node_id);
        hub.connections.remove(&node_id);
    }
    log::debug!(
        "[stage-host] speaker {} disconnected ({frames} frames), removed from SFU hub",
        short_id(&node_id)
    );
}

/// Connect to an upstream node (host or relay) as a listener and start relaying the
/// mixed audio stream to downstream subscribers via a `RelayActor`.
///
/// Returns the `RelayHandle` used to register downstream listener connections.
async fn start_relay_pipeline(
    endpoint: Endpoint,
    upstream_node_id: String,
    cancel: CancellationToken,
) -> Result<relay::RelayHandle, AppError> {
    let upstream_id: EndpointId = upstream_node_id
        .parse()
        .map_err(|e| AppError::Other(format!("invalid relay upstream node id: {e}")))?;

    let conn = endpoint
        .connect(EndpointAddr::from(upstream_id), STAGE_ALPN)
        .await
        .map_err(|e| AppError::Other(format!("[stage-relay] connect to upstream failed: {e}")))?;

    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| AppError::Other(format!("[stage-relay] open bi-stream failed: {e}")))?;

    // Identify ourselves as a listener to the upstream; drop send after (host doesn't use it further)
    send.write_all(&[CONN_TYPE_LISTENER])
        .await
        .map_err(|_| AppError::Other("[stage-relay] write conn type failed".into()))?;

    log::info!(
        "[stage-relay] connected to upstream {}, spawning relay actor",
        short_id(&upstream_node_id)
    );

    relay::spawn_relay(recv, cancel)
}

/// Receive mix-minus PCM from the mixer and play it locally (host only).
///
/// The mixer sends only the contributions of other speakers — the host's own
/// voice is excluded, eliminating echo without a round-trip through the network.
async fn run_host_playback(
    mut rx: mpsc::Receiver<Vec<f32>>,
    far_end_tx: mpsc::Sender<Vec<f32>>,
    cancel: CancellationToken,
) {
    let (mut prod, _playback) = match AudioPlayback::start() {
        Ok(p) => p,
        Err(e) => {
            log::error!("[stage-host] failed to start playback: {e}");
            return;
        }
    };

    let mut frames: u32 = 0;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            pcm = rx.recv() => {
                let Some(samples) = pcm else { break };
                let _ = far_end_tx.try_send(samples.clone());
                prod.push(&samples);
                frames += 1;
            }
        }
    }
    log::info!("[stage-host] host playback stopped ({frames} frames)");
}

/// Connect to an upstream node (host or relay), receive the mixed stream, verify auth,
/// decode, and play back. Automatically reconnects with exponential backoff on stream failure.
async fn start_listener_pipeline(
    endpoint: Endpoint,
    upstream_node_id: String,
    host_signing_pubkey: String,
    app_handle: AppHandle,
    cancel: CancellationToken,
) {
    const BACKOFF_INITIAL: std::time::Duration = std::time::Duration::from_secs(2);
    const BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(30);
    let mut backoff = BACKOFF_INITIAL;

    loop {
        run_listener_once(
            &endpoint,
            &upstream_node_id,
            &host_signing_pubkey,
            &app_handle,
            &cancel,
        )
        .await;

        if cancel.is_cancelled() {
            break;
        }

        log::info!(
            "[stage-listener] stream ended, reconnecting in {:?}",
            backoff
        );
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Single connection attempt for the listener audio pipeline.
async fn run_listener_once(
    endpoint: &Endpoint,
    host_node_id: &str,
    host_signing_pubkey: &str,
    app_handle: &AppHandle,
    cancel: &CancellationToken,
) {
    let host_id: EndpointId = match host_node_id.parse() {
        Ok(id) => id,
        Err(e) => {
            log::error!("[stage-listener] invalid host node id: {e}");
            return;
        }
    };

    let conn = match endpoint
        .connect(EndpointAddr::from(host_id), STAGE_ALPN)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("[stage-listener] failed to connect to host: {e}");
            return;
        }
    };

    let source_id = conn.remote_id().to_string();

    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(pair) => pair,
        Err(e) => {
            log::error!("[stage-listener] failed to open bi-stream: {e}");
            return;
        }
    };

    // Identify ourselves as a listener
    if send.write_all(&[CONN_TYPE_LISTENER]).await.is_err() {
        return;
    }

    log::info!("[stage-listener] connected to host, starting playback");

    // Build listener auth state from host's signing public key
    let host_pubkey = match host_signing_pubkey.parse::<iroh::PublicKey>() {
        Ok(pk) => pk,
        Err(e) => {
            log::error!("[stage-listener] invalid host pubkey: {e}");
            return;
        }
    };
    let mut auth = auth::ListenerAuthState::new(host_pubkey);

    let mut decoder = match OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[stage-listener] failed to create decoder: {e}");
            return;
        }
    };

    // Adaptive jitter buffer.
    //
    // AudioPlayback::start() owns ring buffer creation and the cpal stream.
    // PlaybackProducer is the sole write path; the cpal callback is the sole
    // reader. Capacity is PLAYBACK_CAPACITY_FRAMES (defined in playback.rs).
    //
    // Adaptation rules (applied once per decoded frame while playing):
    //   - Underrun detected  -> target += 1 (up to JB_MAX_FRAMES)
    //   - DRIFT_INTERVAL consecutive frames with no underrun -> target -= 1 (down to JB_MIN_FRAMES)
    //
    // The target governs the pre-fill gate: playback (re-)starts only once
    // target_frames decoded frames are buffered, so the callback never fires
    // into an empty buffer. Depth converges toward the minimum that avoids
    // underruns on the current network path.
    const JB_MIN_FRAMES: usize = 3; // 60 ms — LAN / low-latency WAN
    const JB_INIT_FRAMES: usize = 4; // 80 ms — conservative start
    const JB_MAX_FRAMES: usize = 10; // 200 ms — ceiling for bad WAN
    const DRIFT_INTERVAL: usize = 250; // ~5 s at 20 ms/frame before drifting down

    // cpal starts immediately (outputs silence until pre-fill completes).
    let (mut prod, playback) = match AudioPlayback::start() {
        Ok(p) => p,
        Err(e) => {
            log::error!("[stage-listener] failed to start playback: {e}");
            return;
        }
    };

    let mut playback_live = false;
    let mut prefill_count: usize = 0;

    let mut target_frames: usize = JB_INIT_FRAMES;
    let mut frames_since_adapt: usize = 0;

    // Sequence tracking for packet loss concealment.
    let mut last_seq: Option<u32> = None;

    let mut frames: u32 = 0;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = read_audio_frame(&mut recv) => {
                match result {
                    Ok(Some((seq, _, tag, wire_payload))) => {
                        // Fill any detected gaps with PLC frames before processing this frame.
                        if let Some(prev) = last_seq {
                            let lost = seq.wrapping_sub(prev.wrapping_add(1));
                            if lost > 0 && lost < 64 {
                                log::warn!("[stage-listener] {lost} lost frame(s) before seq {seq}, inserting PLC");
                                for _ in 0..lost {
                                    if let Ok(plc) = decoder.decode_loss() {
                                        prod.push(&plc);
                                    }
                                }
                            }
                        }
                        last_seq = Some(seq);

                        // Split checkpoint prefix from Opus payload.
                        let (checkpoint, opus) =
                            if tag == crate::audio::transport::TAG_CHECKPOINT {
                                match auth::decode_checkpoint_payload(&wire_payload) {
                                    Ok((hash, sig, opus)) => (Some((hash, sig)), opus),
                                    Err(e) => {
                                        log::warn!("[stage-listener] malformed checkpoint: {e}");
                                        continue;
                                    }
                                }
                            } else {
                                (None, wire_payload.as_slice())
                            };

                        // Verify auth chain. TamperDetected and InvalidSignature
                        // are fatal — the auth state is permanently compromised
                        // and we must disconnect.
                        let auth_result = auth.verify_frame(opus, tag, checkpoint);
                        match &auth_result {
                            auth::AuthResult::TamperDetected => {
                                log::warn!("[stage-listener] tamper detected from {}, disconnecting", short_id(&source_id));
                                let _ = app_handle.emit(
                                    "stage-event",
                                    StageEvent::AuthFailed {
                                        source: source_id.clone(),
                                        reason: "tamper_detected".to_string(),
                                    },
                                );
                                break;
                            }
                            auth::AuthResult::InvalidSignature => {
                                log::warn!("[stage-listener] invalid signature from {}, disconnecting", short_id(&source_id));
                                let _ = app_handle.emit(
                                    "stage-event",
                                    StageEvent::AuthFailed {
                                        source: source_id.clone(),
                                        reason: "invalid_signature".to_string(),
                                    },
                                );
                                break;
                            }
                            _ => {}
                        }

                        // Decode and push into jitter buffer.
                        if let Ok(samples) = decoder.decode(opus) {
                            let pushed = prod.push(&samples);
                            if pushed < samples.len() {
                                log::debug!(
                                    "[stage-listener] jitter buffer full, dropped {} samples",
                                    samples.len() - pushed
                                );
                            }
                            frames += 1;

                            if playback_live {
                                // Adaptive depth: check for underruns each frame.
                                let underruns = prod.drain_underruns();
                                if underruns > 0 {
                                    if target_frames < JB_MAX_FRAMES {
                                        target_frames += 1;
                                        log::debug!(
                                            "[stage-listener] underrun x{underruns}, target -> {}f ({}ms)",
                                            target_frames,
                                            target_frames * 20
                                        );
                                    }
                                    frames_since_adapt = 0;
                                } else {
                                    frames_since_adapt += 1;
                                    if frames_since_adapt >= DRIFT_INTERVAL
                                        && target_frames > JB_MIN_FRAMES
                                    {
                                        target_frames -= 1;
                                        frames_since_adapt = 0;
                                        log::debug!(
                                            "[stage-listener] drifting down, target -> {}f ({}ms)",
                                            target_frames,
                                            target_frames * 20
                                        );
                                    }
                                }
                            } else {
                                // Pre-fill gate: begin adaptive tracking once target depth is buffered.
                                prefill_count += 1;
                                if prefill_count >= target_frames {
                                    log::info!(
                                        "[stage-listener] pre-fill reached ({}f / {}ms), audio live",
                                        target_frames,
                                        target_frames * 20
                                    );
                                    playback_live = true;
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        log::info!("[stage-listener] host stream ended");
                        break;
                    }
                    Err(e) => {
                        log::warn!("[stage-listener] recv error: {e}");
                        break;
                    }
                }
            }
        }
    }

    drop(playback);
    conn.close(0u32.into(), b"listener left");
    log::info!("[stage-listener] playback stopped ({frames} frames decoded)");
}

/// Receive a single forwarded speaker stream from the host (via a QUIC uni-stream),
/// decode Opus to PCM, and push into the speaker mixer for this stream's slot.
/// Deregisters the stream from the mixer on exit.
async fn speaker_stream_recv(
    mut recv: iroh::endpoint::RecvStream,
    stream_id: u32,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    mixer: SpeakerMixerHandle,
    cancel: CancellationToken,
) {
    let mut decoder = match OpusDecoder::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[stage-speaker] failed to create stream decoder: {e}");
            mixer.remove_stream(stream_id).await;
            return;
        }
    };

    let mut frames: u32 = 0;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = read_audio_frame(&mut recv) => {
                match result {
                    Ok(Some((_, _, _, payload))) => {
                        if let Ok(samples) = decoder.decode(&payload) {
                            if pcm_tx.send(samples).await.is_err() {
                                break;
                            }
                            frames += 1;
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }

    mixer.remove_stream(stream_id).await;
    log::debug!("[stage-speaker] stream recv stopped ({frames} frames)");
}

/// Encode mic PCM samples from the AEC output and send to host via a QUIC send stream.
async fn speaker_mic_send(
    mut send: iroh::endpoint::SendStream,
    mut mic_rx: mpsc::Receiver<Vec<f32>>,
    cancel: CancellationToken,
) {
    let mut encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("[stage-speaker] failed to create mic encoder: {e}");
            return;
        }
    };

    let mut seq: u32 = 0;
    let mut timestamp: u32 = 0;
    let mut frames: u32 = 0;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            samples = mic_rx.recv() => {
                let Some(s) = samples else { break };
                for packet in encoder.push_samples(&s) {
                    if write_audio_frame(&mut send, seq, timestamp, TAG_NORMAL, &packet)
                        .await
                        .is_err()
                    {
                        log::warn!("[stage-speaker] mic send failed, host may have disconnected");
                        cancel.cancel();
                        break;
                    }
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(SAMPLES_PER_FRAME as u32);
                    frames += 1;
                }
            }
        }
    }

    let _ = send.finish();
    log::info!("[stage-speaker] mic send stopped ({frames} frames)");
}

/// Connect to the host as a speaker (SFU model) with automatic reconnect.
///
/// Each attempt creates its own AEC + capture session (so reconnect gets a fresh mic
/// pipeline). The `SpeakerMixerHandle` (AudioPlayback) persists across reconnects.
/// On each attempt the AEC far-end channel is re-wired via `set_far_end_tx`.
async fn start_speaker_pipeline(
    endpoint: Endpoint,
    host_node_id: String,
    speaker_mixer: SpeakerMixerHandle,
    cancel: CancellationToken,
) {
    const BACKOFF_INITIAL: std::time::Duration = std::time::Duration::from_secs(2);
    const BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(30);
    let mut backoff = BACKOFF_INITIAL;

    let host_id: EndpointId = match host_node_id.parse() {
        Ok(id) => id,
        Err(e) => {
            log::error!("[stage-speaker] invalid host node id: {e}");
            return;
        }
    };

    loop {
        run_speaker_once(&endpoint, host_id, &speaker_mixer, cancel.child_token()).await;

        if cancel.is_cancelled() {
            break;
        }

        log::info!(
            "[stage-speaker] disconnected, reconnecting in {}s",
            backoff.as_secs()
        );
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }

    log::info!("[stage-speaker] pipeline stopped");
}

/// One speaker connection attempt: set up AEC + capture, connect to host, run until
/// the connection drops or `session_cancel` fires.
async fn run_speaker_once(
    endpoint: &Endpoint,
    host_id: EndpointId,
    speaker_mixer: &SpeakerMixerHandle,
    session_cancel: CancellationToken,
) {
    // Fresh AEC far-end channel for this session.
    let (far_end_tx, mut far_end_rx) = mpsc::channel::<Vec<f32>>(20);
    speaker_mixer.set_far_end_tx(far_end_tx).await;

    // AEC + capture on a dedicated std thread (VoipAec3 is !Send).
    let (cap_tx, mut cap_raw_rx) = mpsc::channel::<Vec<f32>>(32);
    let (fwd_tx, fwd_rx) = mpsc::channel::<Vec<f32>>(32);
    std::thread::spawn(move || {
        let _capture = match AudioCapture::start(cap_tx) {
            Ok(c) => c,
            Err(e) => {
                log::error!("[stage-speaker] mic capture failed: {e}");
                return;
            }
        };
        let mut aec = match EchoCanceller::new() {
            Ok(a) => a,
            Err(e) => {
                log::error!("[stage-speaker] failed to create AEC: {e}");
                return;
            }
        };
        while let Some(s) = cap_raw_rx.blocking_recv() {
            while let Ok(r) = far_end_rx.try_recv() {
                aec.render(&r);
            }
            let cleaned = aec.process_capture(&s);
            if !cleaned.is_empty() && fwd_tx.blocking_send(cleaned).is_err() {
                break;
            }
        }
    });

    let conn = match endpoint
        .connect(EndpointAddr::from(host_id), STAGE_ALPN)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[stage-speaker] failed to connect to host: {e}");
            return;
        }
    };

    let (mut send, _recv) = match conn.open_bi().await {
        Ok(pair) => pair,
        Err(e) => {
            log::warn!("[stage-speaker] failed to open bi-stream: {e}");
            return;
        }
    };

    if send.write_all(&[CONN_TYPE_SPEAKER]).await.is_err() {
        return;
    }

    log::info!("[stage-speaker] connected to host");

    tokio::spawn(speaker_mic_send(send, fwd_rx, session_cancel.child_token()));

    let mut next_stream_id: u32 = 0;
    loop {
        tokio::select! {
            _ = session_cancel.cancelled() => break,
            result = conn.accept_uni() => {
                match result {
                    Ok(uni_recv) => {
                        let stream_id = next_stream_id;
                        next_stream_id = next_stream_id.wrapping_add(1);
                        let (stream_tx, stream_rx) = mpsc::channel::<Vec<f32>>(32);
                        speaker_mixer.add_stream(stream_id, stream_rx).await;
                        tokio::spawn(speaker_stream_recv(
                            uni_recv,
                            stream_id,
                            stream_tx,
                            speaker_mixer.clone(),
                            session_cancel.child_token(),
                        ));
                    }
                    Err(e) => {
                        log::warn!("[stage-speaker] host connection lost: {e}");
                        break;
                    }
                }
            }
        }
    }

    conn.close(0u32.into(), b"speaker left");
}
