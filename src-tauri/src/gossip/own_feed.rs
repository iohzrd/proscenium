use crate::constants::{
    OWN_FEED_FOLLOWER_WINDOW, OWN_FEED_NEW_FOLLOWERS_PER_WINDOW, OWN_FEED_RESOLVE_CONCURRENCY,
    OWN_FEED_RESOLVE_QUEUE,
};
use crate::error::AppError;
use crate::storage::Storage;
use futures_lite::StreamExt;
use iroh::{Endpoint, EndpointId};
use iroh_gossip::{Event, GossipReceiver};
use proscenium_types::{VerifiedIdentity, Visibility, now_millis, short_id, user_feed_topic};
use std::collections::VecDeque;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// Counts new follower rows per `OWN_FEED_FOLLOWER_WINDOW`.
struct NewFollowerLimit {
    window_start: Instant,
    count: u32,
}

impl NewFollowerLimit {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            count: 0,
        }
    }

    /// Returns `true` and counts one new follower if fewer than
    /// `OWN_FEED_NEW_FOLLOWERS_PER_WINDOW` were counted in the current window.
    fn try_take(&mut self) -> bool {
        if self.window_start.elapsed() >= OWN_FEED_FOLLOWER_WINDOW {
            self.window_start = Instant::now();
            self.count = 0;
        }
        if self.count >= OWN_FEED_NEW_FOLLOWERS_PER_WINDOW {
            return false;
        }
        self.count += 1;
        true
    }
}

type Resolution = (EndpointId, Result<VerifiedIdentity, AppError>);

