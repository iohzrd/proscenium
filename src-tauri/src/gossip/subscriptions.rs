use crate::constants::{GOSSIP_DISCARD_SYNC_INTERVAL, GOSSIP_REJOIN_INTERVAL};
use crate::error::AppError;
use crate::state::{AppState, SyncCommand};
use crate::storage::Storage;
use futures_lite::StreamExt;
use iroh::EndpointId;
use iroh_gossip::{Event, GossipSender};
use proscenium_types::{GossipSenderBudgets, short_id, user_feed_topic};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager};

/// Parse cached transport NodeId strings; entries that do not parse are skipped.
fn parse_node_ids(node_ids: &[String]) -> Vec<EndpointId> {
    node_ids.iter().filter_map(|id| id.parse().ok()).collect()
}

/// NodeIds of the cached device list of `pubkey`: their messages on the user's topic have
/// a budget of their own in `GossipSenderBudgets`.
async fn author_nodes(storage: &Storage, pubkey: &str) -> HashSet<EndpointId> {
    let node_ids = storage
        .get_peer_transport_node_ids(pubkey)
        .await
        .unwrap_or_default();
    parse_node_ids(&node_ids).into_iter().collect()
}

/// Queue a `SyncCommand::SyncPeer` for `pubkey` on the peer sync task. Does nothing if
/// the application state is not managed yet or the command queue is full.
fn queue_sync(app_handle: &AppHandle, pubkey: &str) {
    if let Some(state) = app_handle.try_state::<Arc<AppState>>()
        && state
            .sync_tx()
            .try_send(SyncCommand::SyncPeer(pubkey.to_string()))
            .is_err()
    {
        log::debug!(
            "[gossip-rx] sync queue full, {} not queued",
            short_id(pubkey)
        );
    }
}

/// Call `join_peers` on the topic of `pubkey` with its cached transport NodeIds.
async fn rejoin(storage: &Storage, sender: &GossipSender, pubkey: &str) {
    let node_ids = storage
        .get_peer_transport_node_ids(pubkey)
        .await
        .unwrap_or_default();
    let peers = parse_node_ids(&node_ids);
    if peers.is_empty() {
        return;
    }
    log::debug!(
        "[gossip-rx] no neighbors for {}, joining {} cached node(s)",
        short_id(pubkey),
        peers.len()
    );
    if let Err(e) = sender.join_peers(peers).await {
        log::warn!(
            "[gossip-rx] join_peers for {} failed: {e}",
            short_id(pubkey)
        );
    }
}

