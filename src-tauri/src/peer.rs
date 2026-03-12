use crate::state::PendingLinkState;
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_social_types::{
    FollowRequest, FollowResponse, IdentityResponse, PEER_ALPN, PeerRequest, PeerResponse,
    SigningKeyDelegation, Visibility, derive_transport_key, now_millis, short_id,
    sign_follow_request, verify_follow_request,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

#[derive(Debug, Clone)]
pub struct PeerHandler {
    storage: Arc<Storage>,
    /// The permanent identity (master public key).
    master_pubkey: String,
    /// The transport NodeId (iroh's own key).
    transport_node_id: String,
    /// The current signing key delegation.
    delegation: SigningKeyDelegation,
    /// Master secret key bytes (needed for deriving transport keys during pairing).
    master_secret_key_bytes: [u8; 32],
    /// Signing secret key bytes (transferred during pairing).
    signing_secret_key_bytes: [u8; 32],
    /// DM secret key bytes (transferred during pairing).
    dm_secret_key_bytes: [u8; 32],
    /// Active device-linking session (if any).
    pending_link: PendingLinkState,
    app_handle: AppHandle,
}

impl PeerHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        storage: Arc<Storage>,
        master_pubkey: String,
        transport_node_id: String,
        delegation: SigningKeyDelegation,
        master_secret_key_bytes: [u8; 32],
        signing_secret_key_bytes: [u8; 32],
        dm_secret_key_bytes: [u8; 32],
        pending_link: PendingLinkState,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            storage,
            master_pubkey,
            transport_node_id,
            delegation,
            master_secret_key_bytes,
            signing_secret_key_bytes,
            dm_secret_key_bytes,
            pending_link,
            app_handle,
        }
    }

    /// Build an IdentityResponse for this node.
    async fn identity_response(&self) -> IdentityResponse {
        let profile = self
            .storage
            .get_profile(&self.master_pubkey)
            .await
            .ok()
            .flatten();
        IdentityResponse {
            master_pubkey: self.master_pubkey.clone(),
            delegation: self.delegation.clone(),
            transport_node_ids: vec![self.transport_node_id.clone()],
            profile,
        }
    }

    async fn handle_follow_request(
        &self,
        remote_str: &str,
        mut send: iroh::endpoint::SendStream,
        req: FollowRequest,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        // Verify the delegation (master key signed the signing key binding)
        if let Err(reason) = iroh_social_types::verify_delegation(&req.delegation) {
            log::warn!(
                "[follow-req] invalid delegation from {}: {reason}",
                short_id(remote_str)
            );
            return Err(AcceptError::from_err(std::io::Error::other(
                "invalid delegation",
            )));
        }

        // Check that the delegation's master pubkey matches the requester
        if req.delegation.master_pubkey != req.requester {
            log::warn!(
                "[follow-req] delegation master_pubkey mismatch: delegation={}, requester={}",
                short_id(&req.delegation.master_pubkey),
                short_id(&req.requester)
            );
            return Err(AcceptError::from_err(std::io::Error::other(
                "delegation master_pubkey mismatch",
            )));
        }

        // Verify signature using the signing key from the delegation
        let signer: iroh::PublicKey = match req.delegation.signing_pubkey.parse() {
            Ok(pk) => pk,
            Err(e) => {
                log::warn!(
                    "[follow-req] invalid signing pubkey in delegation from {}: {e}",
                    short_id(remote_str)
                );
                return Err(AcceptError::from_err(std::io::Error::other(
                    "invalid signing pubkey in delegation",
                )));
            }
        };
        if let Err(reason) = verify_follow_request(&req, &signer) {
            log::warn!(
                "[follow-req] invalid signature from {}: {reason}",
                short_id(remote_str)
            );
            return Err(AcceptError::from_err(std::io::Error::other(
                "invalid signature",
            )));
        }

        // Cache the peer's delegation and transport NodeId
        let _ = self
            .storage
            .cache_peer_identity(&IdentityResponse {
                master_pubkey: req.requester.clone(),
                delegation: req.delegation.clone(),
                transport_node_ids: vec![remote_str.to_string()],
                profile: None,
            })
            .await;

        // Use master pubkey (permanent identity) for all storage lookups, not transport NodeId
        let requester_pubkey = &req.requester;

        // Only Listed users accept follow requests
        let visibility = self
            .storage
            .get_visibility(&self.master_pubkey)
            .await
            .unwrap_or(Visibility::Public);

        let response = match visibility {
            Visibility::Listed => {
                // Check if already approved
                if self
                    .storage
                    .is_approved_follower(requester_pubkey)
                    .await
                    .unwrap_or(false)
                {
                    log::info!(
                        "[follow-req] already approved: {}",
                        short_id(requester_pubkey)
                    );
                    FollowResponse::Approved(Box::new(self.identity_response().await))
                } else {
                    let now = now_millis();
                    let expires_at = now + FOLLOW_REQUEST_TTL_MS;
                    match self
                        .storage
                        .insert_follow_request(requester_pubkey, req.timestamp, now, expires_at)
                        .await
                    {
                        Ok(true) => {
                            log::info!(
                                "[follow-req] stored pending request from {}",
                                short_id(requester_pubkey)
                            );
                            let _ = self
                                .storage
                                .insert_notification(
                                    "follow_request",
                                    requester_pubkey,
                                    None,
                                    None,
                                    now,
                                )
                                .await;
                            let _ = self
                                .app_handle
                                .emit("follow-request-received", requester_pubkey);
                            let _ = self.app_handle.emit("notification-received", ());
                            FollowResponse::Pending
                        }
                        Ok(false) => {
                            // Already approved (race condition)
                            FollowResponse::Approved(Box::new(self.identity_response().await))
                        }
                        Err(e) => {
                            log::error!("[follow-req] failed to store request: {e}");
                            return Err(AcceptError::from_err(std::io::Error::other(
                                "storage error",
                            )));
                        }
                    }
                }
            }
            Visibility::Public => {
                // Public users don't need follow requests, auto-approve
                log::info!(
                    "[follow-req] auto-approving for public profile: {}",
                    short_id(requester_pubkey)
                );
                FollowResponse::Approved(Box::new(self.identity_response().await))
            }
            Visibility::Private => {
                // Private users deny follow requests
                log::info!(
                    "[follow-req] denying for private profile: {}",
                    short_id(requester_pubkey)
                );
                FollowResponse::Denied
            }
        };

        let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
        send.write_all(&resp_bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;

        conn.closed().await;
        Ok(())
    }

    async fn handle_identity_request(
        &self,
        mut send: iroh::endpoint::SendStream,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let response = PeerResponse::Identity(self.identity_response().await);

        let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
        send.write_all(&resp_bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;

        conn.closed().await;
        Ok(())
    }

    async fn handle_link_request(
        &self,
        mut send: iroh::endpoint::SendStream,
        noise_init: Vec<u8>,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        // Take the pending link session (consume it - one-time use)
        let link_session = {
            let mut lock = self.pending_link.lock().await;
            lock.take()
        };

        let link_session = match link_session {
            Some(session) => {
                // Check expiry
                if now_millis() > session.expires_at {
                    log::warn!("[link] pending link session expired");
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "link session expired",
                    )));
                }
                session
            }
            None => {
                log::warn!("[link] no pending link session");
                return Err(AcceptError::from_err(std::io::Error::other(
                    "no pending link session",
                )));
            }
        };

        // Perform Noise IK+PSK handshake (responder side)
        let (mut transport, noise_response) = crate::crypto::noise_psk_respond(
            &link_session.x25519_private,
            &link_session.psk,
            &noise_init,
        )
        .map_err(|e| {
            log::error!("[link] noise handshake failed: {e}");
            AcceptError::from_err(std::io::Error::other(format!(
                "noise handshake failed: {e}"
            )))
        })?;

        // Derive a transport key for the new device
        let new_device_index = self.storage.next_device_index().await.map_err(|e| {
            AcceptError::from_err(std::io::Error::other(format!(
                "failed to get next device index: {e}"
            )))
        })?;
        let new_transport_key_bytes =
            derive_transport_key(&self.master_secret_key_bytes, new_device_index);

        // Build the link bundle
        let master_key_to_send = if link_session.transfer_master_key {
            Some(&self.master_secret_key_bytes)
        } else {
            None
        };

        let bundle = self
            .storage
            .export_link_bundle(
                &self.master_pubkey,
                &self.signing_secret_key_bytes,
                &self.dm_secret_key_bytes,
                &self.delegation,
                &new_transport_key_bytes,
                new_device_index,
                master_key_to_send,
            )
            .await
            .map_err(|e| {
                AcceptError::from_err(std::io::Error::other(format!(
                    "failed to export link bundle: {e}"
                )))
            })?;

        // Serialize and encrypt the bundle with Noise transport
        let bundle_json = serde_json::to_vec(&bundle).map_err(AcceptError::from_err)?;
        let encrypted_bundle = crate::crypto::noise_transport_encrypt(&mut transport, &bundle_json)
            .map_err(|e| {
                AcceptError::from_err(std::io::Error::other(format!("noise encrypt failed: {e}")))
            })?;

        // Send the response
        let response = PeerResponse::LinkBundle {
            noise_response,
            encrypted_bundle,
        };
        let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
        send.write_all(&resp_bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;

        log::info!("[link] sent link bundle to new device");
        let _ = self.app_handle.emit("device-link-progress", "bundle_sent");

        // Register the new device and broadcast updated announcement
        let new_transport_secret = iroh::SecretKey::from_bytes(&new_transport_key_bytes);
        let new_transport_node_id = new_transport_secret.public().to_string();

        let device_name = format!("Device {}", new_device_index);
        let now = now_millis();
        if let Err(e) = self
            .storage
            .upsert_linked_device(&new_transport_node_id, &device_name, false, false, now)
            .await
        {
            log::error!("[link] failed to register new device: {e}");
        } else {
            log::info!(
                "[link] registered new device {} (index={})",
                short_id(&new_transport_node_id),
                new_device_index
            );
        }

        // Build and broadcast updated announcement with all devices
        if let Ok(all_devices) = self.storage.get_linked_devices().await {
            let signing_sk = iroh::SecretKey::from_bytes(&self.signing_secret_key_bytes);
            let mut announcement = iroh_social_types::LinkedDevicesAnnouncement {
                master_pubkey: self.master_pubkey.clone(),
                delegation: self.delegation.clone(),
                devices: all_devices,
                version: (new_device_index + 1) as u64,
                timestamp: now,
                signature: String::new(),
            };
            iroh_social_types::sign_linked_devices_announcement(&mut announcement, &signing_sk);

            if let Some(state) = self.app_handle.try_state::<Arc<crate::state::AppState>>() {
                let feed = state.feed.clone();
                let announcement_clone = announcement.clone();
                tokio::spawn(async move {
                    let feed_lock = feed.lock().await;
                    if let Err(e) = feed_lock
                        .broadcast_linked_devices(&announcement_clone)
                        .await
                    {
                        log::error!("[link] failed to broadcast device announcement: {e}");
                    } else {
                        log::info!("[link] broadcast updated device announcement");
                    }
                });
            }
        }

        conn.closed().await;
        Ok(())
    }
}