/// Receives neighbor events of the local user's own feed topic.
///
/// A neighbor is a follower's device. Its identity is queried in a separate task, at most
/// `OWN_FEED_RESOLVE_CONCURRENCY` at a time with `OWN_FEED_RESOLVE_QUEUE` waiting, so a
/// node that does not answer does not delay other events. Messages received on the topic
/// are ignored: content of the local user is never taken from the network.
async fn run_own_feed_listener(
    mut receiver: GossipReceiver,
    storage: std::sync::Arc<Storage>,
    endpoint: Endpoint,
    app_handle: AppHandle,
    my_id: String,
) {
    log::info!("[gossip-own] listener started for own feed neighbors");
    let mut resolutions: JoinSet<Resolution> = JoinSet::new();
    let mut waiting: VecDeque<EndpointId> = VecDeque::new();
    let mut new_followers = NewFollowerLimit::new();

    loop {
        tokio::select! {
            event = receiver.try_next() => match event {
                Ok(Some(Event::NeighborUp(endpoint_id))) => {
                    if waiting.len() >= OWN_FEED_RESOLVE_QUEUE {
                        log::warn!(
                            "[gossip-own] identity query queue full, ignoring neighbor {}",
                            endpoint_id.fmt_short()
                        );
                    } else if !waiting.contains(&endpoint_id) {
                        waiting.push_back(endpoint_id);
                    }
                }
                Ok(Some(Event::NeighborDown(endpoint_id))) => {
                    waiting.retain(|id| *id != endpoint_id);
                    let transport_id = endpoint_id.to_string();
                    // An unresolved neighbor has no follower row.
                    if let Some(pubkey) =
                        storage.get_master_pubkey_for_transport(&transport_id).await
                    {
                        if let Err(e) = storage.set_follower_offline(&my_id, &pubkey).await {
                            log::error!("[gossip-own] failed to update follower: {e}");
                        }
                        let _ = app_handle.emit("follower-changed", &pubkey);
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    log::warn!("[gossip-own] own feed stream ended");
                    break;
                }
                Err(e) => {
                    log::error!("[gossip-own] own feed receiver error: {e}");
                    break;
                }
            },
            Some(joined) = resolutions.join_next(), if !resolutions.is_empty() => {
                if let Ok((endpoint_id, result)) = joined {
                    record_follower(
                        &storage,
                        &app_handle,
                        &my_id,
                        endpoint_id,
                        result,
                        &mut new_followers,
                    )
                    .await;
                }
            }
        }

        while resolutions.len() < OWN_FEED_RESOLVE_CONCURRENCY {
            let Some(endpoint_id) = waiting.pop_front() else {
                break;
            };
            let endpoint = endpoint.clone();
            resolutions.spawn(async move {
                (
                    endpoint_id,
                    crate::peer::query_identity(&endpoint, endpoint_id).await,
                )
            });
        }
    }
}

/// Store the verified identity of a neighbor and record it as a follower.
///
/// A neighbor whose identity does not verify, or that is a device of the local user, is
/// not recorded. A master pubkey that is not yet a follower is recorded only while
/// `new_followers` allows it; that bounds the rows and "follower" notifications that
/// nodes joining the topic can create.
async fn record_follower(
    storage: &Storage,
    app_handle: &AppHandle,
    my_id: &str,
    endpoint_id: EndpointId,
    result: Result<VerifiedIdentity, AppError>,
    new_followers: &mut NewFollowerLimit,
) {
    let identity = match result {
        Ok(identity) => identity,
        Err(e) => {
            log::debug!(
                "[gossip-own] identity of neighbor {} not verified: {e}",
                endpoint_id.fmt_short()
            );
            return;
        }
    };
    let pubkey = identity.master_pubkey.clone();
    if pubkey == my_id {
        return;
    }
    match storage.is_blocked(&pubkey).await {
        Ok(false) => {}
        Ok(true) => return,
        Err(e) => {
            log::error!("[gossip-own] moderation lookup failed: {e}");
            return;
        }
    }
    let known = match storage.is_follower(my_id, &pubkey).await {
        Ok(known) => known,
        Err(e) => {
            log::error!("[gossip-own] follower lookup failed: {e}");
            return;
        }
    };
    if !known && !new_followers.try_take() {
        log::warn!(
            "[gossip-own] new follower limit reached, not recording {}",
            short_id(&pubkey)
        );
        return;
    }
    if let Err(e) = storage.cache_peer_identity(&identity).await {
        log::warn!(
            "[gossip-own] not caching identity of {}: {e}",
            short_id(&pubkey)
        );
        return;
    }

    let now = now_millis();
    match storage.upsert_follower(my_id, &pubkey, now).await {
        Ok(is_new) => {
            let _ = app_handle.emit("follower-changed", &pubkey);
            if is_new {
                log::info!("[gossip-own] new follower {}", short_id(&pubkey));
                let _ = storage
                    .insert_notification("follower", &pubkey, None, None, now)
                    .await;
                let _ = app_handle.emit("new-follower", &pubkey);
                let _ = app_handle.emit("notification-received", ());
            }
        }
        Err(e) => log::error!("[gossip-own] failed to store follower: {e}"),
    }
}

impl super::GossipService {
    pub async fn stop_own_feed(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.own_feed_handle.take() {
            handle.abort();
            inner.my_sender = None;
            log::info!("[gossip] stopped own feed topic");
        }
    }

    /// Open the own feed topic without reading the visibility. The caller has established
    /// that the local user is, or is becoming, Public.
    pub(super) async fn start_own_feed_unconditional(&self) -> Result<(), AppError> {
        let mut inner = self.inner.lock().await;
        if inner
            .own_feed_handle
            .as_ref()
            .is_some_and(|h| !h.is_finished())
        {
            return Ok(());
        }

        let my_id = self.identity.read().await.master_pubkey.clone();
        let topic = user_feed_topic(&my_id);
        log::info!("[gossip] starting own feed topic for {}", short_id(&my_id));

        let topic_handle = self.gossip.subscribe(topic, vec![]).await?;
        let (sender, receiver) = topic_handle.split();
        inner.my_sender = Some(sender);

        inner.own_feed_handle = Some(tokio::spawn(run_own_feed_listener(
            receiver,
            self.storage.clone(),
            self.endpoint.clone(),
            self.app_handle.clone(),
            my_id,
        )));
        Ok(())
    }

    /// Open the own feed topic if the stored visibility is Public, and close it otherwise.
    /// Used after a profile from another device of the user was stored.
    pub async fn apply_stored_visibility(&self) -> Result<(), AppError> {
        if self.my_visibility().await? == Visibility::Public {
            self.start_own_feed_unconditional().await
        } else {
            self.stop_own_feed().await;
            Ok(())
        }
    }

    /// Open the own feed topic if the local user's visibility is Public.
    /// Returns an error, and opens nothing, if the visibility cannot be read.
    pub async fn start_own_feed(&self) -> Result<(), AppError> {
        let visibility = self.my_visibility().await?;
        if visibility != Visibility::Public {
            log::info!("[gossip] skipping own feed topic (visibility={visibility})");
            return Ok(());
        }
        self.start_own_feed_unconditional().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_follower_limit_stops_at_the_window_cap() {
        let mut limit = NewFollowerLimit::new();
        for _ in 0..OWN_FEED_NEW_FOLLOWERS_PER_WINDOW {
            assert!(limit.try_take());
        }
        assert!(!limit.try_take());
        assert!(!limit.try_take());
    }
}
