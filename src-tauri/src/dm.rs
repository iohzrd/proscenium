use crate::crypto::{
    RatchetHeader, RatchetState, ed25519_public_to_x25519, ed25519_secret_to_x25519,
    noise_complete_initiator, noise_complete_responder, noise_initiate, noise_respond,
    x25519_public_from_private,
};
use crate::storage::Storage;
use base64::Engine as _;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, aead::Aead};
use hkdf::Hkdf;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_social_types::{
    DM_ALPN, DirectMessage, DmHandshake, DmPayload, EncryptedEnvelope, RatchetHeaderWire,
    StoredMessage, now_millis, short_id,
};
use sha2::Sha256;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone)]
pub struct DmHandler {
    storage: Arc<Storage>,
    app_handle: AppHandle,
    my_x25519_private: [u8; 32],
    my_x25519_public: [u8; 32],
    /// Master pubkey — used for conversation/message storage identifiers.
    my_master_pubkey_str: String,
    /// Signing pubkey — used as the DM identity sent on the wire (envelope.sender).
    my_signing_pubkey_str: String,
    /// Key derived from master secret for encrypting ratchet state at rest.
    ratchet_storage_key: [u8; 32],
}

impl DmHandler {
    pub fn new(
        storage: Arc<Storage>,
        app_handle: AppHandle,
        signing_secret: [u8; 32],
        master_secret: [u8; 32],
        master_pubkey_str: String,
        signing_pubkey_str: String,
    ) -> Self {
        let my_x25519_private = ed25519_secret_to_x25519(&signing_secret);
        let my_x25519_public = x25519_public_from_private(&my_x25519_private);

        // Derive a stable storage key from the permanent master key.
        // This survives signing key rotation since it's anchored to the master.
        let hk = Hkdf::<Sha256>::new(None, &master_secret);
        let mut ratchet_storage_key = [0u8; 32];
        hk.expand(b"iroh-social-ratchet-storage-v1", &mut ratchet_storage_key)
            .expect("HKDF expand valid length");

        Self {
            storage,
            app_handle,
            my_x25519_private,
            my_x25519_public,
            my_master_pubkey_str: master_pubkey_str,
            my_signing_pubkey_str: signing_pubkey_str,
            ratchet_storage_key,
        }
    }

