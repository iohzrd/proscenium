use crate::constants::{
    MAX_REMOTE_LIST_ENTRIES, PEER_MAX_LIST_RESPONSE_BYTES, PEER_REQUEST_TIMEOUT,
};
use crate::error::AppError;
use crate::state::SharedIdentity;
use crate::storage::{RemoteListKind, Storage};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use proscenium_types::{
    FollowRequest, FollowResponse, PEER_ALPN, PeerRequest, PeerResponse, SocialGraphEntry,
    VerifiedIdentity, is_valid_pubkey, now_millis, sign_follow_request, verify_identity_response,
};

/// Largest identity or follow-request response a requester accepts.
const MAX_IDENTITY_RESPONSE_BYTES: usize = 65_536;

/// Connect to `target`, send `request` on one bi-stream, and return the response bytes
/// (at most `response_limit`). The whole exchange is limited to `PEER_REQUEST_TIMEOUT`.
async fn request_response(
    endpoint: &Endpoint,
    target: EndpointId,
    request: &PeerRequest,
    response_limit: usize,
) -> Result<Vec<u8>, AppError> {
    let req_bytes = serde_json::to_vec(request)?;
    tokio::time::timeout(PEER_REQUEST_TIMEOUT, async {
        let conn = endpoint
            .connect(EndpointAddr::from(target), PEER_ALPN)
            .await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&req_bytes).await?;
        send.finish()?;
        let resp_bytes = recv.read_to_end(response_limit).await?;
        conn.close(0u32.into(), b"done");
        Ok::<_, AppError>(resp_bytes)
    })
    .await
    .map_err(|_| AppError::Other("peer request timed out".into()))?
}

/// A follow or follower list as returned by a remote node.
pub struct RemoteList {
    pub entries: Vec<SocialGraphEntry>,
    /// True if the user does not share this list with the requester.
    pub hidden: bool,
}

/// Client: fetch the follow list or follower list of the user `expected_pubkey` from the
/// NodeId `target`. Returns an error if the response is for another user. Entries whose
/// pubkey is not a canonical key are removed and at most `MAX_REMOTE_LIST_ENTRIES` are returned.
pub async fn fetch_remote_list(
    endpoint: &Endpoint,
    target: EndpointId,
    kind: RemoteListKind,
    expected_pubkey: &str,
) -> Result<RemoteList, AppError> {
    let request = match kind {
        RemoteListKind::Follows => PeerRequest::FollowsListRequest,
        RemoteListKind::Followers => PeerRequest::FollowersListRequest,
    };
    let resp_bytes =
        request_response(endpoint, target, &request, PEER_MAX_LIST_RESPONSE_BYTES).await?;
    let (pubkey, entries, hidden) = match (kind, serde_json::from_slice(&resp_bytes)?) {
        (RemoteListKind::Follows, PeerResponse::FollowsList(list)) => {
            (list.pubkey, list.follows, list.hidden)
        }
        (RemoteListKind::Followers, PeerResponse::FollowersList(list)) => {
            (list.pubkey, list.followers, list.hidden)
        }
        (_, other) => {
            return Err(AppError::Other(format!(
                "unexpected response to a list request: {:?}",
                std::mem::discriminant(&other)
            )));
        }
    };
    if pubkey != expected_pubkey {
        return Err(AppError::Other(
            "list response is for another user".to_string(),
        ));
    }
    let mut entries = entries;
    entries.retain(|entry| is_valid_pubkey(&entry.pubkey));
    entries.truncate(MAX_REMOTE_LIST_ENTRIES);
    Ok(RemoteList { entries, hidden })
}

/// Client: send a follow request to the user `target_master_pubkey` at NodeId `target`.
///
/// If the answer is `Approved`, the responder's identity is verified (signed device list
/// containing `target`, master pubkey equal to `target_master_pubkey`) and cached.
pub async fn send_follow_request(
    endpoint: &Endpoint,
    storage: &Storage,
    identity: &SharedIdentity,
    target: EndpointId,
    target_master_pubkey: &str,
) -> Result<FollowResponse, AppError> {
    let own_identity = crate::announcement::build_own_identity_response(storage, identity).await?;
    let (master_pubkey, signing_secret_key_bytes) = {
        let id = identity.read().await;
        (id.master_pubkey.clone(), id.signing_secret_key_bytes)
    };
    let timestamp = now_millis();
    let secret_key = iroh::SecretKey::from_bytes(&signing_secret_key_bytes);
    let signature =
        sign_follow_request(&master_pubkey, target_master_pubkey, timestamp, &secret_key);

    let request = PeerRequest::FollowRequest(Box::new(FollowRequest {
        requester: master_pubkey,
        target: target_master_pubkey.to_string(),
        timestamp,
        signature,
        identity: own_identity,
    }));
    let resp_bytes =
        request_response(endpoint, target, &request, MAX_IDENTITY_RESPONSE_BYTES).await?;
    let response: FollowResponse = serde_json::from_slice(&resp_bytes)?;

    if let FollowResponse::Approved(ref response) = response {
        let verified = verify_identity_response((**response).clone(), &target.to_string())?;
        if verified.master_pubkey != target_master_pubkey {
            return Err(AppError::Other(
                "follow response identity does not match the requested user".into(),
            ));
        }
        storage.cache_peer_identity(&verified).await?;
    }

    Ok(response)
}

/// Client: ask the node `target` for its identity and verify the answer.
///
/// The returned identity has a master-signed delegation and a signed device list that
/// contains `target`. Nothing is written to storage.
pub async fn query_identity(
    endpoint: &Endpoint,
    target: EndpointId,
) -> Result<VerifiedIdentity, AppError> {
    let resp_bytes = request_response(
        endpoint,
        target,
        &PeerRequest::IdentityRequest,
        MAX_IDENTITY_RESPONSE_BYTES,
    )
    .await?;
    match serde_json::from_slice(&resp_bytes)? {
        PeerResponse::Identity(identity) => {
            Ok(verify_identity_response(identity, &target.to_string())?)
        }
        other => Err(AppError::Other(format!(
            "unexpected response: expected Identity, got {:?}",
            std::mem::discriminant(&other)
        ))),
    }
}