const FOLLOW_REQUEST_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000; // 30 days

impl ProtocolHandler for PeerHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();

        // Reject blocked peers
        if self.storage.is_blocked(&remote_str).await.unwrap_or(false) {
            log::warn!("[peer] rejecting blocked peer {}", short_id(&remote_str));
            return Err(AcceptError::from_err(std::io::Error::other("blocked")));
        }

        let (send, mut recv) = conn.accept_bi().await?;

        // Read initial PeerRequest to determine intent
        let req_bytes = recv
            .read_to_end(10_000_000)
            .await
            .map_err(AcceptError::from_err)?;
        let req: PeerRequest = serde_json::from_slice(&req_bytes).map_err(AcceptError::from_err)?;

        log::info!(
            "[peer] {} from {}",
            match &req {
                PeerRequest::Sync(_) => "sync",
                PeerRequest::Push(_) => "push",
                PeerRequest::FollowRequest(_) => "follow-request",
                PeerRequest::IdentityRequest => "identity-request",
                PeerRequest::LinkRequest { .. } => "link-request",
                PeerRequest::DeviceSyncRequest { .. } => "device-sync",
            },
            short_id(&remote_str)
        );

        match req {
            PeerRequest::Sync(sync_req) => {
                crate::sync::handle_sync(
                    &self.storage,
                    &self.master_pubkey,
                    &remote_str,
                    &conn,
                    send,
                    sync_req,
                )
                .await
            }
            PeerRequest::Push(push_msg) => {
                crate::push::handle_push(
                    &self.storage,
                    &self.master_pubkey,
                    &remote_str,
                    &self.app_handle,
                    send,
                    push_msg,
                    &conn,
                )
                .await
            }
            PeerRequest::FollowRequest(follow_req) => {
                self.handle_follow_request(&remote_str, send, follow_req, &conn)
                    .await
            }
            PeerRequest::IdentityRequest => self.handle_identity_request(send, &conn).await,
            PeerRequest::LinkRequest { noise_init } => {
                self.handle_link_request(send, noise_init, &conn).await
            }
            PeerRequest::DeviceSyncRequest {
                challenge,
                challenge_sig,
                vector,
            } => {
                crate::device_sync::handle_device_sync(
                    &self.storage,
                    &self.master_pubkey,
                    &self.signing_secret_key_bytes,
                    send,
                    challenge,
                    challenge_sig,
                    vector,
                    &conn,
                )
                .await
            }
        }
    }
}

