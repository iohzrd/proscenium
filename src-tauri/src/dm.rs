use crate::constants::OUTBOX_FLUSH_INTERVAL;
use crate::crypto::{
    RatchetHeader, RatchetState, noise_complete_initiator, noise_complete_responder,
    noise_initiate, noise_respond,
};
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use base64::Engine as _;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, aead::Aead};
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_social_types::{
    DM_ALPN, DirectMessage, DmAck, DmHandshake, DmMessage, DmPayload, EncryptedEnvelope,
    RatchetHeaderWire, StoredMessage, now_millis, short_id,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct DmHandler {
    storage: Arc<Storage>,
    app_handle: AppHandle,
    endpoint: Endpoint,
    identity: SharedIdentity,
    outbox_notify: Arc<Notify>,
}

impl DmHandler {
    pub fn new(
        storage: Arc<Storage>,
        app_handle: AppHandle,
        endpoint: Endpoint,
        identity: SharedIdentity,
    ) -> Self {
        Self {
            storage,
            app_handle,
            endpoint,
            identity,
            outbox_notify: Arc::new(Notify::new()),
        }
    }

    /// Spawn the outbox flush loop. Select on immediate notify (from send_dm on peer-offline)
    /// or the periodic fallback timer so no messages are stranded indefinitely.
    pub fn start_background(&self, token: CancellationToken) {
        let handler = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = handler.outbox_notify.notified() => {}
                    _ = tokio::time::sleep(OUTBOX_FLUSH_INTERVAL) => {}
                }
                handler.flush_all_outbox().await;
            }
            log::info!("[dm] outbox flush loop stopped");
        });
    }

    /// Attempt delivery of all queued outbox messages. Entries that succeed are removed;
    /// entries that fail are left for the next flush.
    async fn flush_all_outbox(&self) {
        let entries = match self.storage.get_all_outbox_messages().await {
            Ok(e) => e,
            Err(e) => {
                log::error!("[dm] failed to read outbox: {e}");
                return;
            }
        };
        for (id, peer_pubkey, envelope_json, message_id) in entries {
            let envelope: EncryptedEnvelope = match serde_json::from_str(&envelope_json) {
                Ok(e) => e,
                Err(e) => {
                    log::error!("[dm] invalid outbox entry {}: {e}", short_id(&id));
                    continue;
                }
            };
            match self.try_send_envelope(&peer_pubkey, &envelope).await {
                Ok(()) => {
                    log::info!(
                        "[dm] outbox delivered to {}, removing entry",
                        short_id(&peer_pubkey)
                    );
                    if let Err(e) = self.storage.remove_outbox_message(&id).await {
                        log::error!("[dm] failed to remove outbox entry {}: {e}", short_id(&id));
                    }
                    self.mark_delivered(&message_id).await;
                }
                Err(e) => {
                    log::debug!(
                        "[dm] outbox flush: {} still offline: {e}",
                        short_id(&peer_pubkey)
                    );
                }
            }
        }
    }

    /// Get or establish a ratchet session with a peer.
    /// Returns `(RatchetState, peer_dm_pubkey)` -- callers must use `peer_dm_pubkey`
    /// as the key for any subsequent `save_ratchet_session` calls.
    async fn get_or_establish_session(
        &self,
        peer_master_pubkey: &str,
    ) -> Result<(RatchetState, String), AppError> {
        log::info!(
            "[dm] get_or_establish_session: peer={}",
            short_id(peer_master_pubkey)
        );

        // Resolve the peer's DM pubkey from cached delegation.
        let peer_dm_pubkey = self
            .storage
            .get_peer_dm_pubkey(peer_master_pubkey)
            .await?
            .ok_or_else(|| {
                AppError::Other(format!(
                    "no DM pubkey cached for {}",
                    short_id(peer_master_pubkey)
                ))
            })?;

        // Read all identity fields needed before any await points.
        let (my_x25519_private, my_dm_pubkey, ratchet_storage_key) = {
            let id = self.identity.read().await;
            (
                id.dm_x25519_private,
                id.dm_pubkey.clone(),
                id.ratchet_storage_key,
            )
        };

        // Try loading existing session (keyed by DM pubkey)
        if let Some(stored) = self.storage.get_ratchet_session(&peer_dm_pubkey).await? {
            log::info!(
                "[dm] loaded existing ratchet session for {}",
                short_id(peer_master_pubkey),
            );
            let json = open_ratchet_state(&ratchet_storage_key, &stored)?;
            let state: RatchetState = serde_json::from_str(&json)?;
            return Ok((state, peer_dm_pubkey));
        }

        // No session -- need to handshake
        log::info!(
            "[dm] no existing session, initiating Noise IK handshake with {}",
            short_id(peer_master_pubkey)
        );

        // Fail fast: resolve transport NodeId before doing any crypto work
        let node_ids = self
            .storage
            .get_peer_transport_node_ids(peer_master_pubkey)
            .await?;
        let transport_id_str = node_ids.into_iter().next().ok_or_else(|| {
            AppError::Other(format!(
                "no transport NodeId for {}",
                short_id(peer_master_pubkey)
            ))
        })?;
        let transport_id: EndpointId = transport_id_str.parse()?;
        let addr = EndpointAddr::from(transport_id);

        // Peer's DM pubkey IS already X25519 -- just hex-decode it
        let peer_x25519_public = dm_pubkey_to_x25519(&peer_dm_pubkey)?;
        log::info!("[dm] resolved peer DM X25519 key");

        // Noise IK handshake: initiator
        let (initiator_hs, msg1) = noise_initiate(&my_x25519_private, &peer_x25519_public)?;
        log::info!("[dm] noise init message created ({} bytes)", msg1.len());

        log::info!(
            "[dm] connecting to {} on DM_ALPN...",
            short_id(peer_master_pubkey)
        );
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.endpoint.connect(addr, DM_ALPN),
        )
        .await
        .map_err(|_| {
            AppError::Other(format!(
                "handshake connect timeout to {}",
                short_id(peer_master_pubkey)
            ))
        })?
        .inspect_err(|e| {
            log::error!(
                "[dm] QUIC connect failed to {}: {e}",
                short_id(peer_master_pubkey)
            );
        })?;
        log::info!("[dm] QUIC connected, opening bi-stream...");
        let (mut send, mut recv) = conn.open_bi().await?;

        // Send handshake init -- sender is our DM pubkey
        let handshake = DmHandshake::Init {
            noise_message: msg1,
            sender: my_dm_pubkey,
        };
        let bytes = serde_json::to_vec(&DmMessage::Handshake(handshake))?;
        log::info!("[dm] sending handshake init ({} bytes)...", bytes.len());
        send.write_all(&bytes).await?;
        send.finish()?;

        // Read handshake response (acceptor sends DmHandshake::Response directly)
        log::info!("[dm] waiting for handshake response...");
        let resp_bytes = recv.read_to_end(65536).await?;
        log::info!(
            "[dm] received handshake response ({} bytes)",
            resp_bytes.len()
        );
        let resp: DmHandshake = serde_json::from_slice(&resp_bytes)?;

        let noise_response = match resp {
            DmHandshake::Response { noise_message } => noise_message,
            _ => return Err(AppError::Other("unexpected handshake response".into())),
        };

        // Complete handshake
        let shared_secret = noise_complete_initiator(initiator_hs, &noise_response)?;
        log::info!("[dm] noise handshake completed successfully");

        conn.close(0u32.into(), b"done");

        // Initialize Double Ratchet as Alice (initiator)
        let ratchet = RatchetState::init_alice(&shared_secret, &peer_x25519_public);

        // Encrypt and save session keyed by peer's DM pubkey
        let json = serde_json::to_string(&ratchet)?;
        let sealed = seal_ratchet_state(&ratchet_storage_key, &json)?;
        self.storage
            .save_ratchet_session(&peer_dm_pubkey, &sealed, now_millis())
            .await?;

        log::info!(
            "[dm] established and saved ratchet session with {}",
            short_id(peer_master_pubkey)
        );
        Ok((ratchet, peer_dm_pubkey))
    }

    /// Encrypt a payload with Double Ratchet and save the updated ratchet state.
    /// Returns the sealed envelope ready for transmission.
    async fn encrypt_and_save(
        &self,
        peer_pubkey: &str,
        payload: &DmPayload,
    ) -> Result<EncryptedEnvelope, AppError> {
        let (mut ratchet, peer_dm_pubkey) = self.get_or_establish_session(peer_pubkey).await?;

        let plaintext = serde_json::to_vec(payload)?;
        let (header, ciphertext) = ratchet.encrypt(&plaintext);

        let (dm_pubkey, ratchet_storage_key) = {
            let id = self.identity.read().await;
            (id.dm_pubkey.clone(), id.ratchet_storage_key)
        };

        let ratchet_json = serde_json::to_string(&ratchet)?;
        let ratchet_sealed = seal_ratchet_state(&ratchet_storage_key, &ratchet_json)?;
        self.storage
            .save_ratchet_session(&peer_dm_pubkey, &ratchet_sealed, now_millis())
            .await?;

        let envelope = EncryptedEnvelope {
            sender: dm_pubkey,
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

        Ok(envelope)
    }

    /// Send a DM to a peer. Encrypts with Double Ratchet and sends over QUIC.
    /// If the peer is offline, queues to outbox.
    /// On successful delivery, marks the message as delivered and emits `dm-delivered`.
    pub async fn send_dm(&self, peer_pubkey: &str, message: DirectMessage) -> Result<(), AppError> {
        let message_id = message.id.clone();
        let payload = DmPayload::Message(message);
        let envelope = self.encrypt_and_save(peer_pubkey, &payload).await?;

        match self.try_send_envelope(peer_pubkey, &envelope).await {
            Ok(()) => {
                log::info!("[dm] sent message to {}", short_id(peer_pubkey));
                self.mark_delivered(&message_id).await;
            }
            Err(e) => {
                log::warn!(
                    "[dm] peer {} offline, queuing to outbox: {e}",
                    short_id(peer_pubkey)
                );
                let envelope_json = serde_json::to_string(&envelope)?;
                let id = crate::util::generate_id();
                self.storage
                    .insert_outbox_message(
                        &id,
                        peer_pubkey,
                        &envelope_json,
                        now_millis(),
                        &message_id,
                    )
                    .await?;
                self.outbox_notify.notify_one();
            }
        }

        Ok(())
    }

    /// Mark a message as delivered in storage and notify the frontend.
    async fn mark_delivered(&self, message_id: &str) {
        if let Err(e) = self.storage.mark_dm_delivered(message_id).await {
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
        peer_master_pubkey: &str,
        envelope: &EncryptedEnvelope,
    ) -> Result<(), AppError> {
        let node_ids = self
            .storage
            .get_peer_transport_node_ids(peer_master_pubkey)
            .await?;
        let transport_id_str = node_ids.into_iter().next().ok_or_else(|| {
            AppError::Other(format!(
                "no transport NodeId for {}",
                short_id(peer_master_pubkey)
            ))
        })?;
        let transport_id: EndpointId = transport_id_str.parse()?;
        let addr = EndpointAddr::from(transport_id);

        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.endpoint.connect(addr, DM_ALPN),
        )
        .await
        .map_err(|_| AppError::Other("connection timeout".into()))??;

        let (mut send, mut recv) = conn.open_bi().await?;

        let bytes = serde_json::to_vec(&DmMessage::Envelope(envelope.clone()))?;
        send.write_all(&bytes).await?;
        send.finish()?;

        // Wait for ACK
        let ack_bytes =
            tokio::time::timeout(std::time::Duration::from_secs(5), recv.read_to_end(256))
                .await
                .map_err(|_| AppError::Other("ack timeout".into()))??;
        serde_json::from_slice::<DmAck>(&ack_bytes)?;

        conn.close(0u32.into(), b"done");
        Ok(())
    }

    /// Send a lightweight DM signal (typing, read receipt) without storing a message.
    pub async fn send_signal(&self, peer_pubkey: &str, payload: DmPayload) -> Result<(), AppError> {
        let envelope = self.encrypt_and_save(peer_pubkey, &payload).await?;
        self.try_send_envelope(peer_pubkey, &envelope).await?;
        Ok(())
    }

    /// Handle an incoming handshake (Noise IK responder side).
    /// `peer_dm_pubkey` is taken from `DmHandshake::Init.sender`.
    async fn handle_handshake(
        &self,
        peer_dm_pubkey: &str,
        noise_message: Vec<u8>,
    ) -> Result<Vec<u8>, AppError> {
        log::info!("[dm] handling handshake from {}", short_id(peer_dm_pubkey));

        let (my_x25519_private, my_x25519_public, ratchet_storage_key) = {
            let id = self.identity.read().await;
            (
                id.dm_x25519_private,
                id.dm_x25519_public,
                id.ratchet_storage_key,
            )
        };

        // Noise IK responder
        let (responder_hs, response_msg) = noise_respond(&my_x25519_private, &noise_message)?;

        let (shared_secret, initiator_dm_pubkey) = noise_complete_responder(responder_hs)?;

        // Verify the Noise IK-authenticated initiator DM key matches the claimed sender.
        // Noise IK encrypts the initiator's long-term key to us, so this is cryptographically
        // authenticated -- not just a wire claim.
        let claimed_key = dm_pubkey_to_x25519(peer_dm_pubkey)?;
        match initiator_dm_pubkey {
            Some(actual) if actual == claimed_key => {}
            Some(_) => {
                return Err(AppError::Other(
                    "DmHandshake::Init sender mismatch: claimed key does not match authenticated key".into(),
                ));
            }
            None => {
                return Err(AppError::Other(
                    "Noise IK handshake did not reveal initiator key".into(),
                ));
            }
        }

        // Initialize Double Ratchet as Bob (responder)
        let ratchet = RatchetState::init_bob(&shared_secret, (my_x25519_private, my_x25519_public));

        // Save session keyed by peer's DM pubkey
        let json = serde_json::to_string(&ratchet)?;
        let sealed = seal_ratchet_state(&ratchet_storage_key, &json)?;
        self.storage
            .save_ratchet_session(peer_dm_pubkey, &sealed, now_millis())
            .await?;

        log::info!("[dm] session established with {}", short_id(peer_dm_pubkey));

        let resp = DmHandshake::Response {
            noise_message: response_msg,
        };
        Ok(serde_json::to_vec(&resp)?)
    }

    /// Handle an incoming encrypted message.
    /// `peer_dm_pubkey` is taken from `EncryptedEnvelope.sender`.
    async fn handle_encrypted_message(
        &self,
        peer_dm_pubkey: &str,
        envelope: EncryptedEnvelope,
    ) -> Result<(), AppError> {
        log::debug!(
            "[dm] received envelope: sender={} msg_n={} prev_chain={} dh={} ciphertext_len={}",
            short_id(&envelope.sender),
            envelope.ratchet_header.message_number,
            envelope.ratchet_header.previous_chain_length,
            &envelope.ratchet_header.dh_public[..8],
            envelope.ciphertext.len(),
        );
        let (ratchet_storage_key, my_master_pubkey) = {
            let id = self.identity.read().await;
            (id.ratchet_storage_key, id.master_pubkey.clone())
        };

        // Load and decrypt ratchet session (keyed by DM pubkey)
        let stored = self
            .storage
            .get_ratchet_session(peer_dm_pubkey)
            .await?
            .ok_or_else(|| {
                AppError::Other(format!("no session with {}", short_id(peer_dm_pubkey)))
            })?;
        let json = open_ratchet_state(&ratchet_storage_key, &stored)?;
        let mut ratchet: RatchetState = serde_json::from_str(&json)?;

        // Convert wire header to crypto header
        let header = wire_to_ratchet_header(&envelope.ratchet_header)?;

        // Decrypt
        let plaintext = ratchet.decrypt(&header, &envelope.ciphertext)?;

        // Serialize updated ratchet state (encrypted)
        let ratchet_json = serde_json::to_string(&ratchet)?;
        let ratchet_sealed = seal_ratchet_state(&ratchet_storage_key, &ratchet_json)?;
        let ratchet_ts = now_millis();

        // Parse payload
        let payload: DmPayload = serde_json::from_slice(&plaintext)?;

        match payload {
            DmPayload::Message(msg) => {
                // Master pubkey is required to route the message to the right conversation.
                let peer_master_pubkey = self
                    .storage
                    .get_master_pubkey_for_dm_pubkey(peer_dm_pubkey)
                    .await
                    .ok_or_else(|| {
                        AppError::Other(format!(
                            "no master pubkey for DM key {}",
                            short_id(peer_dm_pubkey)
                        ))
                    })?;

                let conv_id = Storage::conversation_id(&my_master_pubkey, &peer_master_pubkey);
                let preview = if msg.content.len() > 80 {
                    format!("{}...", &msg.content[..77])
                } else {
                    msg.content.clone()
                };

                let stored_msg = StoredMessage {
                    id: msg.id.clone(),
                    conversation_id: conv_id.clone(),
                    from_pubkey: peer_master_pubkey.clone(),
                    to_pubkey: my_master_pubkey.clone(),
                    content: msg.content,
                    timestamp: msg.timestamp,
                    media: msg.media,
                    read: false,
                    delivered: true,
                    reply_to: msg.reply_to,
                };

                // Atomically save ratchet state + store message (single SQLite transaction)
                self.storage
                    .receive_dm_message_atomically(
                        peer_dm_pubkey,
                        &peer_master_pubkey,
                        &ratchet_sealed,
                        ratchet_ts,
                        &stored_msg,
                        &preview,
                    )
                    .await?;

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
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                self.storage.mark_dm_delivered(&message_id).await?;
                let _ = self.app_handle.emit(
                    "dm-delivered",
                    serde_json::json!({ "message_id": message_id }),
                );
            }
            DmPayload::Read { message_id } => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                self.storage.mark_dm_read_by_id(&message_id).await?;
                let _ = self
                    .app_handle
                    .emit("dm-read", serde_json::json!({ "message_id": message_id }));
            }
            DmPayload::Typing => {
                self.storage
                    .save_ratchet_session(peer_dm_pubkey, &ratchet_sealed, ratchet_ts)
                    .await?;
                if let Some(peer_master_pubkey) = self
                    .storage
                    .get_master_pubkey_for_dm_pubkey(peer_dm_pubkey)
                    .await
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

        // Resolve transport NodeId to master pubkey for block check
        let remote_pubkey = self
            .storage
            .get_master_pubkey_for_transport(&remote_str)
            .await
            .unwrap_or_else(|| remote_str.clone());

        // Reject blocked peers
        if self
            .storage
            .is_blocked(&remote_pubkey)
            .await
            .unwrap_or(false)
        {
            log::warn!("[dm] rejecting blocked peer {}", short_id(&remote_pubkey));
            return Err(AcceptError::from_err(std::io::Error::other("blocked")));
        }

        let (mut send, mut recv) = conn.accept_bi().await?;

        let frame_bytes = recv
            .read_to_end(1_048_576)
            .await
            .map_err(AcceptError::from_err)?;

        let msg: DmMessage = serde_json::from_slice(&frame_bytes).map_err(AcceptError::from_err)?;

        match msg {
            DmMessage::Handshake(DmHandshake::Init {
                noise_message,
                sender,
            }) => {
                let response = self
                    .handle_handshake(&sender, noise_message)
                    .await
                    .map_err(AcceptError::from_err)?;
                send.write_all(&response)
                    .await
                    .map_err(AcceptError::from_err)?;
                send.finish().map_err(AcceptError::from_err)?;
            }
            DmMessage::Handshake(DmHandshake::Response { .. }) => {
                log::error!(
                    "[dm] unexpected handshake response from {}",
                    short_id(&remote_str)
                );
            }
            DmMessage::Envelope(envelope) => {
                let sender = envelope.sender.clone();
                if let Err(e) = self.handle_encrypted_message(&sender, envelope).await {
                    log::error!(
                        "[dm] failed to handle message from {}: {e}",
                        short_id(&remote_str)
                    );
                }
                let ack = serde_json::to_vec(&DmAck).map_err(AcceptError::from_err)?;
                send.write_all(&ack).await.map_err(AcceptError::from_err)?;
                send.finish().map_err(AcceptError::from_err)?;
            }
        }

        conn.closed().await;
        Ok(())
    }
}

// -- Ratchet state encryption at rest --

/// Encrypt a ratchet state JSON string with ChaCha20Poly1305.
/// Returns base64(nonce || ciphertext).
fn seal_ratchet_state(key: &[u8; 32], plaintext: &str) -> Result<String, AppError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).expect("getrandom failed");
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| AppError::Other("ratchet state encryption failed".into()))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
}

