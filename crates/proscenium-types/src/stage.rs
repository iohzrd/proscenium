use crate::signing::{canonical_bytes, signature_to_hex, verify_signed_by};
use crate::validation::{is_valid_id, is_valid_pubkey, parse_pubkey};
use iroh::{PublicKey, SecretKey};
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

/// QUIC ALPN for the Stage audio transport protocol.
pub const STAGE_ALPN: &[u8] = b"proscenium/stage/2";

/// Maximum stage title length in characters.
pub const MAX_STAGE_TITLE_CHARS: usize = 200;
/// Maximum chat message length in characters.
pub const MAX_STAGE_CHAT_CHARS: usize = 500;
/// Maximum reaction length in characters.
pub const MAX_STAGE_REACTION_CHARS: usize = 16;
/// Maximum number of speakers and co-hosts combined, not counting the host.
pub const MAX_STAGE_SPEAKERS: usize = 16;
/// Maximum number of banned master pubkeys per stage.
pub const MAX_STAGE_BANNED: usize = 256;
/// Maximum number of participant entries a node tracks per stage.
pub const MAX_STAGE_PARTICIPANTS: usize = 1000;
/// Maximum number of listener pubkeys in one `RelayAssignment`.
pub const MAX_RELAY_ASSIGNMENT_LISTENERS: usize = 100;
/// Maximum length of an encoded `StageTicket` string.
pub const MAX_STAGE_TICKET_LEN: usize = 8192;

const STAGE_CONTROL_TAG: &str = "proscenium/stage-control/2";

/// Derive the iroh-gossip TopicId for a Stage room's control plane.
pub fn stage_control_topic(stage_id: &str) -> TopicId {
    let mut hasher = Sha256::new();
    hasher.update(b"proscenium-stage-v2:");
    hasher.update(stage_id.as_bytes());
    TopicId::from_bytes(hasher.finalize().into())
}

/// A participant's role in a Stage room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageRole {
    /// Room creator; mixes all speaker audio and distributes host-mixed stream.
    Host,
    /// Speaker who can also promote, demote, mute and kick participants that are
    /// neither the host nor a co-host.
    CoHost,
    /// Active audio sender; has an open QUIC stream to the host.
    Speaker,
    /// Receive-only; gets host-mixed audio via the relay tree.
    Listener,
}

/// Roles assigned by the host. Signed by the host as `StageControl::RoomState`.
/// Every pubkey is a master pubkey. A participant in none of the lists is a listener.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    /// Incremented by the host on every change. A receiver applies a snapshot only
    /// if `version` is higher than the last one it applied.
    pub version: u64,
    pub co_hosts: Vec<String>,
    pub speakers: Vec<String>,
    /// Speakers whose audio the host discards.
    pub host_muted: Vec<String>,
    pub banned: Vec<String>,
}

/// Control-plane messages exchanged over the iroh-gossip Stage topic.
/// The acting participant is the master pubkey that signed the enclosing
/// `SignedStageControl`; a `pubkey` field names the target of the action and is a master
/// pubkey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StageControl {
    /// Host signals the room has ended.
    End,
    /// Participant heartbeat (15 s interval). Also sent as the first frame of every
    /// audio connection.
    Presence {
        /// Transport NodeId of the sending device.
        node_id: String,
    },
    /// Host publishes the roles it assigned.
    RoomState(RoomState),
    /// A speaker or co-host offers to serve the mixed stream to listeners.
    RelayVolunteer { capacity: u32 },
    /// Host tells the listed listeners which node serves them the mixed stream.
    RelayAssignment {
        /// Transport NodeId of the relay, or of the host.
        source_node_id: String,
        listener_pubkeys: Vec<String>,
    },
    /// Sender requests a speaker slot.
    RaiseHand,
    /// Sender withdraws the request.
    LowerHand,
    /// Host or co-host promotes a listener to speaker.
    PromoteSpeaker { pubkey: String },
    /// Host or co-host demotes a speaker to listener.
    DemoteSpeaker { pubkey: String },
    /// Host or co-host makes the host discard a speaker's audio.
    MuteSpeaker { pubkey: String },
    /// Sender reports its own microphone mute state.
    SelfMuteToggle { muted: bool },
    /// Host or co-host removes a participant from the room.
    Kick { pubkey: String },
    /// Host removes a participant and rejects it for the rest of the room.
    Ban { pubkey: String },
    /// Text reaction (at most `MAX_STAGE_REACTION_CHARS` characters).
    Reaction { emoji: String },
    /// In-room text chat message (at most `MAX_STAGE_CHAT_CHARS` characters).
    Chat { text: String },
}

