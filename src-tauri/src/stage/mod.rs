pub mod auth;
mod command;
pub mod control;
mod control_handler;
pub mod fanout;
mod lifecycle;
pub mod mixer;
mod moderation;
mod pipeline;
mod relay;
mod serving;
mod speaker_mixer;
mod state;
pub mod topology;

use command::ActorSender;
pub use command::StageActorHandle;
pub(crate) use command::StageCommand;

use crate::error::AppError;
use crate::gossip::GossipService;
use crate::state::SharedIdentity;
use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh_gossip::Gossip;
use proscenium_types::{StageEvent, StageParticipant, StageState, short_id};
use state::{ActiveStage, Moderation};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

/// Connection type byte: sent as first byte of the bi-stream by the initiating side.
pub(super) const CONN_TYPE_SPEAKER: u8 = 0x01;
pub(super) const CONN_TYPE_LISTENER: u8 = 0x02;

/// Application close codes of STAGE_ALPN connections.
pub(super) const CLOSE_NORMAL: u32 = 0;
/// The identity named in the hello is banned from the stage.
pub(super) const CLOSE_BANNED: u32 = 1;
/// The serving node has no free connection slot.
pub(super) const CLOSE_FULL: u32 = 2;
/// The hello is invalid, or the node does not accept this connection type or role.
pub(super) const CLOSE_REJECTED: u32 = 3;
/// A newer connection from the same node or identity replaced this one.
pub(super) const CLOSE_REPLACED: u32 = 4;

/// What the actor does with the active stage after a control message or command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Continue,
    /// The host ended the room.
    Ended,
    /// The local user was kicked or banned.
    Kicked,
}

// ---- StageActor ---------------------------------------------------------

pub(super) struct StageActor {
    cmd_rx: mpsc::Receiver<StageCommand>,
    /// Sender to ourselves, handed to background tasks.
    self_tx: ActorSender,
    pub(super) endpoint: Endpoint,
    pub(super) gossip: Gossip,
    pub(super) identity: SharedIdentity,
    pub(super) gossip_service: GossipService,
    pub(super) app_handle: AppHandle,
    pub(super) active: Option<ActiveStage>,
    /// Source of connection ids and mixer input ids. Never reused, so a late
    /// `*ConnClosed` command cannot match a newer connection.
    next_conn_id: u64,
}

