use crate::constants::MAX_NODE_IDS_TRIED;
use crate::error::CmdResult;
use crate::state::{AppState, SyncCommand};
use crate::storage::RemoteListKind;
use proscenium_types::{
    FollowResponse, SocialGraphEntry, Visibility, is_valid_pubkey, now_millis, short_id,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// 60-minute cache staleness threshold.
const CACHE_MAX_AGE_MS: u64 = 60 * 60 * 1000;

#[derive(Serialize)]
pub struct RemoteFollowsResult {
    pub follows: Vec<SocialGraphEntry>,
    pub hidden: bool,
    pub cached: bool,
}

#[derive(Serialize)]
pub struct RemoteFollowersResult {
    pub followers: Vec<SocialGraphEntry>,
    pub hidden: bool,
    pub cached: bool,
}

/// Result of `follow_user`.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowOutcome {
    /// The follow is stored.
    Followed,
    /// The user's profile is not Public and the follow request awaits approval. The
    /// follow is stored: content arrives once the request is approved, and a Private
    /// user approves the request once they follow the local user.
    Pending,
    /// The user denied the follow request.
    Denied,
}

/// Follow the user whose device has transport NodeId `node_id`.
///
/// The node's identity is verified before anything is stored. If `expected_pubkey` is
/// given, the command fails unless the node's master pubkey equals it. Unless the node
/// returned a Public profile (a node returns a Listed or Private profile only to approved
/// followers), the follow is stored and then a follow request is sent; a denial or a
/// failed request removes the follow again. The follow is stored before the request
/// because a Private user that approves it sends a follow request back at once, which
/// the local node approves only if it already follows that user.
#[tauri::command]
pub async fn follow_user(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    expected_pubkey: Option<String>,
) -> CmdResult<FollowOutcome> {
    let (my_id, my_transport) = {
        let id = state.identity.read().await;
        (id.master_pubkey.clone(), id.transport_node_id.clone())
    };
    if node_id == my_transport {
        return Err("cannot follow yourself".into());
    }
    log::info!("[follow] resolving identity for {}...", short_id(&node_id));

    let target: iroh::EndpointId = node_id.parse()?;
    let identity = crate::peer::query_identity(&state.endpoint(), target)
        .await
        .map_err(|e| format!("failed to query identity from {}: {e}", short_id(&node_id)))?;
    let pubkey = identity.master_pubkey.clone();

    if pubkey == my_id {
        return Err("cannot follow yourself".into());
    }
    if let Some(expected) = &expected_pubkey
        && *expected != pubkey
    {
        return Err(format!(
            "node {} belongs to {}, not to the requested user {}",
            short_id(&node_id),
            short_id(&pubkey),
            short_id(expected)
        )
        .into());
    }
    if state.storage.is_blocked(&pubkey).await? {
        return Err("cannot follow a blocked user".into());
    }

    state.storage.cache_peer_identity(&identity).await?;
    log::info!(
        "[identity] resolved {} -> master={}",
        short_id(&node_id),
        short_id(&pubkey),
    );
    let node_ids = identity.transport_node_ids();

    log::info!("[follow] following {}...", short_id(&pubkey));
    let entry = SocialGraphEntry {
        pubkey: pubkey.clone(),
        followed_at: now_millis(),
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    state.storage.follow(&my_id, &entry).await?;

    // For a Private user, following is the condition of a mutual follow: a pending
    // request of the followee is approved now. A Listed user decides on requests itself.
    if state.storage.get_visibility(&my_id).await? == Visibility::Private
        && state
            .storage
            .approve_follow_request(&my_id, &pubkey, now_millis())
            .await?
    {
        log::info!(
            "[follow] approved the pending request of {}",
            short_id(&pubkey)
        );
    }

    let mut outcome = FollowOutcome::Followed;
    if identity
        .profile
        .as_ref()
        .is_none_or(|p| p.visibility != Visibility::Public)
    {
        log::info!("[follow] sending a follow request to {}", short_id(&pubkey));
        let response = crate::peer::send_follow_request(
            &state.endpoint(),
            &state.storage,
            &state.identity,
            target,
            &pubkey,
        )
        .await;
        match response {
            Ok(FollowResponse::Approved(_)) => {}
            Ok(FollowResponse::Pending) => outcome = FollowOutcome::Pending,
            Ok(FollowResponse::Denied) => {
                state.storage.unfollow(&my_id, &pubkey).await?;
                return Ok(FollowOutcome::Denied);
            }
            Ok(FollowResponse::RateLimited) => {
                state.storage.unfollow(&my_id, &pubkey).await?;
                return Err(format!(
                    "{} receives too many follow requests now; try again later",
                    short_id(&pubkey)
                )
                .into());
            }
            Err(e) => {
                state.storage.unfollow(&my_id, &pubkey).await?;
                return Err(e);
            }
        }
    }

    state.gossip().follow_user(pubkey.clone(), node_ids).await?;
    log::info!("[follow] subscribed to gossip for {}", short_id(&pubkey));

    // Queue a background sync for the new followee. `try_send` does not wait when the
    // queue is full; the periodic sync covers the followee in that case.
    if state
        .sync_tx()
        .try_send(SyncCommand::SyncPeer(pubkey.clone()))
        .is_err()
    {
        log::debug!("[follow] sync queue full, {} not queued", short_id(&pubkey));
    }

    Ok(outcome)
}

/// Unfollow `pubkey`: mark the follow as removed, close the gossip subscription, and
/// delete the stored posts of `pubkey`.
///
/// Fails for the local user's own pubkey. Posts are deleted only if an active follow
/// existed, so the command cannot be used to delete posts of an arbitrary author.
#[tauri::command]
pub async fn unfollow_user(state: State<'_, Arc<AppState>>, pubkey: String) -> CmdResult<()> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    if pubkey == my_id {
        return Err("cannot unfollow yourself".into());
    }
    log::info!("[follow] unfollowing {}...", short_id(&pubkey));
    let was_following = state.storage.unfollow(&my_id, &pubkey).await?;
    state.gossip().unfollow_user(&pubkey).await;
    if !was_following {
        log::info!("[follow] {} was not followed", short_id(&pubkey));
        return Ok(());
    }
    let deleted = state.storage.delete_posts_by_author(&pubkey).await?;
    log::info!(
        "[follow] unfollowed {}, deleted {deleted} posts",
        short_id(&pubkey)
    );
    Ok(())
}

