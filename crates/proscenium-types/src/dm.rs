use crate::types::MediaAttachment;
use serde::{Deserialize, Serialize};

pub const DM_ALPN: &[u8] = b"proscenium/dm/2";
pub const CALL_ALPN: &[u8] = b"proscenium/call/2";

// -- Wire types (sent over QUIC) --

/// Serde adapter: a byte vector as a lowercase hex string.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        hex::decode(&encoded).map_err(serde::de::Error::custom)
    }
}

/// Frame sent by the initiator of a DM connection.
/// One JSON value (write_all + finish) on a QUIC bi-stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmMessage {
    Handshake(DmHandshake),
    Envelope(EncryptedEnvelope),
}

/// Answer of the acceptor to a `DmMessage::Envelope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmAck {
    /// The envelope was decrypted and its payload was applied.
    Ok,
    /// The envelope was not applied.
    Failed(DmNack),
}

/// Reason an envelope was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmNack {
    /// The acceptor has no usable ratchet session for the sender. A new handshake is required.
    NoSession,
    /// The acceptor's session could not decrypt the envelope. A new handshake is required.
    DecryptFailed,
    /// The sender is blocked, or the envelope or its payload is invalid. Sending again
    /// gives the same answer.
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmHandshake {
    Init {
        /// Noise IK message 1.
        #[serde(with = "hex_bytes")]
        noise_message: Vec<u8>,
        /// Sender's DM public key (hex-encoded X25519).
        sender: String,
    },
    Response {
        /// Noise IK message 2.
        #[serde(with = "hex_bytes")]
        noise_message: Vec<u8>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    /// Sender's DM public key (hex-encoded X25519).
    pub sender: String,
    pub ratchet_header: RatchetHeaderWire,
    #[serde(with = "hex_bytes")]
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetHeaderWire {
    pub dh_public: String,
    pub message_number: u32,
    pub previous_chain_length: u32,
}

// -- Decrypted payload (inside ciphertext) --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmPayload {
    Message(DirectMessage),
    Typing,
    Read {
        message_id: String,
    },
    Delivered {
        message_id: String,
    },
    /// Offer to start a call. Sent by the caller.
    CallOffer {
        call_id: String,
        /// Whether video is offered (audio is always included).
        video: bool,
    },
    /// Accept an incoming call. Sent by the callee.
    CallAnswer {
        call_id: String,
    },
    /// Reject or cancel an incoming call.
    CallReject {
        call_id: String,
    },
    /// End an active call.
    CallHangup {
        call_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectMessage {
    pub id: String,
    pub content: String,
    pub timestamp: u64,
    #[serde(default)]
    pub media: Vec<MediaAttachment>,
    pub reply_to: Option<String>,
}

// -- Call transport types --

/// Header prepended to each Opus frame on the QUIC audio stream.
/// Fixed 8-byte binary header: [seq:u32 BE][timestamp:u32 BE]
/// Followed by the raw Opus-encoded bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFrameHeader {
    /// Monotonic sequence number (wraps at u32::MAX).
    pub seq: u32,
    /// RTP-style timestamp in sample units (48kHz clock).
    pub timestamp: u32,
}

/// Call state emitted to the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    /// Outgoing call ringing (waiting for answer).
    Ringing,
    /// Incoming call offer received.
    Incoming,
    /// Call is active (audio flowing).
    Active,
    /// Call ended normally.
    Ended,
    /// Call failed (peer unreachable, timeout, etc.).
    Failed,
}

/// Event payload emitted to the frontend for call state changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEvent {
    pub call_id: String,
    pub peer_pubkey: String,
    pub state: CallState,
}

// -- Frontend-facing types --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    pub peer_pubkey: String,
    pub last_message_at: u64,
    pub last_message_preview: String,
    pub unread_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub conversation_id: String,
    pub from_pubkey: String,
    pub to_pubkey: String,
    pub content: String,
    pub timestamp: u64,
    #[serde(default)]
    pub media: Vec<MediaAttachment>,
    pub read: bool,
    pub delivered: bool,
    pub reply_to: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_ciphertext_is_a_hex_string() {
        let envelope = EncryptedEnvelope {
            sender: "aa".repeat(32),
            ratchet_header: RatchetHeaderWire {
                dh_public: "bb".repeat(32),
                message_number: 7,
                previous_chain_length: 3,
            },
            ciphertext: vec![0x00, 0x0f, 0xff],
        };
        let json = serde_json::to_string(&DmMessage::Envelope(envelope)).unwrap();
        assert!(json.contains(r#""ciphertext":"000fff""#));
        let DmMessage::Envelope(decoded) = serde_json::from_str(&json).unwrap() else {
            panic!("expected an envelope");
        };
        assert_eq!(decoded.ciphertext, vec![0x00, 0x0f, 0xff]);
        assert_eq!(decoded.ratchet_header.message_number, 7);
    }

    #[test]
    fn handshake_noise_message_round_trips() {
        let init = DmHandshake::Init {
            noise_message: vec![1, 2, 3],
            sender: "cc".repeat(32),
        };
        let json = serde_json::to_string(&init).unwrap();
        assert!(json.contains(r#""noise_message":"010203""#));
        let DmHandshake::Init { noise_message, .. } = serde_json::from_str(&json).unwrap() else {
            panic!("expected Init");
        };
        assert_eq!(noise_message, vec![1, 2, 3]);
    }

    #[test]
    fn non_hex_ciphertext_is_rejected() {
        let json = r#"{"sender":"aa","ratchet_header":{"dh_public":"bb","message_number":0,"previous_chain_length":0},"ciphertext":[1,2,3]}"#;
        assert!(serde_json::from_str::<EncryptedEnvelope>(json).is_err());
        let json = r#"{"sender":"aa","ratchet_header":{"dh_public":"bb","message_number":0,"previous_chain_length":0},"ciphertext":"zz"}"#;
        assert!(serde_json::from_str::<EncryptedEnvelope>(json).is_err());
    }

    #[test]
    fn ack_round_trips() {
        for ack in [
            DmAck::Ok,
            DmAck::Failed(DmNack::NoSession),
            DmAck::Failed(DmNack::DecryptFailed),
            DmAck::Failed(DmNack::Rejected),
        ] {
            let json = serde_json::to_vec(&ack).unwrap();
            assert!(json.len() <= 64);
            assert_eq!(serde_json::from_slice::<DmAck>(&json).unwrap(), ack);
        }
    }
}
