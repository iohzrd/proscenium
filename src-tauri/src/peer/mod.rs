mod client;
mod follow_protocol;
mod linking;
mod list_sharing;

pub use client::{fetch_remote_list, query_identity, send_follow_request};

use crate::constants::{
    PEER_ACCEPT_STREAM_TIMEOUT, PEER_CLOSE_WAIT, PEER_MAX_CONNECTIONS,
    PEER_MAX_CONNECTIONS_PER_NODE, PEER_MAX_DEVICE_SYNC_REQUEST_BYTES,
    PEER_MAX_FOLLOW_REQUEST_BYTES, PEER_MAX_LINK_REQUEST_BYTES, PEER_MAX_PUSH_BYTES,
    PEER_MAX_SMALL_REQUEST_BYTES, PEER_MAX_SYNC_REQUEST_BYTES, PEER_REQUEST_READ_TIMEOUT,
    PEER_RESPONSE_WRITE_TIMEOUT, SYNC_SERVE_TIMEOUT, SYNC_SERVER_CONCURRENCY,
};
use crate::error::AppError;
use crate::framing::{wait_closed, write_all_timeout};
use crate::gossip::GossipService;
use crate::stage::StageActorHandle;
use crate::state::SharedIdentity;
use crate::storage::{RemoteListKind, Storage};
use crate::util::NodeSlot;
use iroh::{
    EndpointId,
    endpoint::{Connection, RecvStream, SendStream},
    protocol::{AcceptError, ProtocolHandler},
};
use linking::PendingLinkState;
use proscenium_types::{IdentityResponse, PeerRequest, PeerResponse, Visibility, short_id};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

/// Number of leading request bytes that contain the variant name of any `PeerRequest`.
const REQUEST_HEAD_LEN: usize = 32;
/// Bytes requested from the stream per read call while receiving a request.
const REQUEST_READ_CHUNK: usize = 16 * 1024;

fn reject(reason: &'static str) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(reason))
}

/// Largest request body accepted for the `PeerRequest` variant whose JSON starts with
/// `head`. `None` if `head` does not start with a known variant name.
///
/// serde encodes a unit variant as `"Name"` and any other variant as `{"Name":...}`.
fn request_size_limit(head: &[u8]) -> Option<usize> {
    let mut rest = head.trim_ascii_start();
    if let Some(inner) = rest.strip_prefix(b"{") {
        rest = inner.trim_ascii_start();
    }
    let rest = rest.strip_prefix(b"\"")?;
    let end = rest.iter().position(|b| *b == b'"')?;
    match rest.get(..end)? {
        b"Sync" => Some(PEER_MAX_SYNC_REQUEST_BYTES),
        b"IdentityRequest" | b"FollowsListRequest" | b"FollowersListRequest" => {
            Some(PEER_MAX_SMALL_REQUEST_BYTES)
        }
        b"FollowRequest" => Some(PEER_MAX_FOLLOW_REQUEST_BYTES),
        b"Push" => Some(PEER_MAX_PUSH_BYTES),
        b"LinkRequest" => Some(PEER_MAX_LINK_REQUEST_BYTES),
        b"DeviceSyncRequest" => Some(PEER_MAX_DEVICE_SYNC_REQUEST_BYTES),
        _ => None,
    }
}

/// Read one request body until the remote finishes the stream.
///
/// The size limit depends on the request type, which is read from the first
/// `REQUEST_HEAD_LEN` bytes. At most one byte more than the limit is buffered.
async fn read_request(recv: &mut RecvStream) -> Result<Vec<u8>, AppError> {
    let mut body = Vec::new();
    let mut chunk = vec![0u8; REQUEST_READ_CHUNK];
    let mut limit: Option<usize> = None;
    loop {
        let cap = limit.unwrap_or(REQUEST_HEAD_LEN);
        let want = cap
            .saturating_add(1)
            .saturating_sub(body.len())
            .clamp(1, REQUEST_READ_CHUNK);
        let Some(buf) = chunk.get_mut(..want) else {
            return Err(AppError::Other("request buffer range out of bounds".into()));
        };
        let read = recv
            .read(buf)
            .await
            .map_err(|e| AppError::Other(format!("request read failed: {e}")))?;
        let Some(n) = read else {
            break;
        };
        let Some(data) = buf.get(..n) else {
            return Err(AppError::Other("request read length out of bounds".into()));
        };
        body.extend_from_slice(data);
        if limit.is_none() && body.len() >= REQUEST_HEAD_LEN {
            limit = Some(
                request_size_limit(&body)
                    .ok_or_else(|| AppError::Other("unknown request type".into()))?,
            );
        }
        if let Some(limit) = limit
            && body.len() > limit
        {
            return Err(AppError::Other(format!(
                "request exceeds {limit} bytes for its type"
            )));
        }
    }
    // A request shorter than the head was not checked inside the loop.
    let limit = match limit {
        Some(limit) => limit,
        None => request_size_limit(&body)
            .ok_or_else(|| AppError::Other("unknown request type".into()))?,
    };
    if body.len() > limit {
        return Err(AppError::Other(format!(
            "request exceeds {limit} bytes for its type"
        )));
    }
    Ok(body)
}