fn validate_pubkey_list(name: &str, list: &[String], max: usize) -> Result<(), String> {
    if list.len() > max {
        return Err(format!("{name} has {} entries (max {max})", list.len()));
    }
    let mut seen = HashSet::with_capacity(list.len());
    for pubkey in list {
        if !is_valid_pubkey(pubkey) {
            return Err(format!("{name} holds an invalid pubkey"));
        }
        if !seen.insert(pubkey.as_str()) {
            return Err(format!("{name} holds a repeated pubkey"));
        }
    }
    Ok(())
}

/// Check the list sizes and pubkey formats of a `RoomState`.
pub fn validate_room_state(state: &RoomState) -> Result<(), String> {
    if state.co_hosts.len() + state.speakers.len() > MAX_STAGE_SPEAKERS {
        return Err(format!(
            "room state lists more than {MAX_STAGE_SPEAKERS} speakers"
        ));
    }
    validate_pubkey_list("co_hosts", &state.co_hosts, MAX_STAGE_SPEAKERS)?;
    validate_pubkey_list("speakers", &state.speakers, MAX_STAGE_SPEAKERS)?;
    validate_pubkey_list("host_muted", &state.host_muted, MAX_STAGE_SPEAKERS)?;
    validate_pubkey_list("banned", &state.banned, MAX_STAGE_BANNED)
}

/// Check field formats and size limits of a control payload.
pub fn validate_stage_control(control: &StageControl) -> Result<(), String> {
    match control {
        StageControl::End
        | StageControl::RaiseHand
        | StageControl::LowerHand
        | StageControl::SelfMuteToggle { .. }
        | StageControl::RelayVolunteer { .. } => Ok(()),
        StageControl::Presence { node_id } => {
            if is_valid_pubkey(node_id) {
                Ok(())
            } else {
                Err("presence holds an invalid node id".to_string())
            }
        }
        StageControl::RoomState(state) => validate_room_state(state),
        StageControl::RelayAssignment {
            source_node_id,
            listener_pubkeys,
        } => {
            if !is_valid_pubkey(source_node_id) {
                return Err("relay assignment holds an invalid node id".to_string());
            }
            validate_pubkey_list(
                "listener_pubkeys",
                listener_pubkeys,
                MAX_RELAY_ASSIGNMENT_LISTENERS,
            )
        }
        StageControl::PromoteSpeaker { pubkey }
        | StageControl::DemoteSpeaker { pubkey }
        | StageControl::MuteSpeaker { pubkey }
        | StageControl::Kick { pubkey }
        | StageControl::Ban { pubkey } => {
            if is_valid_pubkey(pubkey) {
                Ok(())
            } else {
                Err("target pubkey is invalid".to_string())
            }
        }
        StageControl::Reaction { emoji } => {
            let chars = emoji.chars().count();
            if chars == 0 || chars > MAX_STAGE_REACTION_CHARS {
                return Err(format!("reaction has {chars} characters"));
            }
            Ok(())
        }
        StageControl::Chat { text } => {
            let chars = text.chars().count();
            if chars == 0 || chars > MAX_STAGE_CHAT_CHARS {
                return Err(format!("chat message has {chars} characters"));
            }
            Ok(())
        }
    }
}

/// A signed StageControl message for authenticated gossip broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedStageControl {
    /// Stage this message belongs to. Covered by the signature.
    pub stage_id: String,
    /// Master pubkey of the sender.
    pub sender_pubkey: String,
    /// Unix timestamp milliseconds when this was signed. Strictly increasing per sender.
    pub timestamp: u64,
    /// The control message payload.
    pub control: StageControl,
    /// Ed25519 signature (hex) by `sender_pubkey` over the canonical bytes.
    pub signature: String,
}

/// Canonical bytes for signing a StageControl message.
fn stage_control_signing_bytes(
    stage_id: &str,
    sender_pubkey: &str,
    timestamp: u64,
    control: &StageControl,
) -> Vec<u8> {
    canonical_bytes(&(
        STAGE_CONTROL_TAG,
        stage_id,
        sender_pubkey,
        timestamp,
        control,
    ))
}

/// Sign a StageControl message for `stage_id` with the sender's master key.
pub fn sign_stage_control(
    stage_id: &str,
    control: StageControl,
    master_key: &SecretKey,
    timestamp: u64,
) -> SignedStageControl {
    let sender_pubkey = master_key.public().to_string();
    let bytes = stage_control_signing_bytes(stage_id, &sender_pubkey, timestamp, &control);
    let sig = master_key.sign(&bytes);
    SignedStageControl {
        stage_id: stage_id.to_string(),
        sender_pubkey,
        timestamp,
        control,
        signature: signature_to_hex(&sig),
    }
}

