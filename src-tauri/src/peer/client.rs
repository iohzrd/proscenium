use crate::error::AppError;
use crate::storage::Storage;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use proscenium_types::{
    FollowRequest, FollowResponse, FollowersListResponse, FollowsListResponse, IdentityResponse,
    PEER_ALPN, PeerRequest, PeerResponse, SigningKeyDelegation, now_millis, sign_follow_request,
};

/// Client: fetch a remote peer's follow list.
pub async fn fetch_remote_follows(
    endpoint: &Endpoint,
    target: EndpointId,
) -> Result<FollowsListResponse, AppError> {
    let addr = EndpointAddr::from(target);
    let conn = endpoint.connect(addr, PEER_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let req_bytes = serde_json::to_vec(&PeerRequest::FollowsListRequest)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    let resp_bytes = recv.read_to_end(1_000_000).await?;
    let response: PeerResponse = serde_json::from_slice(&resp_bytes)?;

    conn.close(0u32.into(), b"done");

    match response {
        PeerResponse::FollowsList(list) => Ok(list),
        other => Err(AppError::Other(format!(
            "unexpected response: expected FollowsList, got {:?}",
            std::mem::discriminant(&other)
        ))),
    }
}

/// Client: fetch a remote peer's followers list.
pub async fn fetch_remote_followers(
    endpoint: &Endpoint,
    target: EndpointId,
) -> Result<FollowersListResponse, AppError> {
    let addr = EndpointAddr::from(target);
    let conn = endpoint.connect(addr, PEER_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;

    let req_bytes = serde_json::to_vec(&PeerRequest::FollowersListRequest)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    let resp_bytes = recv.read_to_end(1_000_000).await?;
    let response: PeerResponse = serde_json::from_slice(&resp_bytes)?;

    conn.close(0u32.into(), b"done");

    match response {
        PeerResponse::FollowersList(list) => Ok(list),
        other => Err(AppError::Other(format!(
            "unexpected response: expected FollowersList, got {:?}",
            std::mem::discriminant(&other)
        ))),
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