#[derive(Clone)]
pub struct PeerHandler {
    storage: Arc<Storage>,
    identity: SharedIdentity,
    gossip: GossipService,
    pending_link: PendingLinkState,
    app_handle: tauri::AppHandle,
    stage_handle: StageActorHandle,
    /// Limits the connections handled at the same time.
    connection_limit: Arc<Semaphore>,
    /// Connections currently handled per remote NodeId.
    connections_per_node: Arc<Mutex<HashMap<EndpointId, usize>>>,
    /// Limits the sync requests served at the same time.
    sync_limit: Arc<Semaphore>,
    /// Sync requests currently served per remote NodeId (at most one).
    syncs_per_node: Arc<Mutex<HashMap<EndpointId, usize>>>,
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
            connection_limit: Arc::new(Semaphore::new(PEER_MAX_CONNECTIONS)),
            connections_per_node: Arc::new(Mutex::new(HashMap::new())),
            sync_limit: Arc::new(Semaphore::new(SYNC_SERVER_CONCURRENCY)),
            syncs_per_node: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Build the signed `IdentityResponse` for this node.
    async fn identity_response(&self) -> Result<IdentityResponse, AppError> {
        crate::announcement::build_own_identity_response(&self.storage, &self.identity).await
    }

    /// Serialize `response`, write it to `send`, finish the stream and wait for the
    /// remote to close the connection. Each step has a timeout.
    async fn respond<T: serde::Serialize>(
        mut send: SendStream,
        conn: &Connection,
        response: &T,
    ) -> Result<(), AcceptError> {
        let bytes = serde_json::to_vec(response).map_err(AcceptError::from_err)?;
        write_all_timeout(&mut send, &bytes, PEER_RESPONSE_WRITE_TIMEOUT)
            .await
            .map_err(AcceptError::from_err)?;
        wait_closed(conn, PEER_CLOSE_WAIT).await;
        Ok(())
    }

    /// Answer an `IdentityRequest` with the signed device list, and with the profile only
    /// if it is Public or `may_see_profile` allows the requester `remote_str`.
    async fn handle_identity_request(
        &self,
        remote_str: &str,
        send: SendStream,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let mut identity = self
            .identity_response()
            .await
            .map_err(AcceptError::from_err)?;
        if let Some(profile) = &identity.profile
            && profile.visibility != Visibility::Public
            && !self.may_see_profile(remote_str, profile.visibility).await?
        {
            identity.profile = None;
        }
        Self::respond(send, conn, &PeerResponse::Identity(identity)).await
    }

