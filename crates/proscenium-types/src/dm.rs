use crate::protocol::LinkedDevicesAnnouncement;
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

/// A DM payload encrypted to one device of the recipient. Sent as one JSON value on a
/// QUIC bi-stream (write_all + finish); the receiver answers with a `DmAck`.
///
/// The ciphertext is HPKE (RFC 9180) in Auth mode with DHKEM(X25519, HKDF-SHA256),
/// HKDF-SHA256 and ChaCha20-Poly1305. The sender and recipient keys are the X25519 forms
/// of the two devices' transport keys (their NodeIds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmEnvelope {
    /// Master pubkey of the sender.
    pub sender: String,
    /// NodeId of the sending device.
    pub sender_device: String,
    /// Master pubkey of the recipient.
    pub recipient: String,
    /// NodeId of the device the payload is encrypted to.
    pub recipient_device: String,
    /// The sender's signed device list. Must list `sender_device`.
    pub sender_devices: LinkedDevicesAnnouncement,
    /// HPKE encapsulated key (32 bytes).
    #[serde(with = "hex_bytes")]
    pub encapped_key: Vec<u8>,
    /// HPKE ciphertext of a JSON-serialized `DmPayload`.
    #[serde(with = "hex_bytes")]
    pub ciphertext: Vec<u8>,
}

/// Answer of the receiving device to a `DmEnvelope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmAck {
    /// The payload was decrypted and applied.
    Ok,
    /// The payload was not applied: the sender is blocked, or the envelope or its payload
    /// is invalid. Sending again gives the same answer.
    Rejected,
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

    fn envelope(ciphertext: Vec<u8>) -> DmEnvelope {
        DmEnvelope {
            sender: "aa".repeat(32),
            sender_device: "bb".repeat(32),
            recipient: "cc".repeat(32),
            recipient_device: "dd".repeat(32),
            sender_devices: LinkedDevicesAnnouncement {
                master_pubkey: "aa".repeat(32),
                devices: vec![],
                version: 1,
                signature: String::new(),
            },
            encapped_key: vec![1; 32],
            ciphertext,
        }
    }

    #[test]
    fn envelope_bytes_are_hex_strings() {
        let json = serde_json::to_string(&envelope(vec![0x00, 0x0f, 0xff])).unwrap();
        assert!(json.contains(r#""ciphertext":"000fff""#));
        assert!(json.contains(&format!(r#""encapped_key":"{}""#, "01".repeat(32))));
        let decoded: DmEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.ciphertext, vec![0x00, 0x0f, 0xff]);

        let not_hex = json.replace("000fff", "zz");
        assert!(serde_json::from_str::<DmEnvelope>(&not_hex).is_err());
        let array = json.replace(r#""000fff""#, "[0,15,255]");
        assert!(serde_json::from_str::<DmEnvelope>(&array).is_err());
    }

    #[test]
    fn ack_round_trips() {
        for ack in [DmAck::Ok, DmAck::Rejected] {
            let json = serde_json::to_vec(&ack).unwrap();
            assert!(json.len() <= 64);
            assert_eq!(serde_json::from_slice::<DmAck>(&json).unwrap(), ack);
        }
    }
}
