use crate::signing::{hex_to_signature, signature_to_hex};
use iroh::{PublicKey, SecretKey};
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// QUIC ALPN for the Stage audio transport protocol.
pub const STAGE_ALPN: &[u8] = b"iroh-social/stage/1";

/// Derive the iroh-gossip TopicId for a Stage room's control plane.
pub fn stage_control_topic(stage_id: &str) -> TopicId {
    let mut hasher = Sha256::new();
    hasher.update(b"iroh-social-stage-v1:");
    hasher.update(stage_id.as_bytes());
    TopicId::from_bytes(hasher.finalize().into())
}

/// A participant's role in a Stage room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageRole {
    /// Room creator; mixes all speaker audio and distributes host-mixed stream.
    Host,
    /// Elevated listener who can promote/demote speakers and kick listeners.
    CoHost,
    /// Active audio sender; has an open QUIC stream to the host.
    Speaker,
    /// Receive-only; gets host-mixed audio via the relay tree.
    Listener,
}

/// Control-plane messages exchanged over the iroh-gossip Stage topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StageControl {
    /// Host announces a new Stage room.
    Announce {
        stage_id: String,
        title: String,
        host_pubkey: String,
        started_at: u64,
    },
    /// Host signals the room has ended.
    End { stage_id: String },
    /// Participant heartbeat: proves presence and current role (15s interval).
    Presence {
        stage_id: String,
        pubkey: String,
        role: StageRole,
        timestamp: u64,
        /// Transport NodeId of this participant (used by speakers to establish
        /// direct mesh connections to peer speakers).
        node_id: Option<String>,
    },
    /// A listener volunteers to become a relay node.
    RelayVolunteer {
        stage_id: String,
        relay_pubkey: String,
        /// Transport NodeId of the volunteering relay (needed for listeners to connect).
        relay_node_id: String,
        capacity: u32,
    },
    /// Host assigns a set of listeners to a relay.
    RelayAssignment {
        stage_id: String,
        relay_pubkey: String,
        /// Transport NodeId of the relay (needed for listeners to connect).
        relay_node_id: String,
        listener_pubkeys: Vec<String>,
    },
    /// Participant raises their hand to request speaker slot.
    RaiseHand { stage_id: String, pubkey: String },
    /// Participant lowers their hand.
    LowerHand { stage_id: String, pubkey: String },
    /// Host promotes a listener to speaker.
    PromoteSpeaker {
        stage_id: String,
        pubkey: String,
        /// Transport NodeId of the promoted speaker. Included by the host so
        /// existing speakers can open direct mesh connections immediately.
        speaker_node_id: Option<String>,
    },
    /// Host demotes a speaker to listener.
    DemoteSpeaker { stage_id: String, pubkey: String },
    /// Host mutes a speaker on the mix (speaker can still hear themselves locally).
    MuteSpeaker { stage_id: String, pubkey: String },
    /// Speaker toggles their own microphone.
    SelfMuteToggle {
        stage_id: String,
        pubkey: String,
        muted: bool,
    },
    /// Host kicks a participant from the room.
    Kick { stage_id: String, pubkey: String },
    /// Host bans a participant permanently from the room.
    Ban { stage_id: String, pubkey: String },
    /// Text reaction (emoji or short string, max 16 chars).
    Reaction {
        stage_id: String,
        pubkey: String,
        emoji: String,
    },
    /// In-room text chat message (max 500 chars).
    Chat {
        stage_id: String,
        pubkey: String,
        text: String,
    },
}

/// A signed StageControl message for authenticated gossip broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedStageControl {
    /// The master pubkey of the sender (permanent identity).
    pub sender_pubkey: String,
    /// The control message payload.
    pub control: StageControl,
    /// Unix timestamp milliseconds when this was signed.
    pub timestamp: u64,
    /// Ed25519 signature (hex) from the sender's signing key over canonical bytes.
    pub signature: String,
}

