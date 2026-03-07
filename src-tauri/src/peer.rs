use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_social_types::{
    FollowRequest, FollowResponse, PEER_ALPN, PeerRequest, Visibility, now_millis, short_id,
    sign_follow_request, verify_follow_request,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone)]
pub struct PeerHandler {
    storage: Arc<Storage>,
    node_id: String,
    app_handle: AppHandle,
}

impl PeerHandler {
    pub fn new(storage: Arc<Storage>, node_id: String, app_handle: AppHandle) -> Self {
        Self {
            storage,
            node_id,
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
            },
            short_id(&remote_str)
        );

        match req {
            PeerRequest::Sync(sync_req) => {
                crate::sync::handle_sync(
                    &self.storage,
                    &self.node_id,
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
                    &self.node_id,
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
                    &self.node_id,
                    &remote_str,
                    &self.app_handle,
                    send,
                    follow_req,
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
    // Validate requester matches remote peer
    if req.requester != remote_str {
        log::warn!(
            "[follow-req] requester mismatch: req={}, remote={}",
            short_id(&req.requester),
            short_id(remote_str)
        );
        return Err(AcceptError::from_err(std::io::Error::other(
            "requester mismatch",
        )));
    }

    // Verify signature
    if let Err(reason) = verify_follow_request(&req) {
        log::warn!(
            "[follow-req] invalid signature from {}: {reason}",
            short_id(remote_str)
        );
        return Err(AcceptError::from_err(std::io::Error::other(
            "invalid signature",
        )));
    }

    // Only Listed users accept follow requests
    let visibility = storage
        .get_visibility(node_id)
        .unwrap_or(Visibility::Public);

    let response = match visibility {
        Visibility::Listed => {
            // Check if already approved
            if storage.is_approved_follower(remote_str).unwrap_or(false) {
                log::info!("[follow-req] already approved: {}", short_id(remote_str));
                FollowResponse::Approved
            } else {
                let now = now_millis();
                let expires_at = now + FOLLOW_REQUEST_TTL_MS;
                match storage.insert_follow_request(remote_str, req.timestamp, now, expires_at) {
                    Ok(true) => {
                        log::info!(
                            "[follow-req] stored pending request from {}",
                            short_id(remote_str)
                        );
                        let _ = storage.insert_notification(
                            "follow_request",
                            remote_str,
                            None,
                            None,
                            now,
                        );
                        let _ = app_handle.emit("follow-request-received", remote_str);
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
                short_id(remote_str)
            );
            FollowResponse::Approved
        }
        Visibility::Private => {
            // Private users deny follow requests
            log::info!(
                "[follow-req] denying for private profile: {}",
                short_id(remote_str)
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
    secret_key_bytes: &[u8; 32],
) -> anyhow::Result<FollowResponse> {
    let my_id = endpoint.id().to_string();
    let timestamp = now_millis();
    let secret_key = iroh::SecretKey::from_bytes(secret_key_bytes);
    let signature = sign_follow_request(&my_id, timestamp, &secret_key);

    let req = FollowRequest {
        requester: my_id,
        timestamp,
        signature,
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
