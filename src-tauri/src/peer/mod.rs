mod client;
mod follow_protocol;
mod linking;
mod list_sharing;

pub use client::{
    fetch_remote_followers, fetch_remote_follows, query_identity, send_follow_request,
};

use crate::gossip::GossipService;
use crate::stage::StageActorHandle;
use crate::state::SharedIdentity;
use crate::storage::Storage;
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use linking::PendingLinkState;
use proscenium_types::{IdentityResponse, PeerRequest, PeerResponse, short_id};
use std::sync::Arc;

const FOLLOW_REQUEST_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000; // 30 days

#[derive(Clone)]
pub struct PeerHandler {
    storage: Arc<Storage>,
    identity: SharedIdentity,
    gossip: GossipService,
    pending_link: PendingLinkState,
    app_handle: tauri::AppHandle,
    stage_handle: StageActorHandle,
}

impl PeerHandler {
    pub fn new(
        storage: Arc<Storage>,
        identity: SharedIdentity,
        gossip: GossipService,
        app_handle: tauri::AppHandle,
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
                PeerRequest::FollowsListRequest => "follows-list",
                PeerRequest::FollowersListRequest => "followers-list",
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
            PeerRequest::FollowsListRequest => {
                self.handle_follows_list_request(&my_pubkey, &remote_str, send, &conn)
                    .await
            }
            PeerRequest::FollowersListRequest => {
                self.handle_followers_list_request(&my_pubkey, &remote_str, send, &conn)
                    .await
            }
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
