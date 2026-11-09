use crate::constants::FOLLOW_REQUEST_TTL_MS;
use crate::storage::FollowRequestOutcome;
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
                    FollowRequestOutcome::Full => {
                        log::warn!("[follow-req] pending request limit reached");
                        return Err(reject("too many pending follow requests"));
                    }
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
                self.storage
                    .is_following(&my_pubkey, requester)
                    .await
                    .map_err(AcceptError::from_err)?
                    && self
                        .storage
                        .approve_follower_directly(
                            &my_pubkey,
                            requester,
                            req.timestamp,
                            now,
                            expires_at,
                        )
                        .await
                        .map_err(AcceptError::from_err)?
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
        .await
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