    /// Get or establish a ratchet session with a peer.
    /// Returns `(RatchetState, peer_signing_pubkey)` — callers must use `peer_signing_pubkey`
    /// as the key for any subsequent `save_ratchet_session` calls.
    async fn get_or_establish_session(
        &self,
        endpoint: &Endpoint,
        peer_master_pubkey: &str,
    ) -> anyhow::Result<(RatchetState, String)> {
        log::info!(
            "[dm] get_or_establish_session: peer={}",
            short_id(peer_master_pubkey)
        );

        // Resolve the peer's signing pubkey — this is the session key for ratchet storage.
        let peer_signing_pubkey = self
            .storage
            .get_peer_signing_pubkey(peer_master_pubkey)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no signing pubkey cached for {}",
                    short_id(peer_master_pubkey)
                )
            })?;

        // Try loading existing session (keyed by signing pubkey)
        if let Some(stored) = self.storage.get_ratchet_session(&peer_signing_pubkey)? {
            log::info!(
                "[dm] loaded existing ratchet session for {}",
                short_id(peer_master_pubkey),
            );
            let json = open_ratchet_state(&self.ratchet_storage_key, &stored)?;
            let state: RatchetState = serde_json::from_str(&json)?;
            return Ok((state, peer_signing_pubkey));
        }

        // No session — need to handshake
        log::info!(
            "[dm] no existing session, initiating Noise IK handshake with {}",
            short_id(peer_master_pubkey)
        );

        // Fail fast: resolve transport NodeId before doing any crypto work
        let node_ids = self
            .storage
            .get_peer_transport_node_ids(peer_master_pubkey)?;
        let transport_id_str = node_ids.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("no transport NodeId for {}", short_id(peer_master_pubkey))
        })?;
        let transport_id: EndpointId = transport_id_str.parse()?;
        let addr = EndpointAddr::from(transport_id);

        // Use the peer's signing pubkey for x25519 (mirrors how we derive our own from signing key)
        let peer_signing_id: EndpointId = peer_signing_pubkey.parse().map_err(|e| {
            log::error!("[dm] failed to parse peer signing pubkey: {e}");
            anyhow::anyhow!("invalid peer signing pubkey: {e}")
        })?;
        let peer_ed_public = peer_signing_id.as_bytes();
        let peer_x25519_public = ed25519_public_to_x25519(peer_ed_public)
            .ok_or_else(|| anyhow::anyhow!("invalid peer signing public key"))?;
        log::info!("[dm] converted peer signing ed25519 -> x25519 key");

        // Noise IK handshake: initiator
        let (initiator_hs, msg1) = noise_initiate(&self.my_x25519_private, &peer_x25519_public)
            .map_err(|e| anyhow::anyhow!("noise init: {e}"))?;
        log::info!("[dm] noise init message created ({} bytes)", msg1.len());

        log::info!(
            "[dm] connecting to {} on DM_ALPN...",
            short_id(peer_master_pubkey)
        );
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            endpoint.connect(addr, DM_ALPN),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "handshake connect timeout to {}",
                short_id(peer_master_pubkey)
            )
        })
        .and_then(|r| {
            r.map_err(|e| {
                log::error!(
                    "[dm] QUIC connect failed to {}: {e}",
                    short_id(peer_master_pubkey)
                );
                anyhow::anyhow!("{e}")
            })
        })?;
        log::info!("[dm] QUIC connected, opening bi-stream...");
        let (mut send, mut recv) = conn.open_bi().await?;

        // Send handshake init — sender is our signing pubkey (the DM identity)
        let handshake = DmHandshake::Init {
            noise_message: msg1,
            sender: self.my_signing_pubkey_str.clone(),
        };
        let bytes = serde_json::to_vec(&handshake)?;
        log::info!("[dm] sending handshake init ({} bytes)...", bytes.len());
        send.write_all(&bytes).await?;
        send.finish()?;

        // Read handshake response
        log::info!("[dm] waiting for handshake response...");
        let resp_bytes = recv.read_to_end(65536).await?;
        log::info!(
            "[dm] received handshake response ({} bytes)",
            resp_bytes.len()
        );
        let resp: DmHandshake = serde_json::from_slice(&resp_bytes)?;

        let noise_response = match resp {
            DmHandshake::Response { noise_message } => noise_message,
            _ => return Err(anyhow::anyhow!("unexpected handshake response")),
        };

        // Complete handshake
        let shared_secret = noise_complete_initiator(initiator_hs, &noise_response)
            .map_err(|e| anyhow::anyhow!("noise complete: {e}"))?;
        log::info!("[dm] noise handshake completed successfully");

        conn.close(0u32.into(), b"done");

        // Initialize Double Ratchet as Alice (initiator)
        let ratchet = RatchetState::init_alice(&shared_secret, &peer_x25519_public);

        // Encrypt and save session keyed by peer's signing pubkey
        let json = serde_json::to_string(&ratchet)?;
        let sealed = seal_ratchet_state(&self.ratchet_storage_key, &json)?;
        self.storage
            .save_ratchet_session(&peer_signing_pubkey, &sealed, now_millis())?;

        log::info!(
            "[dm] established and saved ratchet session with {}",
            short_id(peer_master_pubkey)
        );
        Ok((ratchet, peer_signing_pubkey))
    }

    /// Send a DM to a peer. Encrypts with Double Ratchet and sends over QUIC.
    /// If the peer is offline, queues to outbox.
    /// On successful delivery, marks the message as delivered and emits `dm-delivered`.
    pub async fn send_dm(
        &self,
        endpoint: &Endpoint,
        peer_pubkey: &str,
        message: DirectMessage,
    ) -> anyhow::Result<()> {
        let message_id = message.id.clone();
        let (mut ratchet, peer_signing_pubkey) =
            self.get_or_establish_session(endpoint, peer_pubkey).await?;

        // Encrypt the message
        let payload = DmPayload::Message(message);
        let plaintext = serde_json::to_vec(&payload)?;
        let (header, ciphertext) = ratchet.encrypt(&plaintext);

        // Encrypt and save updated ratchet state (keyed by signing pubkey)
        let ratchet_json = serde_json::to_string(&ratchet)?;
        let ratchet_sealed = seal_ratchet_state(&self.ratchet_storage_key, &ratchet_json)?;
        self.storage
            .save_ratchet_session(&peer_signing_pubkey, &ratchet_sealed, now_millis())?;

        let envelope = EncryptedEnvelope {
            sender: self.my_signing_pubkey_str.clone(),
            ratchet_header: ratchet_header_to_wire(&header),
            ciphertext,
        };
        log::debug!(
            "[dm] encrypted envelope: sender={} msg_n={} prev_chain={} dh={} ciphertext_len={}",
            short_id(&envelope.sender),
            envelope.ratchet_header.message_number,
            envelope.ratchet_header.previous_chain_length,
            &envelope.ratchet_header.dh_public[..8],
            envelope.ciphertext.len(),
        );

        // Try to send
        match self
            .try_send_envelope(endpoint, peer_pubkey, &envelope)
            .await
        {
            Ok(()) => {
                log::info!("[dm] sent message to {}", short_id(peer_pubkey));
                self.mark_delivered(&message_id);
            }
            Err(e) => {
                log::warn!(
                    "[dm] peer {} offline, queuing to outbox: {e}",
                    short_id(peer_pubkey)
                );
                let envelope_json = serde_json::to_string(&envelope)?;
                let id = uuid::Uuid::new_v4().to_string();
                self.storage.insert_outbox_message(
                    &id,
                    peer_pubkey,
                    &envelope_json,
                    now_millis(),
                    &message_id,
                )?;
            }
        }

        Ok(())
    }

    /// Mark a message as delivered in storage and notify the frontend.
    fn mark_delivered(&self, message_id: &str) {
        if let Err(e) = self.storage.mark_dm_delivered(message_id) {
            log::error!(
                "[dm] failed to mark delivered {}: {e}",
                short_id(message_id)
            );
            return;
        }
        let _ = self.app_handle.emit(
            "dm-delivered",
            serde_json::json!({ "message_id": message_id }),
        );
    }

    /// Try to send an encrypted envelope to a peer over QUIC.
    async fn try_send_envelope(
        &self,
        endpoint: &Endpoint,
        peer_master_pubkey: &str,
        envelope: &EncryptedEnvelope,
    ) -> anyhow::Result<()> {
        let node_ids = self
            .storage
            .get_peer_transport_node_ids(peer_master_pubkey)?;
        let transport_id_str = node_ids.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!("no transport NodeId for {}", short_id(peer_master_pubkey))
        })?;
        let transport_id: EndpointId = transport_id_str.parse()?;
        let addr = EndpointAddr::from(transport_id);

        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            endpoint.connect(addr, DM_ALPN),
        )
        .await
        .map_err(|_| anyhow::anyhow!("connection timeout"))??;

        let (mut send, mut recv) = conn.open_bi().await?;

        let bytes = serde_json::to_vec(envelope)?;
        send.write_all(&bytes).await?;
        send.finish()?;

        // Wait for ACK
        let ack = tokio::time::timeout(std::time::Duration::from_secs(5), recv.read_to_end(1024))
            .await
            .map_err(|_| anyhow::anyhow!("ack timeout"))??;

        if ack != b"ok" {
            return Err(anyhow::anyhow!("unexpected ack: {:?}", ack));
        }

        conn.close(0u32.into(), b"done");
        Ok(())
    }

    /// Flush all pending outbox messages for a peer.
    pub async fn flush_outbox_for_peer(
        &self,
        endpoint: &Endpoint,
        peer_pubkey: &str,
    ) -> anyhow::Result<(u32, u32)> {
        let entries = self.storage.get_outbox_for_peer(peer_pubkey)?;
        if entries.is_empty() {
            return Ok((0, 0));
        }

        let mut sent = 0u32;
        let mut failed = 0u32;

        for (id, envelope_json, message_id) in &entries {
            let envelope: EncryptedEnvelope = match serde_json::from_str(envelope_json) {
                Ok(e) => e,
                Err(_) => {
                    self.storage.remove_outbox_message(id)?;
                    failed += 1;
                    continue;
                }
            };

            match self
                .try_send_envelope(endpoint, peer_pubkey, &envelope)
                .await
            {
                Ok(()) => {
                    self.storage.remove_outbox_message(id)?;
                    self.mark_delivered(message_id);
                    sent += 1;
                }
                Err(_) => {
                    failed += 1;
                    // Stop trying this peer if first message fails (they're offline)
                    break;
                }
            }
        }

        if sent > 0 {
            log::info!(
                "[dm-outbox] flushed {sent} messages to {}",
                short_id(peer_pubkey)
            );
        }
        Ok((sent, failed))
    }

    /// Send a lightweight DM signal (typing, read receipt) without storing a message.
    pub async fn send_signal(
        &self,
        endpoint: &Endpoint,
        peer_pubkey: &str,
        payload: DmPayload,
    ) -> anyhow::Result<()> {
        let (mut ratchet, peer_signing_pubkey) =
            self.get_or_establish_session(endpoint, peer_pubkey).await?;

        let plaintext = serde_json::to_vec(&payload)?;
        let (header, ciphertext) = ratchet.encrypt(&plaintext);

        let ratchet_json = serde_json::to_string(&ratchet)?;
        let ratchet_sealed = seal_ratchet_state(&self.ratchet_storage_key, &ratchet_json)?;
        self.storage
            .save_ratchet_session(&peer_signing_pubkey, &ratchet_sealed, now_millis())?;

        let envelope = EncryptedEnvelope {
            sender: self.my_signing_pubkey_str.clone(),
            ratchet_header: ratchet_header_to_wire(&header),
            ciphertext,
        };

        self.try_send_envelope(endpoint, peer_pubkey, &envelope)
            .await?;

        Ok(())
    }

    /// Handle an incoming handshake (Noise IK responder side).
    /// `peer_signing_pubkey` is taken from `DmHandshake::Init.sender`.
    fn handle_handshake(
        &self,
        peer_signing_pubkey: &str,
        noise_message: Vec<u8>,
    ) -> anyhow::Result<Vec<u8>> {
        log::info!(
            "[dm] handling handshake from {}",
            short_id(peer_signing_pubkey)
        );

        // Noise IK responder
        let (responder_hs, response_msg) = noise_respond(&self.my_x25519_private, &noise_message)
            .map_err(|e| anyhow::anyhow!("noise respond: {e}"))?;

        let shared_secret = noise_complete_responder(responder_hs)
            .map_err(|e| anyhow::anyhow!("noise complete: {e}"))?;

        // Initialize Double Ratchet as Bob (responder)
        let ratchet = RatchetState::init_bob(
            &shared_secret,
            (self.my_x25519_private, self.my_x25519_public),
        );

        // Save session keyed by peer's signing pubkey
        let json = serde_json::to_string(&ratchet)?;
        let sealed = seal_ratchet_state(&self.ratchet_storage_key, &json)?;
        self.storage
            .save_ratchet_session(peer_signing_pubkey, &sealed, now_millis())?;

        log::info!(
            "[dm] session established with {}",
            short_id(peer_signing_pubkey)
        );

        let resp = DmHandshake::Response {
            noise_message: response_msg,
        };
        Ok(serde_json::to_vec(&resp)?)
    }

    /// Handle an incoming encrypted message.
    /// `peer_signing_pubkey` is taken from `EncryptedEnvelope.sender`.
    fn handle_encrypted_message(
        &self,
        peer_signing_pubkey: &str,
        envelope: EncryptedEnvelope,
    ) -> anyhow::Result<()> {
        log::debug!(
            "[dm] received envelope: sender={} msg_n={} prev_chain={} dh={} ciphertext_len={}",
            short_id(&envelope.sender),
            envelope.ratchet_header.message_number,
            envelope.ratchet_header.previous_chain_length,
            &envelope.ratchet_header.dh_public[..8],
            envelope.ciphertext.len(),
        );
        // Load and decrypt ratchet session (keyed by signing pubkey)
        let stored = self
            .storage
            .get_ratchet_session(peer_signing_pubkey)?
            .ok_or_else(|| anyhow::anyhow!("no session with {}", short_id(peer_signing_pubkey)))?;
        let json = open_ratchet_state(&self.ratchet_storage_key, &stored)?;
        let mut ratchet: RatchetState = serde_json::from_str(&json)?;

        // Convert wire header to crypto header
        let header = wire_to_ratchet_header(&envelope.ratchet_header)?;

        // Decrypt
        let plaintext = ratchet
            .decrypt(&header, &envelope.ciphertext)
            .map_err(|e| anyhow::anyhow!("decrypt: {e}"))?;

        // Serialize updated ratchet state (encrypted)
        let ratchet_json = serde_json::to_string(&ratchet)?;
        let ratchet_sealed = seal_ratchet_state(&self.ratchet_storage_key, &ratchet_json)?;
        let ratchet_ts = now_millis();

        // Parse payload
        let payload: DmPayload = serde_json::from_slice(&plaintext)?;

        match payload {
            DmPayload::Message(msg) => {
                // Master pubkey is required to route the message to the right conversation.
                // If we can't resolve it, we can't store the message — fail without saving
                // ratchet state so the session is not silently corrupted.
                let peer_master_pubkey = self
                    .storage
                    .get_master_pubkey_for_signing_pubkey(peer_signing_pubkey)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "no master pubkey for signing key {}",
                            short_id(peer_signing_pubkey)
                        )
                    })?;

                let conv_id =
                    Storage::conversation_id(&self.my_master_pubkey_str, &peer_master_pubkey);
                let preview = if msg.content.len() > 80 {
                    format!("{}...", &msg.content[..77])
                } else {
                    msg.content.clone()
                };

                let stored_msg = StoredMessage {
                    id: msg.id.clone(),
                    conversation_id: conv_id.clone(),
                    from_pubkey: peer_master_pubkey.clone(),
                    to_pubkey: self.my_master_pubkey_str.clone(),
                    content: msg.content,
                    timestamp: msg.timestamp,
                    media: msg.media,
                    read: false,
                    delivered: true,
                    reply_to: msg.reply_to,
                };

                // Atomically save ratchet state + store message (single SQLite transaction)
                self.storage.receive_dm_message_atomically(
                    peer_signing_pubkey,
                    &peer_master_pubkey,
                    &ratchet_sealed,
                    ratchet_ts,
                    &stored_msg,
                    &preview,
                )?;

                log::info!(
                    "[dm] received message from {}",
                    short_id(&peer_master_pubkey)
                );

                let _ = self.app_handle.emit(
                    "dm-received",
                    serde_json::json!({
                        "from": peer_master_pubkey,
                        "message": stored_msg,
                    }),
                );
            }
            DmPayload::Delivered { message_id } => {
                self.storage.save_ratchet_session(
                    peer_signing_pubkey,
                    &ratchet_sealed,
                    ratchet_ts,
                )?;
                self.storage.mark_dm_delivered(&message_id)?;
                let _ = self.app_handle.emit(
                    "dm-delivered",
                    serde_json::json!({ "message_id": message_id }),
                );
            }
            DmPayload::Read { message_id } => {
                self.storage.save_ratchet_session(
                    peer_signing_pubkey,
                    &ratchet_sealed,
                    ratchet_ts,
                )?;
                self.storage.mark_dm_read_by_id(&message_id)?;
                let _ = self
                    .app_handle
                    .emit("dm-read", serde_json::json!({ "message_id": message_id }));
            }
            DmPayload::Typing => {
                self.storage.save_ratchet_session(
                    peer_signing_pubkey,
                    &ratchet_sealed,
                    ratchet_ts,
                )?;
                // Best-effort: if master pubkey isn't cached, skip the event
                if let Some(peer_master_pubkey) = self
                    .storage
                    .get_master_pubkey_for_signing_pubkey(peer_signing_pubkey)
                {
                    let _ = self.app_handle.emit(
                        "typing-indicator",
                        serde_json::json!({ "peer": peer_master_pubkey }),
                    );
                }
            }
        }

        Ok(())
    }
}