    /// True if the NodeId `remote_str` may read a profile with `visibility`: another
    /// device of the local user, or the device of an approved follower (Listed) or of a
    /// mutual (Private).
    async fn may_see_profile(
        &self,
        remote_str: &str,
        visibility: Visibility,
    ) -> Result<bool, AcceptError> {
        if self
            .storage
            .is_other_device(remote_str)
            .await
            .map_err(AcceptError::from_err)?
        {
            return Ok(true);
        }
        let Some(remote_pubkey) = self
            .storage
            .try_get_master_pubkey_for_transport(remote_str)
            .await
            .map_err(AcceptError::from_err)?
        else {
            return Ok(false);
        };
        let my_pubkey = self.identity.read().await.master_pubkey.clone();
        match visibility {
            Visibility::Private => self.storage.is_mutual(&my_pubkey, &remote_pubkey).await,
            _ => self.storage.is_approved_follower(&remote_pubkey).await,
        }
        .map_err(AcceptError::from_err)
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

        // Bound the handlers that run at the same time, in total and per remote NodeId.
        // Both are released when this function returns.
        let Ok(_connection_permit) = self.connection_limit.clone().try_acquire_owned() else {
            conn.close(1u32.into(), b"busy");
            return Err(reject("connection limit reached"));
        };
        let Some(_node_slot) = NodeSlot::acquire(
            &self.connections_per_node,
            remote,
            PEER_MAX_CONNECTIONS_PER_NODE,
        ) else {
            conn.close(1u32.into(), b"busy");
            return Err(reject("per-node connection limit reached"));
        };

        // Reject a blocked user. The NodeId is mapped to a master pubkey if a stored
        // device list names it; a failed lookup rejects the connection.
        let remote_pubkey = self
            .storage
            .try_get_master_pubkey_for_transport(&remote_str)
            .await
            .map_err(AcceptError::from_err)?
            .unwrap_or_else(|| remote_str.clone());
        if self
            .storage
            .is_blocked(&remote_pubkey)
            .await
            .map_err(AcceptError::from_err)?
        {
            log::warn!("[peer] rejecting blocked peer {}", short_id(&remote_pubkey));
            return Err(reject("blocked"));
        }

        let (send, mut recv) = tokio::time::timeout(PEER_ACCEPT_STREAM_TIMEOUT, conn.accept_bi())
            .await
            .map_err(|_| reject("no request stream opened in time"))??;

        let req_bytes = tokio::time::timeout(PEER_REQUEST_READ_TIMEOUT, read_request(&mut recv))
            .await
            .map_err(|_| reject("request not received in time"))?
            .map_err(AcceptError::from_err)?;
        let req: PeerRequest = serde_json::from_slice(&req_bytes).map_err(AcceptError::from_err)?;
        drop(req_bytes);

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

        let my_pubkey = self.identity.read().await.master_pubkey.clone();

        match req {
            PeerRequest::Sync(sync_req) => {
                let Some(_sync_slot) = NodeSlot::acquire(&self.syncs_per_node, remote, 1) else {
                    conn.close(1u32.into(), b"busy");
                    return Err(reject("a sync of this node is already served"));
                };
                let Ok(_sync_permit) = self.sync_limit.clone().try_acquire_owned() else {
                    conn.close(1u32.into(), b"busy");
                    return Err(reject("sync limit reached"));
                };
                let served = tokio::time::timeout(
                    SYNC_SERVE_TIMEOUT,
                    crate::sync::handle_sync(
                        &self.storage,
                        &self.identity,
                        &remote_str,
                        &conn,
                        send,
                        sync_req,
                        &self.stage_handle,
                    ),
                )
                .await;
                served.unwrap_or_else(|_| {
                    conn.close(1u32.into(), b"sync timed out");
                    Err(reject("sync not completed in time"))
                })
            }
            PeerRequest::Push(push_msg) => {
                crate::push::handle_push(
                    &self.storage,
                    &my_pubkey,
                    &remote_str,
                    &self.app_handle,
                    send,
                    *push_msg,
                    &conn,
                )
                .await
            }
            PeerRequest::FollowRequest(follow_req) => {
                self.handle_follow_request(&remote_str, send, *follow_req, &conn)
                    .await
            }
            PeerRequest::IdentityRequest => {
                self.handle_identity_request(&remote_str, send, &conn).await
            }
            PeerRequest::FollowsListRequest => {
                self.handle_list_request(
                    RemoteListKind::Follows,
                    &my_pubkey,
                    &remote_str,
                    send,
                    &conn,
                )
                .await
            }
            PeerRequest::FollowersListRequest => {
                self.handle_list_request(
                    RemoteListKind::Followers,
                    &my_pubkey,
                    &remote_str,
                    send,
                    &conn,
                )
                .await
            }
            PeerRequest::LinkRequest {
                secret,
                node_id,
                device_name,
            } => {
                self.handle_link_request(send, secret, &node_id, &device_name, &conn)
                    .await
            }
            PeerRequest::DeviceSyncRequest { announcement } => {
                crate::device_sync::handle_device_sync(
                    &self.storage,
                    &self.identity,
                    &self.gossip,
                    &self.app_handle,
                    &conn,
                    send,
                    &announcement,
                )
                .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proscenium_types::{SyncRequest, post_ids_digest};

    /// The largest device list that passes `verify_linked_devices_announcement`: names of
    /// control characters, which JSON escapes as `\u00XX`.
    fn largest_announcement() -> proscenium_types::LinkedDevicesAnnouncement {
        use proscenium_types::{DeviceEntry, MAX_DEVICE_NAME_LEN, MAX_LINKED_DEVICES};
        proscenium_types::LinkedDevicesAnnouncement {
            master_pubkey: "a".repeat(64),
            devices: (0..MAX_LINKED_DEVICES)
                .map(|_| DeviceEntry {
                    node_id: "b".repeat(64),
                    device_name: "\u{1}".repeat(MAX_DEVICE_NAME_LEN),
                    added_at: u64::MAX,
                })
                .collect(),
            removed: vec![],
            version: u64::MAX,
            signature: "c".repeat(128),
        }
    }

    #[test]
    fn request_size_limit_selects_by_variant_name() {
        let sync = serde_json::to_vec(&PeerRequest::Sync(SyncRequest {
            author: "a".repeat(64),
            post_count: u64::MAX,
            interaction_count: u64::MAX,
            newest_timestamp: u64::MAX,
            newest_interaction_timestamp: u64::MAX,
            post_ids_digest: post_ids_digest(&mut []),
            tombstone_count: u64::MAX,
            since: u64::MAX,
            announcement: Some(largest_announcement()),
        }))
        .unwrap();
        assert!(sync.len() <= PEER_MAX_SYNC_REQUEST_BYTES);
        assert_eq!(request_size_limit(&sync), Some(PEER_MAX_SYNC_REQUEST_BYTES));

        for request in [
            PeerRequest::IdentityRequest,
            PeerRequest::FollowsListRequest,
            PeerRequest::FollowersListRequest,
        ] {
            let bytes = serde_json::to_vec(&request).unwrap();
            assert!(bytes.len() <= REQUEST_HEAD_LEN);
            assert_eq!(
                request_size_limit(&bytes),
                Some(PEER_MAX_SMALL_REQUEST_BYTES)
            );
        }

        assert_eq!(
            request_size_limit(br#"{"Push":{"author":"x"#),
            Some(PEER_MAX_PUSH_BYTES)
        );
        assert_eq!(
            request_size_limit(br#" { "FollowRequest" : {"#),
            Some(PEER_MAX_FOLLOW_REQUEST_BYTES)
        );
        assert_eq!(
            request_size_limit(br#"{"LinkRequest":{"secret":"a"#),
            Some(PEER_MAX_LINK_REQUEST_BYTES)
        );
        assert_eq!(
            request_size_limit(br#"{"DeviceSyncRequest":{"announcement""#),
            Some(PEER_MAX_DEVICE_SYNC_REQUEST_BYTES)
        );
    }

    #[test]
    fn largest_device_sync_request_fits_its_limit() {
        let request = PeerRequest::DeviceSyncRequest {
            announcement: largest_announcement(),
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(bytes.len() <= PEER_MAX_DEVICE_SYNC_REQUEST_BYTES);
        assert_eq!(
            request_size_limit(&bytes),
            Some(PEER_MAX_DEVICE_SYNC_REQUEST_BYTES)
        );
    }

    #[test]
    fn request_size_limit_rejects_unknown_or_malformed_heads() {
        assert_eq!(request_size_limit(br#"{"Unknown":1}"#), None);
        assert_eq!(request_size_limit(b""), None);
        assert_eq!(request_size_limit(b"[1,2,3]"), None);
        assert_eq!(request_size_limit(br#"{"Sync"#), None);
        assert_eq!(request_size_limit("{\"\u{20ac}".as_bytes()), None);
    }

    #[test]
    fn node_slot_limits_connections_per_node() {
        let counts = Arc::new(Mutex::new(HashMap::new()));
        let node = iroh::SecretKey::from_bytes(&[3u8; 32]).public();
        let mut slots: Vec<NodeSlot> = (0..PEER_MAX_CONNECTIONS_PER_NODE)
            .map(|_| NodeSlot::acquire(&counts, node, PEER_MAX_CONNECTIONS_PER_NODE).unwrap())
            .collect();
        assert!(NodeSlot::acquire(&counts, node, PEER_MAX_CONNECTIONS_PER_NODE).is_none());
        slots.pop();
        assert!(NodeSlot::acquire(&counts, node, PEER_MAX_CONNECTIONS_PER_NODE).is_some());
        drop(slots);
        assert!(counts.lock().unwrap().is_empty());
    }
}
