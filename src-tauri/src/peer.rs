use crate::storage::Storage;
use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_social_types::{PeerRequest, short_id};
use std::sync::Arc;
use tauri::AppHandle;

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
            PeerRequest::FollowRequest(_) => {
                // TODO: implement follow request handling
                Err(AcceptError::from_err(std::io::Error::other(
                    "not implemented",
                )))
            }
        }
    }
}