/// Verify that a StageControl message was signed by the key `signed.sender_pubkey`.
/// The caller checks `stage_id` and `timestamp`.
pub fn verify_stage_control(signed: &SignedStageControl) -> Result<(), String> {
    verify_signed_by(
        &signed.sender_pubkey,
        &stage_control_signing_bytes(
            &signed.stage_id,
            &signed.sender_pubkey,
            signed.timestamp,
            &signed.control,
        ),
        &signed.signature,
        "stage control",
    )
}

/// Identity and device named by a verified `Presence` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPresence {
    pub master_pubkey: String,
    pub node_id: String,
}

/// Verify a signed `Presence`: the message signature and the NodeId format.
/// The caller checks `stage_id`, `timestamp` and, on an audio connection, that
/// `node_id` equals the remote NodeId.
pub fn verify_presence(signed: &SignedStageControl) -> Result<VerifiedPresence, String> {
    let StageControl::Presence { node_id } = &signed.control else {
        return Err("not a presence message".to_string());
    };
    verify_stage_control(signed)?;
    if !is_valid_pubkey(node_id) {
        return Err("presence holds an invalid node id".to_string());
    }
    Ok(VerifiedPresence {
        master_pubkey: signed.sender_pubkey.clone(),
        node_id: node_id.clone(),
    })
}

/// Shareable invite ticket for joining a Stage room.
/// Encoded as hex(JSON) for Display/FromStr.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageTicket {
    pub stage_id: String,
    /// Host's master pubkey. Its key signs the host's control messages and the
    /// checkpoints of the mixed audio stream.
    pub host_pubkey: String,
    /// Transport NodeId of the host (for direct QUIC connection).
    pub host_node_id: String,
    pub title: String,
}

impl StageTicket {
    /// Check the field formats. Returns the host's master public key.
    pub fn verify(&self) -> Result<PublicKey, String> {
        if !is_valid_id(&self.stage_id) {
            return Err("invalid stage id".to_string());
        }
        if self.title.chars().count() > MAX_STAGE_TITLE_CHARS {
            return Err("stage title too long".to_string());
        }
        if !is_valid_pubkey(&self.host_node_id) {
            return Err("invalid host node id".to_string());
        }
        parse_pubkey(&self.host_pubkey).map_err(|e| format!("invalid host pubkey: {e}"))
    }
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
        if s.len() > MAX_STAGE_TICKET_LEN {
            return Err(format!("ticket too long: {} bytes", s.len()));
        }
        let bytes = hex::decode(s).map_err(|e| format!("invalid ticket hex: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid ticket JSON: {e}"))
    }
}

/// Current state snapshot for a Stage participant (sent to frontend).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageParticipant {
    /// Master pubkey.
    pub pubkey: String,
    pub role: StageRole,
    pub display_name: Option<String>,
    pub avatar_hash: Option<String>,
    pub hand_raised: bool,
    pub self_muted: bool,
    /// True if the host discards this speaker's audio.
    pub host_muted: bool,
}

/// Full room state snapshot delivered to the frontend on join or change.
/// `host_pubkey` and `my_pubkey` are master pubkeys.
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
    /// Local microphone mute flag.
    pub self_muted: bool,
    /// Local raised-hand flag.
    pub hand_raised: bool,
}

