pub mod auth;
mod command;
pub mod control;
mod control_handler;
pub mod fanout;
mod lifecycle;
pub mod mixer;
mod moderation;
mod pipeline;
pub mod relay;
mod speaker_mixer;
mod state;
pub mod topology;

pub use command::StageActorHandle;
pub(crate) use command::StageCommand;

use crate::error::AppError;
use crate::gossip::GossipService;
use crate::state::SharedIdentity;
use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh_gossip::Gossip;
use proscenium_types::{SignedStageControl, StageEvent, StageRole, StageState, short_id};
use state::ActiveStage;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

/// Connection type byte: sent as first byte of the bi-stream by the initiating side.
pub(super) const CONN_TYPE_SPEAKER: u8 = 0x01;
pub(super) const CONN_TYPE_LISTENER: u8 = 0x02;

// ---- StageActor ---------------------------------------------------------

pub(super) struct StageActor {
    cmd_rx: mpsc::Receiver<StageCommand>,
    /// Cloned sender to ourselves, used to inject ControlReceived messages.
    self_tx: mpsc::Sender<StageCommand>,
    pub(super) endpoint: Endpoint,
    gossip: Gossip,
    pub(super) identity: SharedIdentity,
    pub(super) gossip_service: GossipService,
    pub(super) app_handle: AppHandle,
    pub(super) active: Option<ActiveStage>,
}

impl StageActor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        cmd_rx: mpsc::Receiver<StageCommand>,
        self_tx: mpsc::Sender<StageCommand>,
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
                        pipeline::handle_incoming_connection(
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
            StageCommand::GetActiveAnnouncement { reply } => {
                let _ = reply.send(self.build_active_announcement());
            }
        }
    }

    // ---- Helpers -----------------------------------------------------------

    pub(super) fn require_active(&self, action: &str) -> Result<(), AppError> {
        if self.active.is_none() {
            return Err(AppError::Other(format!("cannot {action}: not in a stage")));
        }
        Ok(())
    }

    pub(super) fn require_host_active(&self, _pubkey: &str, action: &str) -> Result<(), AppError> {
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
            .map(|p| proscenium_types::StageParticipant {
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

    /// Build a stage announcement from the active stage, if this node is the host.
    fn build_active_announcement(&self) -> Option<proscenium_types::StageAnnouncement> {
        let s = self.active.as_ref()?;
        // Only include the announcement if we are the host
        if s.my_role != StageRole::Host {
            return None;
        }
        let ticket = s.ticket.as_ref()?;
        Some(proscenium_types::StageAnnouncement {
            stage_id: s.stage_id.clone(),
            title: s.title.clone(),
            ticket: ticket.clone(),
            host_pubkey: s.host_pubkey.clone(),
            started_at: s.started_at,
        })
    }

    pub(super) fn emit_state_snapshot(&self) {
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
    pub(super) async fn broadcast_control<F>(&self, f: F)
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
    pub(super) fn cmd_rx_forwarder(&self) -> mpsc::Sender<StageCommand> {
        self.self_tx.clone()
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
            cmd_tx.clone(),
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
        log::info!("[stage] incoming connection from {}", short_id(&remote));
        self.handle.incoming_connection(conn).await;
        Ok(())
    }
}