impl StageActor {
    fn new(
        cmd_rx: mpsc::Receiver<StageCommand>,
        self_tx: ActorSender,
        endpoint: Endpoint,
        gossip: Gossip,
        identity: SharedIdentity,
        gossip_service: GossipService,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            cmd_rx,
            self_tx,
            endpoint,
            gossip,
            identity,
            gossip_service,
            app_handle,
            active: None,
            next_conn_id: 0,
        }
    }

    async fn run(mut self) {
        while let Some(cmd) = self.cmd_rx.recv().await {
            self.handle(cmd).await;
        }
        // Every handle was dropped (the network stack was replaced).
        if let Some(stage) = self.active.take() {
            stage.shutdown();
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
            StageCommand::IncomingConnection(conn) => self.accept_connection(conn),
            StageCommand::AudioConnection(incoming) => {
                let conn_id = self.next_conn_id();
                match self.active.as_mut() {
                    Some(stage) => stage.admit(incoming, conn_id).await,
                    None => incoming
                        .conn
                        .close(CLOSE_REJECTED.into(), b"not in a stage"),
                }
            }
            StageCommand::ListenerConnClosed { node_id, conn_id } => {
                if let Some(stage) = self.active.as_mut() {
                    stage.listener_conn_closed(&node_id, conn_id);
                }
            }
            StageCommand::SpeakerConnClosed { node_id, conn_id } => {
                if let Some(stage) = self.active.as_mut() {
                    stage.speaker_conn_closed(&node_id, conn_id);
                }
            }
            StageCommand::UpstreamFailed { stage_id, node_id } => {
                if let Some(stage) = self.active.as_mut()
                    && stage.stage_id == stage_id
                {
                    stage.upstream_failed(node_id);
                }
            }
            StageCommand::ControlReceived(msg) => {
                let outcome = match self.active.as_mut() {
                    Some(stage) => stage.handle_control(msg).await,
                    None => Outcome::Continue,
                };
                self.finish(outcome);
            }
            StageCommand::SweepPresence => {
                if let Some(stage) = self.active.as_mut() {
                    stage.sweep_presence().await;
                }
            }
            StageCommand::HostTick => {
                if let Some(stage) = self.active.as_mut() {
                    stage.host_tick().await;
                }
            }
            StageCommand::PromoteSpeaker { pubkey, reply } => {
                let _ = reply.send(self.moderate(Moderation::Promote, pubkey).await);
            }
            StageCommand::DemoteSpeaker { pubkey, reply } => {
                let _ = reply.send(self.moderate(Moderation::Demote, pubkey).await);
            }
            StageCommand::MuteSpeaker { pubkey, reply } => {
                let _ = reply.send(self.moderate(Moderation::Mute, pubkey).await);
            }
            StageCommand::Kick { pubkey, reply } => {
                let _ = reply.send(self.moderate(Moderation::Kick, pubkey).await);
            }
            StageCommand::Ban { pubkey, reply } => {
                let _ = reply.send(self.moderate(Moderation::Ban, pubkey).await);
            }
            StageCommand::VolunteerAsRelay { capacity, reply } => {
                let result = match self.active.as_mut() {
                    Some(stage) => stage.volunteer_as_relay(capacity).await,
                    None => Err(not_in_stage()),
                };
                let _ = reply.send(result);
            }
            StageCommand::RaiseHand { reply } => {
                let result = match self.active.as_mut() {
                    Some(stage) => stage.raise_hand().await,
                    None => Err(not_in_stage()),
                };
                let _ = reply.send(result);
            }
            StageCommand::LowerHand { reply } => {
                let result = match self.active.as_mut() {
                    Some(stage) => stage.lower_hand().await,
                    None => Err(not_in_stage()),
                };
                let _ = reply.send(result);
            }
            StageCommand::ToggleSelfMute { reply } => {
                let result = match self.active.as_mut() {
                    Some(stage) => stage.toggle_self_mute().await,
                    None => Err(not_in_stage()),
                };
                let _ = reply.send(result);
            }
            StageCommand::SendReaction { emoji, reply } => {
                let result = match self.active.as_mut() {
                    Some(stage) => stage.send_reaction(emoji).await,
                    None => Err(not_in_stage()),
                };
                let _ = reply.send(result);
            }
            StageCommand::SendChat { text, reply } => {
                let result = match self.active.as_mut() {
                    Some(stage) => stage.send_chat(text).await,
                    None => Err(not_in_stage()),
                };
                let _ = reply.send(result);
            }
            StageCommand::GetState { reply } => {
                let _ = reply.send(self.active.as_ref().map(ActiveStage::build_state));
            }
            StageCommand::GetActiveAnnouncement { reply } => {
                let _ = reply.send(
                    self.active
                        .as_ref()
                        .and_then(ActiveStage::build_announcement),
                );
            }
        }
    }

    // ---- Helpers -----------------------------------------------------------

    pub(super) fn next_conn_id(&mut self) -> u64 {
        self.next_conn_id += 1;
        self.next_conn_id
    }

    /// Return a sender to our own command channel for background tasks.
    pub(super) fn cmd_tx(&self) -> ActorSender {
        self.self_tx.clone()
    }

    /// Stop the active stage and emit `event` to the frontend.
    pub(super) fn close_stage(&mut self, event: StageEvent) {
        if let Some(stage) = self.active.take() {
            stage.shutdown();
            let _ = self.app_handle.emit("stage-event", event);
        }
    }

    /// Close the active stage if `outcome` requires it.
    fn finish(&mut self, outcome: Outcome) {
        let Some(stage) = self.active.as_ref() else {
            return;
        };
        let stage_id = stage.stage_id.clone();
        match outcome {
            Outcome::Continue => {}
            Outcome::Ended => {
                log::info!("[stage] room ended by host: {}", short_id(&stage_id));
                self.close_stage(StageEvent::Ended { stage_id });
            }
            Outcome::Kicked => {
                log::info!("[stage] removed from stage {}", short_id(&stage_id));
                self.close_stage(StageEvent::Kicked);
            }
        }
    }

    /// Run a moderation command of the local user.
    async fn moderate(&mut self, action: Moderation, pubkey: String) -> Result<(), AppError> {
        let stage = self.active.as_mut().ok_or_else(not_in_stage)?;
        stage.moderate(action, pubkey).await
    }

    /// Start reading the hello of an incoming connection, or close the connection if
    /// this node serves no audio.
    fn accept_connection(&self, conn: Connection) {
        let remote = conn.remote_id().to_string();
        let Some(stage) = self.active.as_ref().filter(|s| s.serving.is_some()) else {
            log::debug!(
                "[stage] closing connection from {} (not serving audio)",
                short_id(&remote)
            );
            conn.close(CLOSE_REJECTED.into(), b"not accepting connections");
            return;
        };
        let stage_id = stage.stage_id.clone();
        let cancel = stage.cancel.child_token();
        let cmd_tx = self.cmd_tx();
        tokio::spawn(async move {
            let closer = conn.clone();
            let accepted = tokio::select! {
                _ = cancel.cancelled() => return,
                accepted = pipeline::accept_audio_connection(conn, &stage_id) => accepted,
            };
            match accepted {
                Ok(incoming) => {
                    cmd_tx.send(StageCommand::AudioConnection(incoming)).await;
                }
                Err(reason) => {
                    log::info!(
                        "[stage] rejected connection from {}: {reason}",
                        short_id(&remote)
                    );
                    closer.close(CLOSE_REJECTED.into(), b"invalid hello");
                }
            }
        });
    }
}