/// Events emitted to the Tauri frontend on the "stage-event" channel.
/// Every `pubkey` is a master pubkey.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageEvent {
    /// Full state snapshot (on join or after a role change).
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
    /// We were kicked or banned from the room.
    Kicked,
    /// Stream authentication failed (relay may be tampering with audio).
    /// `source` is the transport NodeId of the node we were receiving from.
    AuthFailed { source: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::now_millis;

    const STAGE_ID: &str = "000102030405060708090a0b0c0d0e0f";

    struct TestIdentity {
        master_pubkey: String,
        key: SecretKey,
    }

    fn identity(seed: u8) -> TestIdentity {
        let key = SecretKey::from_bytes(&[seed; 32]);
        TestIdentity {
            master_pubkey: key.public().to_string(),
            key,
        }
    }

    fn node_id(seed: u8) -> String {
        SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    fn presence(id: &TestIdentity, node: &str) -> SignedStageControl {
        sign_stage_control(
            STAGE_ID,
            StageControl::Presence {
                node_id: node.to_string(),
            },
            &id.key,
            now_millis(),
        )
    }

    #[test]
    fn control_signature_covers_stage_id_timestamp_and_payload() {
        let id = identity(1);
        let signed = sign_stage_control(
            STAGE_ID,
            StageControl::Chat {
                text: "hello".to_string(),
            },
            &id.key,
            1000,
        );
        assert!(verify_stage_control(&signed).is_ok());

        let mut other_stage = signed.clone();
        other_stage.stage_id = "ff0102030405060708090a0b0c0d0e0f".to_string();
        assert!(verify_stage_control(&other_stage).is_err());

        let mut other_time = signed.clone();
        other_time.timestamp = 1001;
        assert!(verify_stage_control(&other_time).is_err());

        let mut other_payload = signed.clone();
        other_payload.control = StageControl::Chat {
            text: "bye".to_string(),
        };
        assert!(verify_stage_control(&other_payload).is_err());

        let mut other_sender = signed;
        other_sender.sender_pubkey = identity(2).master_pubkey;
        assert!(verify_stage_control(&other_sender).is_err());
    }

    #[test]
    fn presence_names_master_pubkey_of_signer() {
        let a = identity(1);
        let node = node_id(40);
        let verified = verify_presence(&presence(&a, &node)).unwrap();
        assert_eq!(verified.master_pubkey, a.master_pubkey);
        assert_eq!(verified.node_id, node);
    }

    #[test]
    fn presence_signed_by_another_identity_is_rejected() {
        let a = identity(1);
        let b = identity(2);
        // Signed by b's key, naming a as the sender.
        let mut forged = presence(&b, &node_id(40));
        forged.sender_pubkey = a.master_pubkey.clone();
        assert!(verify_presence(&forged).is_err());

        // Not a presence message.
        let chat = sign_stage_control(
            STAGE_ID,
            StageControl::Chat {
                text: "hi".to_string(),
            },
            &a.key,
            now_millis(),
        );
        assert!(verify_presence(&chat).is_err());
    }

    #[test]
    fn ticket_round_trip_and_verification() {
        let host = identity(3);
        let ticket = StageTicket {
            stage_id: STAGE_ID.to_string(),
            host_pubkey: host.master_pubkey.clone(),
            host_node_id: node_id(41),
            title: "title".to_string(),
        };
        let parsed: StageTicket = ticket.to_string().parse().unwrap();
        assert_eq!(parsed.verify().unwrap(), host.key.public());

        let mut bad_host = parsed.clone();
        bad_host.host_pubkey = host.master_pubkey.to_uppercase();
        assert!(bad_host.verify().is_err());

        let mut bad_id = parsed;
        bad_id.stage_id = "not-an-id".to_string();
        assert!(bad_id.verify().is_err());

        assert!("zz".parse::<StageTicket>().is_err());
        assert!(
            "ab".repeat(MAX_STAGE_TICKET_LEN)
                .parse::<StageTicket>()
                .is_err()
        );
    }

    #[test]
    fn control_payload_bounds() {
        let ok_key = identity(5).master_pubkey;
        assert!(
            validate_stage_control(&StageControl::Chat {
                text: "a".repeat(MAX_STAGE_CHAT_CHARS)
            })
            .is_ok()
        );
        assert!(
            validate_stage_control(&StageControl::Chat {
                text: "a".repeat(MAX_STAGE_CHAT_CHARS + 1)
            })
            .is_err()
        );
        assert!(
            validate_stage_control(&StageControl::Chat {
                text: String::new()
            })
            .is_err()
        );
        assert!(
            validate_stage_control(&StageControl::Reaction {
                emoji: "a".repeat(MAX_STAGE_REACTION_CHARS + 1)
            })
            .is_err()
        );
        assert!(
            validate_stage_control(&StageControl::Kick {
                pubkey: "x".repeat(5000)
            })
            .is_err()
        );
        assert!(
            validate_stage_control(&StageControl::RelayAssignment {
                source_node_id: ok_key.clone(),
                listener_pubkeys: vec![ok_key.clone(); 2],
            })
            .is_err()
        );
        let too_many: Vec<String> = (0..=MAX_STAGE_SPEAKERS as u8)
            .map(|i| identity(i + 10).master_pubkey)
            .collect();
        assert!(
            validate_room_state(&RoomState {
                version: 1,
                speakers: too_many,
                ..RoomState::default()
            })
            .is_err()
        );
        assert!(
            validate_room_state(&RoomState {
                version: 1,
                speakers: vec![ok_key],
                ..RoomState::default()
            })
            .is_ok()
        );
    }
}
