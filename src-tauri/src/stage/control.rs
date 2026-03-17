use crate::error::AppError;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::SecretKey;
use iroh::{EndpointId, PublicKey};
use iroh_gossip::{
    Gossip,
    api::{Event, GossipReceiver, GossipSender},
};
use iroh_social_types::{
    SignedStageControl, StageControl, StageRole, now_millis, short_id, sign_stage_control,
    stage_control_topic, verify_stage_control,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How often a participant broadcasts a Presence heartbeat.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// How long without a heartbeat before a participant is considered gone.
#[allow(dead_code)]
pub const PRESENCE_EXPIRY: std::time::Duration = std::time::Duration::from_secs(45);

/// Live gossip control plane for one Stage room.
///
/// Owns a `GossipSender` for the stage topic and tracks the background tasks.
#[derive(Clone)]
pub struct ControlPlane {
    sender: GossipSender,
    cancel: CancellationToken,
}

impl ControlPlane {
    /// Subscribe to the stage gossip topic and start receive + heartbeat tasks.
    ///
    /// `initial_peers` are NodeIds we know are in the room (e.g. host's NodeId
    /// extracted from the ticket on join). For the host starting a new room this
    /// is empty.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        gossip: &Gossip,
        stage_id: &str,
        initial_peers: Vec<EndpointId>,
        my_pubkey: String,
        my_role: StageRole,
        signing_key: SecretKey,
        // Deliver parsed incoming control messages back to the StageActor.
        ctrl_tx: mpsc::Sender<SignedStageControl>,
        cancel: CancellationToken,
    ) -> Result<Self, AppError> {
        let topic = stage_control_topic(stage_id);
        let topic_handle = gossip.subscribe(topic, initial_peers).await?;
        let (sender, receiver) = topic_handle.split();

        // Spawn receive task
        let recv_cancel = cancel.child_token();
        let sid = stage_id.to_string();
        tokio::spawn(async move {
            receive_loop(receiver, ctrl_tx, recv_cancel, &sid).await;
        });

        // Spawn heartbeat task
        let hb_cancel = cancel.child_token();
        let hb_sender = sender.clone();
        let hb_stage_id = stage_id.to_string();
        let hb_pubkey = my_pubkey.clone();
        let hb_role = my_role;
        let hb_key = signing_key.clone();
        tokio::spawn(async move {
            heartbeat_loop(
                hb_sender,
                hb_stage_id,
                hb_pubkey,
                hb_role,
                hb_key,
                hb_cancel,
            )
            .await;
        });

        // Broadcast an immediate Presence on join so peers know we're here.
        let signing_pubkey = signing_key.public().to_string();
        let presence = sign_stage_control(
            StageControl::Presence {
                stage_id: stage_id.to_string(),
                pubkey: my_pubkey,
                role: my_role,
                timestamp: now_millis(),
            },
            &signing_pubkey,
            &signing_key,
            now_millis(),
        );
        let _ = sender
            .broadcast(Bytes::from(serde_json::to_vec(&presence).unwrap()))
            .await;

        Ok(Self { sender, cancel })
    }

    /// Broadcast a signed control message to all room participants.
    pub async fn broadcast(&self, signed: &SignedStageControl) -> Result<(), AppError> {
        let payload = serde_json::to_vec(signed)?;
        self.sender.broadcast(Bytes::from(payload)).await?;
        Ok(())
    }

    /// Shut down the control plane (cancels heartbeat + receive tasks).
    pub fn shutdown(self) {
        self.cancel.cancel();
    }
}

