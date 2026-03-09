use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_social_types::{
    FollowRequest, FollowResponse, IdentityResponse, PEER_ALPN, PeerRequest, PeerResponse,
    SigningKeyDelegation, Visibility, now_millis, short_id, sign_follow_request,
    verify_follow_request,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone)]
pub struct PeerHandler {
    storage: Arc<Storage>,
    /// The permanent identity (master public key).
    master_pubkey: String,
    /// The transport NodeId (iroh's own key).
    transport_node_id: String,
    /// The current signing key delegation.
    delegation: SigningKeyDelegation,
    app_handle: AppHandle,
}

impl PeerHandler {
    pub fn new(
        storage: Arc<Storage>,
        master_pubkey: String,
        transport_node_id: String,
        delegation: SigningKeyDelegation,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            storage,
            master_pubkey,
            transport_node_id,
            delegation,
            app_handle,
        }
    }
}

impl ProtocolHandler for PeerHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let remote_str = remote.to_string();

        // Reject blocked peers
        if self.storage.is_blocked(&remote_str).unwrap_or(false) {
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
                handle_follow_request(
                    &self.storage,
                    &self.master_pubkey,
                    &remote_str,
                    &self.app_handle,
                    send,
                    follow_req,
                    &conn,
                )
                .await
            }
            PeerRequest::IdentityRequest => {
                handle_identity_request(
                    &self.storage,
                    &self.master_pubkey,
                    &self.transport_node_id,
                    &self.delegation,
                    send,
                    &conn,
                )
                .await
            }
        }
    }
}

const FOLLOW_REQUEST_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000; // 30 days

async fn handle_follow_request(
    storage: &Storage,
    node_id: &str,
    remote_str: &str,
    app_handle: &AppHandle,
    mut send: iroh::endpoint::SendStream,
    req: iroh_social_types::FollowRequest,
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
    let _ = storage.cache_peer_identity(&IdentityResponse {
        master_pubkey: req.requester.clone(),
        delegation: req.delegation.clone(),
        transport_node_ids: vec![remote_str.to_string()],
        profile: None,
    });

    // Use master pubkey (permanent identity) for all storage lookups, not transport NodeId
    let requester_pubkey = &req.requester;

    // Only Listed users accept follow requests
    let visibility = storage
        .get_visibility(node_id)
        .unwrap_or(Visibility::Public);

    let response = match visibility {
        Visibility::Listed => {
            // Check if already approved
            if storage
                .is_approved_follower(requester_pubkey)
                .unwrap_or(false)
            {
                log::info!(
                    "[follow-req] already approved: {}",
                    short_id(requester_pubkey)
                );
                FollowResponse::Approved
            } else {
                let now = now_millis();
                let expires_at = now + FOLLOW_REQUEST_TTL_MS;
                match storage.insert_follow_request(
                    requester_pubkey,
                    req.timestamp,
                    now,
                    expires_at,
                ) {
                    Ok(true) => {
                        log::info!(
                            "[follow-req] stored pending request from {}",
                            short_id(requester_pubkey)
                        );
                        let _ = storage.insert_notification(
                            "follow_request",
                            requester_pubkey,
                            None,
                            None,
                            now,
                        );
                        let _ = app_handle.emit("follow-request-received", requester_pubkey);
                        let _ = app_handle.emit("notification-received", ());
                        FollowResponse::Pending
                    }
                    Ok(false) => {
                        // Already approved (race condition)
                        FollowResponse::Approved
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
            FollowResponse::Approved
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

/// Client: send a follow request to a remote peer.
pub async fn send_follow_request(
    endpoint: &Endpoint,
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

/// Handle an IdentityRequest: respond with our master pubkey, delegation, and profile.
async fn handle_identity_request(
    storage: &Storage,
    master_pubkey: &str,
    transport_node_id: &str,
    delegation: &SigningKeyDelegation,
    mut send: iroh::endpoint::SendStream,
    conn: &Connection,
) -> Result<(), AcceptError> {
    let profile = storage.get_profile(master_pubkey).ok().flatten();
    let response = PeerResponse::Identity(IdentityResponse {
        master_pubkey: master_pubkey.to_string(),
        delegation: delegation.clone(),
        transport_node_ids: vec![transport_node_id.to_string()],
        profile,
    });

    let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
    send.write_all(&resp_bytes)
        .await
        .map_err(AcceptError::from_err)?;
    send.finish().map_err(AcceptError::from_err)?;

    conn.closed().await;
    Ok(())
}