/// Client: send a follow request to a remote peer.
/// If approved, the responder's identity is automatically cached.
pub async fn send_follow_request(
    endpoint: &Endpoint,
    storage: &Storage,
    target: EndpointId,
    master_pubkey: &str,
    signing_secret_key_bytes: &[u8; 32],
    delegation: &SigningKeyDelegation,
) -> anyhow::Result<FollowResponse> {
    let timestamp = now_millis();
    let secret_key = iroh::SecretKey::from_bytes(signing_secret_key_bytes);
    let signature = sign_follow_request(master_pubkey, timestamp, &secret_key);

    let req = FollowRequest {
        requester: master_pubkey.to_string(),
        timestamp,
        signature,
        delegation: delegation.clone(),
    };

    let peer_req = PeerRequest::FollowRequest(req);
    let addr = EndpointAddr::from(target);
    let conn = endpoint.connect(addr, PEER_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let req_bytes = serde_json::to_vec(&peer_req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    let resp_bytes = recv.read_to_end(65_536).await?;
    let response: FollowResponse = serde_json::from_slice(&resp_bytes)?;

    conn.close(0u32.into(), b"done");

    // Cache the responder's identity if approved
    if let FollowResponse::Approved(ref identity) = response {
        let _ = storage.cache_peer_identity(identity).await;
    }

    Ok(response)
}

/// Client: query a peer's identity by connecting to their transport NodeId.
/// Returns the IdentityResponse containing master_pubkey, delegation, device list, and profile.
pub async fn query_identity(
    endpoint: &Endpoint,
    target: EndpointId,
) -> anyhow::Result<IdentityResponse> {
    let addr = EndpointAddr::from(target);
    let conn = endpoint.connect(addr, PEER_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let req_bytes = serde_json::to_vec(&PeerRequest::IdentityRequest)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    let resp_bytes = recv.read_to_end(65_536).await?;
    let response: PeerResponse = serde_json::from_slice(&resp_bytes)?;

    conn.close(0u32.into(), b"done");

    match response {
        PeerResponse::Identity(identity) => Ok(identity),
        other => anyhow::bail!(
            "unexpected response: expected Identity, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}
