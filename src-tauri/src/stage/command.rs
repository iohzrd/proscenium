use crate::error::AppError;
use iroh::endpoint::Connection;
use proscenium_types::{SignedStageControl, StageState, StageTicket};
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
    /// Query the active stage announcement (if hosting). Used by sync handler.
    GetActiveAnnouncement {
        reply: oneshot::Sender<Option<proscenium_types::StageAnnouncement>>,
    },
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

    /// Query the active stage announcement if this node is currently hosting a stage.
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

    pub(crate) async fn incoming_connection(&self, conn: Connection) {
        let _ = self
            .cmd_tx
            .send(StageCommand::IncomingConnection(conn))
            .await;
    }
}
