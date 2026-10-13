use crate::error::AppError;
use crate::stage::control::VerifiedControl;
use crate::stage::pipeline::IncomingAudio;
use iroh::endpoint::Connection;
use proscenium_types::{StageState, StageTicket};
use tokio::sync::{mpsc, oneshot};

// ---- Command enum -------------------------------------------------------

pub(crate) enum StageCommand {
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

    // Incoming audio connection whose hello was verified
    AudioConnection(IncomingAudio),

    // The task serving a listener connection ended
    ListenerConnClosed {
        node_id: String,
        conn_id: u64,
    },

    // The receive loop of a speaker connection ended (host only)
    SpeakerConnClosed {
        node_id: String,
        conn_id: u64,
    },

    // The listener pipeline stopped using a relay and returned to the host
    UpstreamFailed {
        stage_id: String,
        node_id: String,
    },

    // Verified control message from the gossip receive task
    ControlReceived(VerifiedControl),

    // Periodic presence expiry sweep (sent by a background timer task)
    SweepPresence,

    // One-second timer of the host: broadcasts a pending or periodic RoomState
    HostTick,

    // Moderation (host or co-host)
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
    // Host only
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
    /// Query the signed announcement of the hosted stage. Used by sync handler.
    GetActiveAnnouncement {
        reply: oneshot::Sender<Option<proscenium_types::StageAnnouncement>>,
    },
}

// ---- Sender for background tasks ----------------------------------------

/// Command sender handed to the background tasks of a stage. It holds a weak reference,
/// so the actor's channel closes once every `StageActorHandle` is dropped.
#[derive(Clone)]
pub(crate) struct ActorSender(mpsc::WeakSender<StageCommand>);

impl ActorSender {
    pub(crate) fn new(cmd_tx: &mpsc::Sender<StageCommand>) -> Self {
        Self(cmd_tx.downgrade())
    }

    /// Send a command to the actor. Returns false if the actor is gone.
    pub(crate) async fn send(&self, cmd: StageCommand) -> bool {
        match self.0.upgrade() {
            Some(cmd_tx) => cmd_tx.send(cmd).await.is_ok(),
            None => false,
        }
    }
}

// ---- Actor handle -------------------------------------------------------

/// Cheap-to-clone handle to the StageActor command channel.
#[derive(Clone)]
pub struct StageActorHandle {
    cmd_tx: mpsc::Sender<StageCommand>,
}

impl StageActorHandle {
    pub(crate) fn new(cmd_tx: mpsc::Sender<StageCommand>) -> Self {
        Self { cmd_tx }
    }

    /// Send a command built from a reply sender and wait for the actor's reply.
    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, AppError>>) -> StageCommand,
    ) -> Result<T, AppError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(build(tx))
            .await
            .map_err(|_| AppError::Other("stage actor closed".into()))?;
        rx.await
            .map_err(|_| AppError::Other("stage actor dropped reply".into()))?
    }

    pub async fn create_stage(&self, title: String) -> Result<StageTicket, AppError> {
        self.request(|reply| StageCommand::CreateStage { title, reply })
            .await
    }

    pub async fn join_stage(&self, ticket: StageTicket) -> Result<(), AppError> {
        self.request(|reply| StageCommand::JoinStage { ticket, reply })
            .await
    }

    pub async fn leave_stage(&self) -> Result<(), AppError> {
        self.request(|reply| StageCommand::LeaveStage { reply })
            .await
    }

    pub async fn end_stage(&self) -> Result<(), AppError> {
        self.request(|reply| StageCommand::EndStage { reply }).await
    }

    pub async fn promote_speaker(&self, pubkey: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::PromoteSpeaker { pubkey, reply })
            .await
    }

    pub async fn demote_speaker(&self, pubkey: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::DemoteSpeaker { pubkey, reply })
            .await
    }

    pub async fn mute_speaker(&self, pubkey: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::MuteSpeaker { pubkey, reply })
            .await
    }

    pub async fn kick(&self, pubkey: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::Kick { pubkey, reply })
            .await
    }

    pub async fn ban(&self, pubkey: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::Ban { pubkey, reply })
            .await
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

    /// Query the signed announcement of the stage this node is currently hosting.
    pub async fn get_active_announcement(&self) -> Option<proscenium_types::StageAnnouncement> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(StageCommand::GetActiveAnnouncement { reply: tx })
            .await
            .is_err()
        {
            return None;
        }
        rx.await.unwrap_or(None)
    }

    pub async fn toggle_self_mute(&self) -> Result<bool, AppError> {
        self.request(|reply| StageCommand::ToggleSelfMute { reply })
            .await
    }

    pub async fn raise_hand(&self) -> Result<(), AppError> {
        self.request(|reply| StageCommand::RaiseHand { reply })
            .await
    }

    pub async fn lower_hand(&self) -> Result<(), AppError> {
        self.request(|reply| StageCommand::LowerHand { reply })
            .await
    }

    pub async fn send_reaction(&self, emoji: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::SendReaction { emoji, reply })
            .await
    }

    pub async fn send_chat(&self, text: String) -> Result<(), AppError> {
        self.request(|reply| StageCommand::SendChat { text, reply })
            .await
    }

    pub async fn volunteer_as_relay(&self, capacity: u32) -> Result<(), AppError> {
        self.request(|reply| StageCommand::VolunteerAsRelay { capacity, reply })
            .await
    }

    pub(crate) async fn incoming_connection(&self, conn: Connection) {
        let _ = self
            .cmd_tx
            .send(StageCommand::IncomingConnection(conn))
            .await;
    }
}