impl ProtocolHandler for DmHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();
        log::info!("[dm] incoming connection from {}", short_id(&remote_str));

        // Reject blocked peers
        if self.storage.is_blocked(&remote_str).unwrap_or(false) {
            log::warn!("[dm] rejecting blocked peer {}", short_id(&remote_str));
            return Err(AcceptError::from_err(std::io::Error::other("blocked")));
        }

        let (mut send, mut recv) = conn.accept_bi().await?;

        let frame_bytes = recv
            .read_to_end(1_048_576)
            .await
            .map_err(AcceptError::from_err)?;

        // Try handshake first, then encrypted message.
        // In both cases, sender = peer's signing pubkey (the DM session key).
        if let Ok(handshake) = serde_json::from_slice::<DmHandshake>(&frame_bytes) {
            match handshake {
                DmHandshake::Init {
                    noise_message,
                    sender,
                } => {
                    let response = self
                        .handle_handshake(&sender, noise_message)
                        .map_err(|e| AcceptError::from_err(std::io::Error::other(e)))?;
                    send.write_all(&response)
                        .await
                        .map_err(AcceptError::from_err)?;
                    send.finish().map_err(AcceptError::from_err)?;
                }
                DmHandshake::Response { .. } => {
                    log::error!(
                        "[dm] unexpected handshake response from {}",
                        short_id(&remote_str)
                    );
                }
            }
        } else if let Ok(envelope) = serde_json::from_slice::<EncryptedEnvelope>(&frame_bytes) {
            // envelope.sender is the peer's signing pubkey
            let sender = envelope.sender.clone();
            if let Err(e) = self.handle_encrypted_message(&sender, envelope) {
                log::error!(
                    "[dm] failed to handle message from {}: {e}",
                    short_id(&remote_str)
                );
            }
            // Send ACK
            send.write_all(b"ok").await.map_err(AcceptError::from_err)?;
            send.finish().map_err(AcceptError::from_err)?;
        } else {
            log::error!("[dm] unknown frame from {}", short_id(&remote_str));
        }

        conn.closed().await;
        Ok(())
    }
}

