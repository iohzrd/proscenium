use crate::error::AppError;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{EndpointId, SecretKey};
use iroh_gossip::{Event, Gossip, GossipReceiver, GossipSender};
use proscenium_types::{
    SignedStageControl, StageControl, VerifiedPresence, is_recent_timestamp, now_millis, short_id,
    sign_stage_control, stage_control_topic, validate_stage_control, verify_presence,
    verify_stage_control,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

/// How often a participant broadcasts a Presence heartbeat.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// How long without a heartbeat before a participant is considered gone.
pub const PRESENCE_EXPIRY: std::time::Duration = std::time::Duration::from_secs(45);

/// Minimum time between two Presence broadcasts caused by new gossip neighbors.
const PRESENCE_MIN_SPACING: std::time::Duration = std::time::Duration::from_secs(1);

/// Signs this node's control messages for one stage with the master key.
pub struct ControlSigner {
    stage_id: String,
    master_key: SecretKey,
    node_id: String,
    /// Timestamp of the last signed message. Receivers require timestamps of one signer
    /// to increase, so two messages never carry the same value.
    last_timestamp: AtomicU64,
}

impl ControlSigner {
    pub fn new(stage_id: String, master_key: SecretKey, node_id: String) -> Self {
        Self {
            stage_id,
            master_key,
            node_id,
            last_timestamp: AtomicU64::new(0),
        }
    }

    /// Next message timestamp: the clock in ms, or the last timestamp plus one if the
    /// clock did not advance.
    fn next_timestamp(&self, now_ms: u64) -> u64 {
        let next = |last: u64| now_ms.max(last.saturating_add(1));
        let last = self
            .last_timestamp
            .update(Ordering::Relaxed, Ordering::Relaxed, next);
        next(last)
    }

    /// Sign a control message for this stage.
    pub fn sign(&self, control: StageControl) -> SignedStageControl {
        let timestamp = self.next_timestamp(now_millis());
        sign_stage_control(&self.stage_id, control, &self.master_key, timestamp)
    }

    /// Sign a `Presence` that names this node's NodeId.
    pub fn presence(&self) -> SignedStageControl {
        self.sign(StageControl::Presence {
            node_id: self.node_id.clone(),
        })
    }

    pub fn master_key(&self) -> &SecretKey {
        &self.master_key
    }
}

/// A control message whose signature, size limits and timestamp window were checked.
pub struct VerifiedControl {
    pub signed: SignedStageControl,
    /// Set for a verified `Presence`.
    pub presence: Option<VerifiedPresence>,
}

/// Check a received control message: stage id, payload limits, signature and timestamp
/// window.
pub fn verify_control(
    signed: SignedStageControl,
    stage_id: &str,
) -> Result<VerifiedControl, String> {
    if signed.stage_id != stage_id {
        return Err("message for another stage".to_string());
    }
    validate_stage_control(&signed.control)?;
    if !is_recent_timestamp(signed.timestamp) {
        return Err("timestamp outside the accepted window".to_string());
    }
    let presence = if matches!(signed.control, StageControl::Presence { .. }) {
        Some(verify_presence(&signed)?)
    } else {
        verify_stage_control(&signed)?;
        None
    };
    Ok(VerifiedControl { signed, presence })
}

/// Check the hello of an audio connection: a signed `Presence` for `stage_id`. The
/// timestamp is not checked: the caller requires the named NodeId to equal the NodeId
/// QUIC authenticated, so a copy sent by another node is rejected, and a node with a
/// wrong clock can still connect.
pub fn verify_hello(
    signed: &SignedStageControl,
    stage_id: &str,
) -> Result<VerifiedPresence, String> {
    if signed.stage_id != stage_id {
        return Err("hello for another stage".to_string());
    }
    verify_presence(signed)
}

/// Live gossip control plane for one Stage room.
///
/// Owns a `GossipSender` for the stage topic and tracks the background tasks.
#[derive(Clone)]
pub struct ControlPlane {
    sender: GossipSender,
    signer: Arc<ControlSigner>,
    cancel: CancellationToken,
}

impl ControlPlane {
    /// Subscribe to the stage gossip topic and start receive + heartbeat tasks.
    ///
    /// `initial_peers` are NodeIds of nodes in the room (e.g. host's NodeId
    /// extracted from the ticket on join). For the host starting a new room this
    /// is empty.
    pub async fn start(
        gossip: &Gossip,
        stage_id: &str,
        initial_peers: Vec<EndpointId>,
        signer: Arc<ControlSigner>,
        // Deliver verified incoming control messages back to the StageActor.
        ctrl_tx: mpsc::Sender<VerifiedControl>,
        cancel: CancellationToken,
    ) -> Result<Self, AppError> {
        let topic = stage_control_topic(stage_id);
        let topic_handle = gossip.subscribe(topic, initial_peers).await?;
        let (sender, receiver) = topic_handle.split();

        // The receive task signals the heartbeat task when a gossip neighbor connects.
        let neighbor_up = Arc::new(Notify::new());

        // Spawn receive task
        let recv_cancel = cancel.child_token();
        let sid = stage_id.to_string();
        let recv_neighbor_up = neighbor_up.clone();
        tokio::spawn(async move {
            receive_loop(receiver, ctrl_tx, recv_neighbor_up, recv_cancel, &sid).await;
        });

        let plane = Self {
            sender,
            signer,
            cancel: cancel.clone(),
        };

        // Spawn heartbeat task.
        let heartbeat = plane.clone();
        let hb_cancel = cancel.child_token();
        tokio::spawn(async move {
            heartbeat_loop(heartbeat, neighbor_up, hb_cancel).await;
        });

        Ok(plane)
    }

    /// Sign a control message and broadcast it to all room participants.
    pub async fn broadcast(&self, control: StageControl) -> Result<(), AppError> {
        let signed = self.signer.sign(control);
        self.broadcast_signed(&signed).await
    }

    async fn broadcast_signed(&self, signed: &SignedStageControl) -> Result<(), AppError> {
        let payload = serde_json::to_vec(signed)?;
        if payload.len() >= crate::constants::GOSSIP_MAX_MESSAGE_SIZE {
            return Err(AppError::Other(format!(
                "control message too large: {} bytes",
                payload.len()
            )));
        }
        self.sender.broadcast(Bytes::from(payload)).await?;
        Ok(())
    }

    /// Shut down the control plane (cancels heartbeat + receive tasks).
    pub fn shutdown(self) {
        self.cancel.cancel();
    }
}

/// Background task: receive gossip events and forward verified control messages.
async fn receive_loop(
    mut receiver: GossipReceiver,
    ctrl_tx: mpsc::Sender<VerifiedControl>,
    neighbor_up: Arc<Notify>,
    cancel: CancellationToken,
    stage_id: &str,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            event = receiver.try_next() => {
                match event {
                    Ok(Some(Event::Received(msg))) => {
                        let signed = match serde_json::from_slice::<SignedStageControl>(&msg.content) {
                            Ok(signed) => signed,
                            Err(e) => {
                                log::debug!("[stage-ctrl] failed to parse control message: {e}");
                                continue;
                            }
                        };
                        match verify_control(signed, stage_id) {
                            Ok(verified) => {
                                if ctrl_tx.send(verified).await.is_err() {
                                    break; // actor dropped
                                }
                            }
                            Err(reason) => {
                                log::debug!("[stage-ctrl] discarded control message: {reason}");
                            }
                        }
                    }
                    Ok(Some(Event::NeighborUp(id))) => {
                        log::debug!(
                            "[stage-ctrl] neighbor joined: {}",
                            short_id(&id.to_string())
                        );
                        neighbor_up.notify_one();
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

/// Background task: broadcast a Presence heartbeat at once, every `HEARTBEAT_INTERVAL`,
/// and when a gossip neighbor connects. A Presence sent before the first neighbor
/// connected reaches nobody, so the neighbor event repeats it.
async fn heartbeat_loop(plane: ControlPlane, neighbor_up: Arc<Notify>, cancel: CancellationToken) {
    let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = interval.tick() => {}
            _ = neighbor_up.notified() => {}
        }
        let presence = plane.signer.presence();
        if let Err(e) = plane.broadcast_signed(&presence).await {
            log::warn!("[stage-ctrl] heartbeat send failed: {e}");
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(PRESENCE_MIN_SPACING) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE_ID: &str = "000102030405060708090a0b0c0d0e0f";

    fn signer(seed: u8) -> ControlSigner {
        let node_id = SecretKey::from_bytes(&[seed.wrapping_add(100); 32])
            .public()
            .to_string();
        ControlSigner::new(
            STAGE_ID.to_string(),
            SecretKey::from_bytes(&[seed; 32]),
            node_id,
        )
    }

    #[test]
    fn signer_timestamps_strictly_increase() {
        let signer = signer(1);
        assert_eq!(signer.next_timestamp(1_000), 1_000);
        // The clock did not advance, or went backwards.
        assert_eq!(signer.next_timestamp(1_000), 1_001);
        assert_eq!(signer.next_timestamp(900), 1_002);
        assert_eq!(signer.next_timestamp(5_000), 5_000);

        let a = signer.sign(StageControl::RaiseHand);
        let b = signer.sign(StageControl::LowerHand);
        assert!(b.timestamp > a.timestamp);
    }

    #[test]
    fn verify_control_accepts_own_presence_and_rejects_other_stage() {
        let signer = signer(2);
        let verified = verify_control(signer.presence(), STAGE_ID).unwrap();
        let presence = verified.presence.unwrap();
        assert_eq!(
            presence.master_pubkey,
            signer.master_key().public().to_string()
        );
        assert_eq!(presence.node_id, signer.node_id);

        assert!(verify_control(signer.presence(), "ff0102030405060708090a0b0c0d0e0f").is_err());
    }

    #[test]
    fn verify_hello_requires_stage_and_presence_but_no_recent_timestamp() {
        let signer = signer(4);
        let hello = signer.presence();
        assert_eq!(
            verify_hello(&hello, STAGE_ID).unwrap().master_pubkey,
            signer.master_key().public().to_string()
        );
        assert!(verify_hello(&hello, "ff0102030405060708090a0b0c0d0e0f").is_err());
        assert!(verify_hello(&signer.sign(StageControl::RaiseHand), STAGE_ID).is_err());

        let old = sign_stage_control(
            STAGE_ID,
            StageControl::Presence {
                node_id: signer.node_id.clone(),
            },
            signer.master_key(),
            1,
        );
        assert!(verify_hello(&old, STAGE_ID).is_ok());
        assert!(verify_control(old, STAGE_ID).is_err());
    }

    #[test]
    fn verify_control_rejects_stale_timestamp_and_oversized_payload() {
        let signer = signer(3);
        let stale = sign_stage_control(
            STAGE_ID,
            StageControl::RaiseHand,
            signer.master_key(),
            now_millis() - 2 * proscenium_types::MAX_TIMESTAMP_DRIFT_MS,
        );
        assert!(verify_control(stale, STAGE_ID).is_err());

        let oversized = signer.sign(StageControl::Chat {
            text: "a".repeat(proscenium_types::MAX_STAGE_CHAT_CHARS + 1),
        });
        assert!(verify_control(oversized, STAGE_ID).is_err());

        let mut tampered = signer.sign(StageControl::Chat {
            text: "hello".to_string(),
        });
        tampered.control = StageControl::Chat {
            text: "other".to_string(),
        };
        assert!(verify_control(tampered, STAGE_ID).is_err());

        assert!(verify_control(signer.sign(StageControl::RaiseHand), STAGE_ID).is_ok());
    }
}