#[tauri::command]
pub async fn get_follows(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<SocialGraphEntry>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    state.storage.get_follows(&my_id).await
}

#[tauri::command]
pub async fn get_followers(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<SocialGraphEntry>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    state.storage.get_followers(&my_id).await
}

#[tauri::command]
pub async fn get_peer_node_ids(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<Vec<String>> {
    state.storage.get_peer_transport_node_ids(&pubkey).await
}

fn cache_is_fresh(fetched_at: Option<u64>) -> bool {
    fetched_at.is_some_and(|t| now_millis().saturating_sub(t) < CACHE_MAX_AGE_MS)
}

/// The follow list or follower list of another user: the cached copy if it is younger
/// than `CACHE_MAX_AGE_MS`, else a copy fetched from one of the user's NodeIds, else the
/// cached copy whatever its age. Returns `(entries, hidden, cached)`.
///
/// Fails for the local user's own pubkey: the local lists are local facts and are read
/// with `get_follows` and `get_followers`.
async fn remote_list(
    state: &AppState,
    pubkey: &str,
    kind: RemoteListKind,
) -> CmdResult<(Vec<SocialGraphEntry>, bool, bool)> {
    if !is_valid_pubkey(pubkey) {
        return Err("invalid pubkey".into());
    }
    if pubkey == state.identity.read().await.master_pubkey {
        return Err("the local user's lists are not fetched from the network".into());
    }

    let cached = state.storage.get_cached_remote_list(pubkey, kind).await?;
    if let Some(c) = &cached
        && cache_is_fresh(c.fetched_at)
    {
        return Ok((c.entries.clone(), c.hidden, true));
    }

    let node_ids = state
        .storage
        .get_peer_transport_node_ids(pubkey)
        .await
        .unwrap_or_default();
    let endpoint = state.endpoint();
    for node_id in node_ids.iter().take(MAX_NODE_IDS_TRIED) {
        let Ok(target) = node_id.parse() else {
            continue;
        };
        match crate::peer::fetch_remote_list(&endpoint, target, kind, pubkey).await {
            Ok(list) => {
                if let Err(e) = state
                    .storage
                    .cache_remote_list(pubkey, kind, &list.entries, list.hidden)
                    .await
                {
                    log::warn!("[remote-list] failed to cache list: {e}");
                }
                return Ok((list.entries, list.hidden, false));
            }
            Err(e) => {
                log::debug!("[remote-list] failed via {}: {e}", short_id(node_id));
            }
        }
    }

    match cached {
        Some(c) => Ok((c.entries, c.hidden, true)),
        None => Err("peer offline and no cached data".into()),
    }
}

#[tauri::command]
pub async fn get_remote_follows(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<RemoteFollowsResult> {
    let (follows, hidden, cached) = remote_list(&state, &pubkey, RemoteListKind::Follows).await?;
    Ok(RemoteFollowsResult {
        follows,
        hidden,
        cached,
    })
}

#[tauri::command]
pub async fn get_remote_followers(
    state: State<'_, Arc<AppState>>,
    pubkey: String,
) -> CmdResult<RemoteFollowersResult> {
    let (followers, hidden, cached) =
        remote_list(&state, &pubkey, RemoteListKind::Followers).await?;
    Ok(RemoteFollowersResult {
        followers,
        hidden,
        cached,
    })
}