/// Background task: receive gossip events and forward parsed control messages.
async fn receive_loop(
    mut receiver: GossipReceiver,
    ctrl_tx: mpsc::Sender<SignedStageControl>,
    cancel: CancellationToken,
    stage_id: &str,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            event = receiver.try_next() => {
                match event {
                    Ok(Some(Event::Received(msg))) => {
                        match serde_json::from_slice::<SignedStageControl>(&msg.content) {
                            Ok(signed) => {
                                // Verify signature before accepting. The sender claims
                                // to be `sender_pubkey`; we verify the signature with
                                // that same key, proving only the holder of the private
                                // key could have produced this message.
                                let sender_pubkey = match signed.sender_pubkey.parse::<PublicKey>() {
                                    Ok(pk) => pk,
                                    Err(_) => {
                                        log::warn!("[stage-ctrl] invalid sender pubkey, dropping message");
                                        continue;
                                    }
                                };
                                if let Err(e) = verify_stage_control(&signed, &sender_pubkey) {
                                    log::warn!("[stage-ctrl] signature verification failed: {e}");
                                    continue;
                                }
                                if stage_id_matches(&signed.control, stage_id)
                                    && ctrl_tx.send(signed).await.is_err()
                                {
                                    break; // actor dropped
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    "[stage-ctrl] failed to parse control message: {e}"
                                );
                            }
                        }
                    }
                    Ok(Some(Event::NeighborUp(id))) => {
                        log::debug!(
                            "[stage-ctrl] neighbor joined: {}",
                            short_id(&id.to_string())
                        );
                    }
                    Ok(Some(Event::NeighborDown(id))) => {
                        log::debug!(
                            "[stage-ctrl] neighbor left: {}",
                            short_id(&id.to_string())
                        );
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        log::warn!("[stage-ctrl] gossip stream ended for {}", short_id(stage_id));
                        break;
                    }
                    Err(e) => {
                        log::error!("[stage-ctrl] gossip event error: {e}");
                        break;
                    }
                }
            }
        }
    }
}

/// Background task: broadcast a Presence heartbeat every 15 seconds.
async fn heartbeat_loop(
    sender: GossipSender,
    stage_id: String,
    pubkey: String,
    role: StageRole,
    signing_key: SecretKey,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Skip the first (immediate) tick — we already sent Presence on subscribe.
    interval.tick().await;

    let signing_pubkey = signing_key.public().to_string();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = interval.tick() => {
                let ts = now_millis();
                let signed = sign_stage_control(
                    StageControl::Presence {
                        stage_id: stage_id.clone(),
                        pubkey: pubkey.clone(),
                        role,
                        timestamp: ts,
                    },
                    &signing_pubkey,
                    &signing_key,
                    ts,
                );
                let Ok(payload) = serde_json::to_vec(&signed) else { continue };
                if let Err(e) = sender.broadcast(Bytes::from(payload)).await {
                    log::warn!("[stage-ctrl] heartbeat send failed: {e}");
                }
            }
        }
    }
}

/// Check whether a StageControl message belongs to the given stage.
fn stage_id_matches(control: &StageControl, stage_id: &str) -> bool {
    let msg_stage_id = match control {
        StageControl::Announce { stage_id, .. } => stage_id.as_str(),
        StageControl::End { stage_id } => stage_id.as_str(),
        StageControl::Presence { stage_id, .. } => stage_id.as_str(),
        StageControl::RelayVolunteer { stage_id, .. } => stage_id.as_str(),
        StageControl::RelayAssignment { stage_id, .. } => stage_id.as_str(),
        StageControl::RaiseHand { stage_id, .. } => stage_id.as_str(),
        StageControl::LowerHand { stage_id, .. } => stage_id.as_str(),
        StageControl::PromoteSpeaker { stage_id, .. } => stage_id.as_str(),
        StageControl::DemoteSpeaker { stage_id, .. } => stage_id.as_str(),
        StageControl::MuteSpeaker { stage_id, .. } => stage_id.as_str(),
        StageControl::SelfMuteToggle { stage_id, .. } => stage_id.as_str(),
        StageControl::Kick { stage_id, .. } => stage_id.as_str(),
        StageControl::Ban { stage_id, .. } => stage_id.as_str(),
        StageControl::Reaction { stage_id, .. } => stage_id.as_str(),
        StageControl::Chat { stage_id, .. } => stage_id.as_str(),
    };
    msg_stage_id == stage_id
}