impl super::GossipService {
    /// Subscribe to a user's gossip feed topic.
    /// `pubkey` is the master pubkey (permanent identity).
    /// `transport_node_ids` are the transport NodeIds to use as gossip bootstrap peers.
    ///
    /// The subscription task handles received messages. While the topic has no neighbor
    /// it calls `join_peers` with the cached NodeIds every `GOSSIP_REJOIN_INTERVAL`,
    /// because iroh-gossip does not dial a bootstrap peer again after a failed attempt.
    pub async fn follow_user(
        &self,
        pubkey: String,
        transport_node_ids: Vec<String>,
    ) -> Result<(), AppError> {
        let mut inner = self.inner.lock().await;

        // Skip if already subscribed with a live task; clean up finished handles.
        if let Some(handle) = inner.subscriptions.get(&pubkey) {
            if !handle.is_finished() {
                log::info!("[gossip] already subscribed to {}", short_id(&pubkey));
                return Ok(());
            }
            inner.subscriptions.remove(&pubkey);
        }

        let topic = user_feed_topic(&pubkey);
        let bootstrap = parse_node_ids(&transport_node_ids);

        if bootstrap.is_empty() {
            return Err(AppError::Other(format!(
                "no valid transport NodeIds for gossip bootstrap of {}",
                short_id(&pubkey)
            )));
        }

        log::info!(
            "[gossip] subscribing to {} with {} bootstrap nodes",
            short_id(&pubkey),
            bootstrap.len(),
        );
        let topic_handle = self.gossip.subscribe(topic, bootstrap).await?;
        let (sender, mut receiver) = topic_handle.split();

        let storage = self.storage.clone();
        let pk = pubkey.clone();
        let my_id = self.identity.read().await.master_pubkey.clone();
        let app_handle = self.app_handle.clone();
        let reconnect_tx = self.reconnect_tx.clone();
        let feed_neighbors = self.feed_neighbors.clone();

        let handle = tokio::spawn(async move {
            log::info!("[gossip-rx] listener started for {}", short_id(&pk));
            // Any NodeId on the topic can send; each sender's messages beyond its budget
            // are discarded before they are parsed and verified.
            let mut budgets =
                GossipSenderBudgets::new(author_nodes(&storage, &pk).await, Instant::now());
            // When the last sync was queued for discarded messages.
            let mut discard_sync_at: Option<Instant> = None;
            let mut rejoin_timer = tokio::time::interval(GOSSIP_REJOIN_INTERVAL);
            rejoin_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick of an interval completes immediately; the subscribe call
            // above already dialed the bootstrap nodes.
            rejoin_timer.tick().await;
            loop {
                tokio::select! {
                    event = receiver.try_next() => match event {
                        Ok(Some(Event::Received(msg))) => {
                            let now = Instant::now();
                            if !budgets.take(msg.delivered_from, now) {
                                // The sync fetches the author's content that was discarded.
                                if discard_sync_at.is_none_or(|at| {
                                    now.duration_since(at) >= GOSSIP_DISCARD_SYNC_INTERVAL
                                }) {
                                    discard_sync_at = Some(now);
                                    log::warn!(
                                        "[gossip-rx] discarding messages of {} on the topic of {}: message rate limit of the sender reached; queueing a sync",
                                        msg.delivered_from.fmt_short(),
                                        short_id(&pk)
                                    );
                                    queue_sync(&app_handle, &pk);
                                }
                                continue;
                            }
                            Self::handle_follow_message(
                                &storage,
                                &pk,
                                &my_id,
                                &app_handle,
                                &msg.content,
                            )
                            .await;
                        }
                        Ok(Some(Event::NeighborUp(_))) => {
                            feed_neighbors.set(&pk, receiver.neighbors().collect());
                            log::info!(
                                "[gossip-rx] neighbor up for {} ({} neighbors)",
                                short_id(&pk),
                                receiver.neighbors().count()
                            );
                        }
                        Ok(Some(Event::NeighborDown(_))) => {
                            feed_neighbors.set(&pk, receiver.neighbors().collect());
                            log::info!(
                                "[gossip-rx] neighbor down for {} ({} neighbors)",
                                short_id(&pk),
                                receiver.neighbors().count()
                            );
                        }
                        Ok(Some(Event::Lagged)) => {
                            // Events were discarded, including messages and possibly
                            // neighbor changes: fetch the missed content by sync and
                            // dial the cached nodes again.
                            log::warn!(
                                "[gossip-rx] receiver for {} lagged, queueing a sync",
                                short_id(&pk)
                            );
                            feed_neighbors.set(&pk, receiver.neighbors().collect());
                            queue_sync(&app_handle, &pk);
                            budgets.set_author_nodes(author_nodes(&storage, &pk).await);
                            rejoin(&storage, &sender, &pk).await;
                        }
                        // Events a later iroh-gossip release adds.
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            log::warn!("[gossip-rx] stream ended for {}", short_id(&pk));
                            break;
                        }
                        Err(e) => {
                            log::error!("[gossip-rx] receiver error for {}: {e}", short_id(&pk));
                            break;
                        }
                    },
                    _ = rejoin_timer.tick() => {
                        if !receiver.is_joined() {
                            budgets.set_author_nodes(author_nodes(&storage, &pk).await);
                            rejoin(&storage, &sender, &pk).await;
                        }
                    }
                }
            }
            feed_neighbors.remove(&pk);
            log::warn!(
                "[gossip-rx] stream for {} ended, requesting reconnect",
                short_id(&pk)
            );
            let _ = reconnect_tx.send(pk);
        });

        inner.subscriptions.insert(pubkey, handle);
        Ok(())
    }

    pub async fn unfollow_user(&self, pubkey: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(handle) = inner.subscriptions.remove(pubkey) {
            log::info!("[gossip] unsubscribed from {}", short_id(pubkey));
            handle.abort();
        }
        self.feed_neighbors.remove(pubkey);
    }

    pub async fn get_subscription_count(&self) -> usize {
        self.inner.lock().await.subscriptions.len()
    }
}
