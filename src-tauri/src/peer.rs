use crate::error::AppError;
use crate::gossip::GossipService;
use crate::stage::StageActorHandle;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use proscenium_types::{
    FollowRequest, FollowResponse, IdentityResponse, LinkQrPayload, PEER_ALPN, PeerRequest,
    PeerResponse, SigningKeyDelegation, Visibility, derive_transport_key, now_millis, short_id,
    sign_follow_request, verify_follow_request,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// Active device-linking session on the existing device.
/// Created when the user initiates "Link New Device", consumed when a new device connects.
struct PendingLink {
    psk: [u8; 32],
    x25519_private: [u8; 32],
    expires_at: u64,
    transfer_master_key: bool,
}

type PendingLinkState = Arc<tokio::sync::Mutex<Option<PendingLink>>>;

const LINK_SESSION_TTL_MS: u64 = 60_000; // 60 seconds
const FOLLOW_REQUEST_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000; // 30 days

#[derive(Clone)]
pub struct PeerHandler {
    storage: Arc<Storage>,
    identity: SharedIdentity,
    gossip: GossipService,
    pending_link: PendingLinkState,
    app_handle: AppHandle,
    stage_handle: StageActorHandle,
}

impl PeerHandler {
    pub fn new(
        storage: Arc<Storage>,
        identity: SharedIdentity,
        gossip: GossipService,
        app_handle: AppHandle,
        stage_handle: StageActorHandle,
    ) -> Self {
        Self {
            storage,
            identity,
            gossip,
            pending_link: Arc::new(tokio::sync::Mutex::new(None)),
            app_handle,
            stage_handle,
        }
    }

    /// Begin a device-link session. Returns the QR payload the initiating device displays.
    /// `relay_url` should come from `endpoint.addr().relay_urls().next()`.
    pub async fn start_link_session(
        &self,
        transfer_master_key: bool,
        relay_url: Option<String>,
    ) -> Result<LinkQrPayload, AppError> {
        let mut psk = [0u8; 32];
        getrandom::fill(&mut psk)?;

        let (master_secret_key_bytes, transport_node_id) = {
            let id = self.identity.read().await;
            (id.master_secret_key_bytes, id.transport_node_id.clone())
        };

        let transport_secret_bytes = derive_transport_key(&master_secret_key_bytes, 0);
        let x25519_private = crate::crypto::ed25519_secret_to_x25519(&transport_secret_bytes);

        let expires_at = now_millis() + LINK_SESSION_TTL_MS;

        {
            let mut lock = self.pending_link.lock().await;
            *lock = Some(PendingLink {
                psk,
                x25519_private,
                expires_at,
                transfer_master_key,
            });
        }

        use base64::Engine;
        let payload = LinkQrPayload {
            node_id: transport_node_id,
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(psk),
            relay_url,
        };

        log::info!("[link] started device link session (expires in 60s)");
        Ok(payload)
    }

    /// Cancel an active device-link session.
    pub async fn cancel_link_session(&self) {
        let mut lock = self.pending_link.lock().await;
        *lock = None;
        log::info!("[link] cancelled device link session");
    }

    /// Build an IdentityResponse for this node.
    async fn identity_response(&self) -> IdentityResponse {
        let (master_pubkey, delegation, transport_node_id) = {
            let id = self.identity.read().await;
            (
                id.master_pubkey.clone(),
                id.delegation.clone(),
                id.transport_node_id.clone(),
            )
        };
        let profile = self
            .storage
            .get_profile(&master_pubkey)
            .await
            .ok()
            .flatten();
        IdentityResponse {
            master_pubkey,
            delegation,
            transport_node_ids: vec![transport_node_id],
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
        if let Err(reason) = proscenium_types::verify_delegation(&req.delegation) {
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
            .get_visibility(&self.identity.read().await.master_pubkey)
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

        let (
            master_secret_key_bytes,
            master_pubkey,
            signing_secret_key_bytes,
            dm_secret_key_bytes,
            delegation,
        ) = {
            let id = self.identity.read().await;
            (
                id.master_secret_key_bytes,
                id.master_pubkey.clone(),
                id.signing_secret_key_bytes,
                id.dm_secret_key_bytes,
                id.delegation.clone(),
            )
        };

        let new_transport_key_bytes =
            derive_transport_key(&master_secret_key_bytes, new_device_index);

        // Build the link bundle
        let master_key_to_send = if link_session.transfer_master_key {
            Some(&master_secret_key_bytes)
        } else {
            None
        };

        let bundle = self
            .storage
            .export_link_bundle(
                &master_pubkey,
                &signing_secret_key_bytes,
                &dm_secret_key_bytes,
                &delegation,
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
            let signing_sk = iroh::SecretKey::from_bytes(&signing_secret_key_bytes);
            let mut announcement = proscenium_types::LinkedDevicesAnnouncement {
                master_pubkey: master_pubkey.clone(),
                delegation: delegation.clone(),
                devices: all_devices,
                version: (new_device_index + 1) as u64,
                timestamp: now,
                signature: String::new(),
            };
            proscenium_types::sign_linked_devices_announcement(&mut announcement, &signing_sk);

            if let Err(e) = self.gossip.broadcast_linked_devices(&announcement).await {
                log::error!("[link] failed to broadcast device announcement: {e}");
            } else {
                log::info!("[link] broadcast updated device announcement");
            }
        }

        conn.closed().await;
        Ok(())
    }
}

impl std::fmt::Debug for PeerHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let master_pubkey = self
            .identity
            .try_read()
            .map(|id| id.master_pubkey.clone())
            .unwrap_or_default();
        f.debug_struct("PeerHandler")
            .field("master_pubkey", &master_pubkey)
            .finish_non_exhaustive()
    }
}

impl ProtocolHandler for PeerHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();

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
            log::warn!("[peer] rejecting blocked peer {}", short_id(&remote_pubkey));
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

        let (my_pubkey, signing_secret_key_bytes) = {
            let id = self.identity.read().await;
            (id.master_pubkey.clone(), id.signing_secret_key_bytes)
        };

        match req {
            PeerRequest::Sync(sync_req) => {
                crate::sync::handle_sync(
                    &self.storage,
                    &my_pubkey,
                    &remote_str,
                    &conn,
                    send,
                    sync_req,
                    &self.stage_handle,
                )
                .await
            }
            PeerRequest::Push(push_msg) => {
                crate::push::handle_push(
                    &self.storage,
                    &my_pubkey,
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
                    &my_pubkey,
                    &signing_secret_key_bytes,
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
) -> Result<FollowResponse, AppError> {
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
) -> Result<IdentityResponse, AppError> {
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
        other => Err(AppError::Other(format!(
            "unexpected response: expected Identity, got {:?}",
            std::mem::discriminant(&other)
        ))),
    }
}