/// Decrypt a ratchet state stored by `seal_ratchet_state`.
fn open_ratchet_state(key: &[u8; 32], stored: &str) -> Result<String, AppError> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(stored)
        .map_err(|_| AppError::Other("invalid base64 in ratchet state".into()))?;
    if blob.len() <= 12 {
        return Err(AppError::Other("ratchet state too short".into()));
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext_bytes = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| AppError::Other("ratchet state decryption failed".into()))?;
    String::from_utf8(plaintext_bytes)
        .map_err(|_| AppError::Other("ratchet state not valid UTF-8".into()))
}

// -- Helper functions --

/// Decode a hex-encoded DM pubkey (X25519) to 32 bytes.
fn dm_pubkey_to_x25519(hex_pubkey: &str) -> Result<[u8; 32], AppError> {
    let bytes = hex::decode(hex_pubkey)?;
    bytes
        .try_into()
        .map_err(|_| AppError::Other("DM pubkey wrong length".into()))
}

fn ratchet_header_to_wire(header: &RatchetHeader) -> RatchetHeaderWire {
    RatchetHeaderWire {
        dh_public: hex::encode(header.dh_public),
        message_number: header.message_number,
        previous_chain_length: header.previous_chain_length,
    }
}

fn wire_to_ratchet_header(wire: &RatchetHeaderWire) -> Result<RatchetHeader, AppError> {
    let bytes = hex::decode(&wire.dh_public)?;
    let dh_public: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AppError::Other("invalid dh_public length".into()))?;
    Ok(RatchetHeader {
        dh_public,
        message_number: wire.message_number,
        previous_chain_length: wire.previous_chain_length,
    })
}
