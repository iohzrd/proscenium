use iroh::{endpoint::Connection, protocol::AcceptError};
use proscenium_types::{
    FollowRequest, FollowResponse, IdentityResponse, Visibility, now_millis, short_id,
    verify_follow_request,
};
use tauri::Emitter;

use super::{FOLLOW_REQUEST_TTL_MS, PeerHandler};

impl PeerHandler {
    pub(super) async fn handle_follow_request(
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
}
