use crate::protocol::LinkedDevicesAnnouncement;
use crate::types::MediaAttachment;
use serde::{Deserialize, Serialize};

pub const DM_ALPN: &[u8] = b"proscenium/dm/3";
pub const CALL_ALPN: &[u8] = b"proscenium/call/3";

// -- Wire types (sent over QUIC) --

/// A DM payload sent to one device of the recipient, as one JSON value on a QUIC
/// bi-stream (write_all + finish); the receiver answers with a `DmAck`.
///
/// The QUIC connection encrypts the envelope and authenticates both devices. The sending
/// device is the remote NodeId of the connection; `sender_devices` maps it to the sender.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmEnvelope {
    /// Master pubkey of the recipient.
    pub recipient: String,
    /// The sender's signed device list. Its `master_pubkey` is the sender. Must list the
    /// sending device.
    pub sender_devices: LinkedDevicesAnnouncement,
    pub payload: DmPayload,
}

/// Answer of the receiving device to a `DmEnvelope`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DmAck {
    /// The payload was applied.
    Ok,
    /// The payload was not applied: the sender is blocked, or the envelope or its payload
    /// is invalid. Sending again gives the same answer.
    Rejected,
    /// The payload was not applied because a rate limit of the recipient was reached.
    /// The sender keeps it queued and sends it again later. A sender that does not know
    /// this variant fails to parse it and also sends again.
    Deferred,
}

// -- Payload (inside `DmEnvelope`) --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmPayload {
    Message(DirectMessage),
    Typing,
    /// The reader read every message the recipient of this receipt sent in their
    /// conversation up to and including `message_id`, in the recipient's
    /// `(timestamp, id)` order. One receipt covers all messages read at once.
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
    fn ack_round_trips() {
        for ack in [DmAck::Ok, DmAck::Rejected] {
            let json = serde_json::to_vec(&ack).unwrap();
            assert!(json.len() <= 64);
            assert_eq!(serde_json::from_slice::<DmAck>(&json).unwrap(), ack);
        }
    }
}