// -- Ratchet state encryption at rest --

/// Encrypt a ratchet state JSON string with ChaCha20Poly1305.
/// Returns base64(nonce || ciphertext).
fn seal_ratchet_state(key: &[u8; 32], plaintext: &str) -> anyhow::Result<String> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).expect("getrandom failed");
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| anyhow::anyhow!("ratchet state encryption failed"))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
}

/// Decrypt a ratchet state stored by `seal_ratchet_state`.
/// Falls back to treating the input as plaintext JSON for migration of existing sessions.
fn open_ratchet_state(key: &[u8; 32], stored: &str) -> anyhow::Result<String> {
    if let Ok(blob) = base64::engine::general_purpose::STANDARD.decode(stored)
        && blob.len() > 12
    {
        let (nonce_bytes, ciphertext) = blob.split_at(12);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
        let nonce = Nonce::from_slice(nonce_bytes);
        if let Ok(plaintext_bytes) = cipher.decrypt(nonce, ciphertext)
            && let Ok(s) = String::from_utf8(plaintext_bytes)
        {
            return Ok(s);
        }
    }
    // Migration path: treat as legacy plaintext JSON
    if serde_json::from_str::<serde_json::Value>(stored).is_ok() {
        log::info!("[dm] migrating legacy plaintext ratchet session to encrypted storage");
        return Ok(stored.to_string());
    }
    Err(anyhow::anyhow!("failed to open ratchet state"))
}

// -- Helper functions for header conversion --

fn ratchet_header_to_wire(header: &RatchetHeader) -> RatchetHeaderWire {
    RatchetHeaderWire {
        dh_public: hex::encode(header.dh_public),
        message_number: header.message_number,
        previous_chain_length: header.previous_chain_length,
    }
}

fn wire_to_ratchet_header(wire: &RatchetHeaderWire) -> anyhow::Result<RatchetHeader> {
    let bytes = hex::decode(&wire.dh_public)?;
    let dh_public: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid dh_public length"))?;
    Ok(RatchetHeader {
        dh_public,
        message_number: wire.message_number,
        previous_chain_length: wire.previous_chain_length,
    })
}
