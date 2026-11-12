use crate::constants::FOLLOW_REQUEST_TTL_MS;
use crate::storage::{DirectApproval, FollowRequestOutcome};
use iroh::{
    endpoint::{Connection, SendStream},
    protocol::AcceptError,
};
use proscenium_types::{
    FollowRequest, FollowResponse, VerifiedIdentity, Visibility, now_millis, short_id,
    verify_follow_request,
};
use tauri::Emitter;

use super::{PeerHandler, reject};

impl PeerHandler {
    /// Answer a follow request.
    ///
    /// Public: approved without a stored row. Listed: a pending row is stored for the
    /// local user to approve or deny; an approved requester is answered `Approved`, a denied one
    /// `Denied`. Private: approved if the local user follows the requester, else denied.
    /// After the first approval of a requester a Private user sends a follow request back,
    /// so two Private users who follow each other become mutuals in either order.
    pub(super) async fn handle_follow_request(
        &self,
        remote_str: &str,
        send: SendStream,
        req: FollowRequest,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let my_pubkey = self.identity.read().await.master_pubkey.clone();

        // Checks: addressed to this user, recent timestamp, signed device list containing
        // the sending NodeId, request signature by the requester's master key.
        let verified = match verify_follow_request(&req, remote_str, &my_pubkey) {
            Ok(identity) => identity,
            Err(reason) => {
                log::warn!(
                    "[follow-req] rejected request from {}: {reason}",
                    short_id(remote_str)
                );
                return Err(reject("invalid follow request"));
            }
        };
        let requester = verified.master_pubkey.as_str();

        if requester == my_pubkey {
            return Err(reject("follow request from the local user"));
        }
        if self
            .storage
            .is_blocked(requester)
            .await
            .map_err(AcceptError::from_err)?
        {
            log::warn!(
                "[follow-req] rejecting blocked user {}",
                short_id(requester)
            );
            return Err(reject("blocked"));
        }

        // A failed read rejects the request; it is not treated as Public.
        let visibility = self
            .storage
            .get_visibility(&my_pubkey)
            .await
            .map_err(AcceptError::from_err)?;

        let now = now_millis();
        let expires_at = now.saturating_add(FOLLOW_REQUEST_TTL_MS);
        let mut reciprocate = false;
        let approved = match visibility {
            Visibility::Public => true,
            Visibility::Listed => {
                let outcome = self
                    .storage
                    .record_follow_request(requester, req.timestamp, now, expires_at)
                    .await
                    .map_err(AcceptError::from_err)?;
                match outcome {
                    FollowRequestOutcome::Approved => true,
                    FollowRequestOutcome::Denied => false,
                    FollowRequestOutcome::NewPending | FollowRequestOutcome::StillPending => {
                        // The identity is stored so the requester can be reached and
                        // verified after approval.
                        self.cache_requester(&verified).await?;
                        // Notified once per pending row: a repeated request while the
                        // row exists produces no notification and no event.
                        if outcome == FollowRequestOutcome::NewPending {
                            log::info!(
                                "[follow-req] stored pending request from {}",
                                short_id(requester)
                            );
                            let _ = self
                                .storage
                                .insert_notification("follow_request", requester, None, None, now)
                                .await;
                            let _ = self.app_handle.emit("follow-request-received", requester);
                            let _ = self.app_handle.emit("notification-received", ());
                        }
                        return Self::respond(send, conn, &FollowResponse::Pending).await;
                    }
                }
            }
            Visibility::Private => {
                let follows_requester = self
                    .storage
                    .is_following(&my_pubkey, requester)
                    .await
                    .map_err(AcceptError::from_err)?;
                let approval = if follows_requester {
                    self.storage
                        .approve_follower_directly(
                            &my_pubkey,
                            requester,
                            req.timestamp,
                            now,
                            expires_at,
                        )
                        .await
                        .map_err(AcceptError::from_err)?
                } else {
                    DirectApproval::Denied
                };
                reciprocate = approval == DirectApproval::New;
                approval != DirectApproval::Denied
            }
        };

        if !approved {
            log::info!("[follow-req] denied {}", short_id(requester));
            return Self::respond(send, conn, &FollowResponse::Denied).await;
        }

        // With Listed and Private visibility content is pushed to approved followers; the
        // push reads their NodeIds from the stored identity. Followers of a Public user
        // are recorded when they join the feed topic.
        if visibility != Visibility::Public {
            self.cache_requester(&verified).await?;
        }
        log::info!(
            "[follow-req] approved {} ({visibility} profile)",
            short_id(requester)
        );
        let own_identity = self
            .identity_response()
            .await
            .map_err(AcceptError::from_err)?;
        Self::respond(
            send,
            conn,
            &FollowResponse::Approved(Box::new(own_identity)),
        )
        .await?;
        if reciprocate {
            self.request_follow_back(conn.remote_id(), requester);
        }
        Ok(())
    }

    /// Send a follow request to `pubkey` at the NodeId `node` in a background task.
    /// Called by a Private user after approving `pubkey`, whom the local user follows.
    fn request_follow_back(&self, node: iroh::EndpointId, pubkey: &str) {
        let endpoint = self.gossip.endpoint.clone();
        let storage = self.storage.clone();
        let identity = self.identity.clone();
        let pubkey = pubkey.to_string();
        tokio::spawn(async move {
            match super::send_follow_request(&endpoint, &storage, &identity, node, &pubkey).await {
                Ok(response) => log::info!(
                    "[follow-req] follow request back to {}: {response:?}",
                    short_id(&pubkey)
                ),
                Err(e) => log::warn!(
                    "[follow-req] follow request back to {} failed: {e}",
                    short_id(&pubkey)
                ),
            }
        });
    }

    /// Store the verified identity of a follow requester.
    /// Returns an error if storing fails.
    async fn cache_requester(&self, verified: &VerifiedIdentity) -> Result<(), AcceptError> {
        self.storage
            .cache_peer_identity(verified)
            .await
            .map_err(|e| {
                log::warn!(
                    "[follow-req] not caching identity of {}: {e}",
                    short_id(&verified.master_pubkey)
                );
                reject("identity could not be stored")
            })
    }
}
