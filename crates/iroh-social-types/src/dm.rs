use crate::types::MediaAttachment;
use serde::{Deserialize, Serialize};

pub const DM_ALPN: &[u8] = b"iroh-social/dm/1";

// -- Wire types (sent over QUIC) --

/// Top-level discriminant for initiator→acceptor DM protocol messages.
/// Sent as a single JSON frame (write_all + finish) on a QUIC bi-stream.
/// Eliminates the need for the acceptor to type-sniff between message variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmMessage {
    Handshake(DmHandshake),
    Envelope(EncryptedEnvelope),
}

/// Sent by the acceptor→initiator after successfully receiving and processing
/// a DmMessage::Envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmAck;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmHandshake {
    Init {
        noise_message: Vec<u8>,
        /// Sender's DM public key (hex-encoded X25519).
        sender: String,
    },
    Response {
        noise_message: Vec<u8>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    /// Sender's DM public key (hex-encoded X25519).
    pub sender: String,
    pub ratchet_header: RatchetHeaderWire,
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
    Read { message_id: String },
    Delivered { message_id: String },
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