/// Canonical bytes for signing a StageControl message.
fn stage_control_signing_bytes(
    sender_pubkey: &str,
    control: &StageControl,
    timestamp: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "sender_pubkey": sender_pubkey,
        "control": control,
        "timestamp": timestamp,
    }))
    .expect("json serialization should not fail")
}

/// Sign a StageControl message with the sender's signing key.
pub fn sign_stage_control(
    control: StageControl,
    sender_pubkey: &str,
    signing_key: &SecretKey,
    timestamp: u64,
) -> SignedStageControl {
    let bytes = stage_control_signing_bytes(sender_pubkey, &control, timestamp);
    let sig = signing_key.sign(&bytes);
    SignedStageControl {
        sender_pubkey: sender_pubkey.to_string(),
        control,
        timestamp,
        signature: signature_to_hex(&sig),
    }
}

/// Verify a signed StageControl message against the sender's signing key.
///
/// The `signer_pubkey` is the *signing* key (from a delegation), not necessarily
/// the same as `signed.sender_pubkey` (which is the master pubkey).
pub fn verify_stage_control(
    signed: &SignedStageControl,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(&signed.signature)?;
    let bytes =
        stage_control_signing_bytes(&signed.sender_pubkey, &signed.control, signed.timestamp);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "stage control signature verification failed".to_string())
}

/// Shareable invite ticket for joining a Stage room.
/// Encoded as hex(JSON) for Display/FromStr (Phase 1; switch to base64url in Phase 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageTicket {
    pub stage_id: String,
    /// Host's *signing* key public key (not master pubkey). Used by listeners
    /// to verify audio stream checkpoint signatures produced by the mixer.
    pub host_pubkey: String,
    /// Transport NodeId of the host (for direct QUIC connection).
    pub host_node_id: String,
    pub title: String,
}

impl fmt::Display for StageTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_vec(self).map_err(|_| fmt::Error)?;
        write!(f, "{}", hex::encode(json))
    }
}

impl FromStr for StageTicket {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = hex::decode(s).map_err(|e| format!("invalid ticket hex: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid ticket JSON: {e}"))
    }
}

/// Current state snapshot for a Stage participant (sent to frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageParticipant {
    pub pubkey: String,
    pub role: StageRole,
    pub display_name: Option<String>,
    pub avatar_hash: Option<String>,
    pub hand_raised: bool,
    pub self_muted: bool,
    /// True if the host has suppressed this speaker in the mix.
    pub host_muted: bool,
}

/// Full room state snapshot delivered to the frontend on join or change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageState {
    pub stage_id: String,
    pub title: String,
    pub host_pubkey: String,
    pub my_pubkey: String,
    pub my_role: StageRole,
    pub participants: Vec<StageParticipant>,
    pub started_at: u64,
    /// Invite ticket string. Only populated for the host; `None` for joiners.
    pub ticket: Option<String>,
}

/// Events emitted to the Tauri frontend on the "stage-event" channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageEvent {
    /// Full state snapshot (on join or after significant topology change).
    StateSnapshot(StageState),
    /// A participant joined.
    ParticipantJoined { pubkey: String, role: StageRole },
    /// A participant left.
    ParticipantLeft { pubkey: String },
    /// A participant's role changed.
    RoleChanged { pubkey: String, role: StageRole },
    /// A participant's mute state changed.
    MuteChanged {
        pubkey: String,
        self_muted: bool,
        host_muted: bool,
    },
    /// Hand raised.
    HandRaised { pubkey: String },
    /// Hand lowered.
    HandLowered { pubkey: String },
    /// An incoming text reaction.
    Reaction { pubkey: String, emoji: String },
    /// An incoming chat message.
    Chat { pubkey: String, text: String },
    /// The room ended.
    Ended { stage_id: String },
    /// We were kicked from the room.
    Kicked,
    /// Stream authentication failed (relay may be tampering with audio).
    /// `source` is the transport NodeId of the node we were receiving from.
    AuthFailed { source: String, reason: String },
}