fn not_in_stage() -> AppError {
    AppError::Other("not in a stage".into())
}

impl ActiveStage {
    pub(super) fn emit(&self, event: StageEvent) {
        let _ = self.rt.app_handle.emit("stage-event", event);
    }

    pub(super) fn emit_snapshot(&self) {
        self.emit(StageEvent::StateSnapshot(self.build_state()));
    }

    /// Stop every task of this session and close its audio connections.
    pub(super) fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(server) = self.serving.as_mut() {
            server.shutdown(b"stage ended");
        }
        if let Some(host) = self.host.as_mut() {
            for (_, speaker) in host.speaker_conns.drain() {
                speaker.conn.close(CLOSE_NORMAL.into(), b"stage ended");
            }
        }
        self.control_plane.shutdown();
    }

    /// Build the state the frontend displays. Roles and host mutes come from the
    /// host-assigned sets; the entry of the local user carries the local flags.
    pub(super) fn build_state(&self) -> StageState {
        let self_muted = self.self_muted.load(Ordering::Relaxed);
        let participants = self
            .participants
            .values()
            .map(|p| {
                let is_me = p.pubkey == self.my_pubkey;
                StageParticipant {
                    pubkey: p.pubkey.clone(),
                    role: if is_me {
                        self.my_role
                    } else {
                        self.roles.role_of(&p.pubkey)
                    },
                    display_name: None,
                    avatar_hash: None,
                    hand_raised: if is_me {
                        self.hand_raised
                    } else {
                        p.hand_raised
                    },
                    self_muted: if is_me { self_muted } else { p.self_muted },
                    host_muted: self.roles.is_host_muted(&p.pubkey),
                }
            })
            .collect();

        StageState {
            stage_id: self.stage_id.clone(),
            title: self.title.clone(),
            host_pubkey: self.roles.host_pubkey.clone(),
            my_pubkey: self.my_pubkey.clone(),
            my_role: self.my_role,
            participants,
            started_at: self.started_at,
            ticket: self.host.as_ref().map(|h| h.ticket.to_string()),
            self_muted,
            hand_raised: self.hand_raised,
        }
    }

    /// Build the signed announcement of this stage, if this node is the host.
    fn build_announcement(&self) -> Option<proscenium_types::StageAnnouncement> {
        let host = self.host.as_ref()?;
        let signature = proscenium_types::sign_stage_announcement(
            &self.stage_id,
            &self.title,
            &host.ticket,
            &self.roles.host_pubkey,
            self.started_at,
            self.signer.master_key(),
        );
        Some(proscenium_types::StageAnnouncement {
            stage_id: self.stage_id.clone(),
            title: self.title.clone(),
            ticket: host.ticket.clone(),
            host_pubkey: self.roles.host_pubkey.clone(),
            started_at: self.started_at,
            signature,
        })
    }
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
        gossip_service: GossipService,
        app_handle: AppHandle,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let actor = StageActor::new(
            cmd_rx,
            ActorSender::new(&cmd_tx),
            endpoint,
            gossip,
            identity,
            gossip_service,
            app_handle,
        );
        tokio::spawn(actor.run());
        Self {
            handle: StageActorHandle::new(cmd_tx),
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
        log::debug!("[stage] incoming connection from {}", short_id(&remote));
        self.handle.incoming_connection(conn).await;
        Ok(())
    }
}
