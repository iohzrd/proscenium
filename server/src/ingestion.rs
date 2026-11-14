use crate::config::Config;
use crate::node::Node;
use crate::storage::{
    AnnouncementOutcome, ChangeError, ChangeOutcome, RegistrationChange, RegistrationPolicy,
    Storage,
};
use crate::sync::SyncHealth;
use futures_lite::StreamExt;
use iroh::{Endpoint, EndpointId};
use iroh_gossip::Gossip;
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use proscenium_types::{
    GossipMessage, GossipSenderBudgets, Interaction, Post, Visibility, is_valid_id, now_millis,
    short_id, truncate_chars, user_feed_topic, validate_interaction, validate_post,
    validate_profile, verify_delete_interaction_signature, verify_delete_post_signature,
    verify_interaction_signature, verify_linked_devices_announcement, verify_post_signature,
    verify_profile_signature,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

/// Delay before the first `join_peers` call on a topic without neighbors. Doubles after
/// each call up to `REJOIN_MAX_DELAY`.
const REJOIN_MIN_DELAY: Duration = Duration::from_secs(15);
const REJOIN_MAX_DELAY: Duration = Duration::from_secs(600);
/// Delay before subscribing again after the gossip event stream ended or a subscribe
/// call failed.
const RESUBSCRIBE_MIN_DELAY: Duration = Duration::from_secs(1);
const RESUBSCRIBE_MAX_DELAY: Duration = Duration::from_secs(60);
/// Minimum time between two syncs requested by one subscription task.
const EVENT_SYNC_MIN_INTERVAL: Duration = Duration::from_secs(60);
/// Rejected gossip messages logged per topic per `REJECT_LOG_WINDOW`.
const REJECT_LOGS_PER_WINDOW: u32 = 5;
const REJECT_LOG_WINDOW: Duration = Duration::from_secs(60);
/// Characters of a rejection reason written to the log.
const REJECT_REASON_LOG_CHARS: usize = 200;

/// Limits applied to content received over gossip and sync.
#[derive(Debug, Clone, Copy)]
pub struct IngestLimits {
    pub max_posts_per_user: u32,
    pub max_interactions_per_user: u32,
    /// Posts and interactions older than this many milliseconds are not stored.
    /// `None` disables the check.
    pub retention_ms: Option<u64>,
}

/// A spawned task. The boxed type states `Send` explicitly: the gossip task and the sync
/// task each reach `reconcile_subscription`, which spawns both, so the compiler cannot
/// infer `Send` for their unboxed futures.
type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

struct Subscription {
    handle: JoinHandle<()>,
    /// `registrations.transport_node_id` when the task was started.
    transport_node_id: Option<String>,
}

pub struct IngestionManager {
    gossip: Gossip,
    pub(crate) endpoint: Endpoint,
    pub(crate) storage: Arc<Storage>,
    pub(crate) limits: IngestLimits,
    policy: RegistrationPolicy,
    /// One gossip task per master pubkey with an active public registration.
    subscriptions: Mutex<HashMap<String, Subscription>>,
    /// Permits of syncs of users for which `SyncHealth::is_proven` holds:
    /// `sync.max_concurrent_syncs`.
    pub(crate) sync_permits: Semaphore,
    /// Permits of syncs of the other users: a quarter of `sync.max_concurrent_syncs`, at
    /// least one. Registrations whose devices do not answer, or answer slowly, cost only
    /// these.
    pub(crate) unproven_sync_permits: Semaphore,
    /// Master pubkeys with a sync queued or running.
    pub(crate) syncing: std::sync::Mutex<HashSet<String>>,
    /// Outcomes of recent syncs per master pubkey with an active public registration.
    pub(crate) sync_health: std::sync::Mutex<HashMap<String, SyncHealth>>,
}

/// Writes at most `REJECT_LOGS_PER_WINDOW` warn records per window for one topic and
/// reports the number of unlogged rejections when the next window starts.
struct RejectLog {
    window_start: Instant,
    logged: u32,
    suppressed: u32,
}

impl RejectLog {
    fn new() -> Self {
        Self {
            window_start: Instant::now(),
            logged: 0,
            suppressed: 0,
        }
    }

    fn record(&mut self, topic_owner: &str, reason: &str) {
        if self.window_start.elapsed() >= REJECT_LOG_WINDOW {
            if self.suppressed > 0 {
                tracing::warn!(
                    "[ingestion] {} more rejected gossip messages for {} not logged",
                    self.suppressed,
                    short_id(topic_owner)
                );
            }
            *self = Self::new();
        }
        if self.logged < REJECT_LOGS_PER_WINDOW {
            self.logged += 1;
            // `{:?}` escapes control characters of peer-supplied text.
            tracing::warn!(
                "[ingestion] rejected gossip message for {}: {:?}",
                short_id(topic_owner),
                truncate_chars(reason, REJECT_REASON_LOG_CHARS)
            );
        } else {
            self.suppressed += 1;
        }
    }
}

impl IngestionManager {
    pub fn new(node: &Node, storage: Arc<Storage>, config: &Config) -> Arc<Self> {
        let retention_ms = (config.limits.retention_days > 0).then(|| {
            config
                .limits
                .retention_days
                .saturating_mul(24 * 60 * 60 * 1000)
        });
        Arc::new(Self {
            gossip: node.gossip.clone(),
            endpoint: node.endpoint.clone(),
            storage,
            limits: IngestLimits {
                max_posts_per_user: config.limits.max_posts_per_user,
                max_interactions_per_user: config.limits.max_interactions_per_user,
                retention_ms,
            },
            policy: RegistrationPolicy {
                registration_open: config.server.registration_open,
                max_registered_users: config.limits.max_registered_users,
            },
            subscriptions: Mutex::new(HashMap::new()),
            sync_permits: Semaphore::new(config.sync.max_concurrent_syncs),
            unproven_sync_permits: Semaphore::new((config.sync.max_concurrent_syncs / 4).max(1)),
            syncing: std::sync::Mutex::new(HashSet::new()),
            sync_health: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn node_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Apply a registration, unregistration or profile change from any source.
    ///
    /// Runs `Storage::apply_registration_change` (one transaction: row update and
    /// deletion of stored content when the user stops being public), then starts, stops
    /// or restarts the gossip task of the user to match the stored row.
    pub async fn apply_registration_change(
        self: &Arc<Self>,
        change: RegistrationChange<'_>,
    ) -> Result<ChangeOutcome, ChangeError> {
        let outcome = self
            .storage
            .apply_registration_change(&change, &self.policy)
            .await?;
        self.reconcile_subscription(change.master_pubkey(), true)
            .await;
        Ok(outcome)
    }

    /// Make the gossip task of `pubkey` match its stored registration.
    ///
    /// Without an active public registration, the task of `pubkey` is aborted. With
    /// one, a task is started if none is running, and the running task is replaced if
    /// the stored transport NodeId differs from the one it was started with. The map
    /// lock is held from the storage read to the map update, so two calls for one user
    /// cannot both start a task.
    async fn reconcile_subscription(self: &Arc<Self>, pubkey: &str, sync_on_start: bool) {
        let mut subscriptions = self.subscriptions.lock().await;
        let target = match self.storage.get_subscription_target(pubkey).await {
            Ok(target) => target,
            Err(e) => {
                tracing::error!(
                    "[ingestion] failed to read registration of {}: {e}",
                    short_id(pubkey)
                );
                return;
            }
        };

        let Some(transport_node_id) = target else {
            if let Some(subscription) = subscriptions.remove(pubkey) {
                subscription.handle.abort();
                tracing::info!("[ingestion] unsubscribed from {}", short_id(pubkey));
            }
            return;
        };

        let running = subscriptions
            .get(pubkey)
            .is_some_and(|s| !s.handle.is_finished() && s.transport_node_id == transport_node_id);
        if running {
            return;
        }
        if let Some(previous) = subscriptions.remove(pubkey) {
            previous.handle.abort();
        }
        let handle = tokio::spawn(self.clone().run_subscription(pubkey.to_string()));
        subscriptions.insert(
            pubkey.to_string(),
            Subscription {
                handle,
                transport_node_id,
            },
        );
        drop(subscriptions);
        tracing::info!("[ingestion] subscribed to {}", short_id(pubkey));
        if sync_on_start {
            self.request_sync(pubkey);
        }
    }

    /// NodeIds to dial for `pubkey`, read from storage: the registering device, then the
    /// devices of the newest announcement. The server's own NodeId is left out.
    pub(crate) async fn stored_node_ids(&self, pubkey: &str) -> Vec<EndpointId> {
        let node_ids = match self.storage.get_device_node_ids(pubkey).await {
            Ok(node_ids) => node_ids.unwrap_or_default(),
            Err(e) => {
                tracing::error!(
                    "[ingestion] failed to read NodeIds of {}: {e}",
                    short_id(pubkey)
                );
                return vec![];
            }
        };
        let own_id = self.endpoint.id();
        node_ids
            .iter()
            .filter_map(|id| id.parse::<EndpointId>().ok())
            .filter(|id| *id != own_id)
            .collect()
    }

    /// Gossip task of one user: subscribe to the feed topic and process its events.
    /// Subscribes again when the event stream ends. The delay before the next attempt
    /// doubles up to `RESUBSCRIBE_MAX_DELAY` and returns to `RESUBSCRIBE_MIN_DELAY`
    /// after a subscription that lasted at least `RESUBSCRIBE_MAX_DELAY`.
    fn run_subscription(self: Arc<Self>, pubkey: String) -> Task {
        Box::pin(async move {
            let topic = user_feed_topic(&pubkey);
            let mut delay = RESUBSCRIBE_MIN_DELAY;
            loop {
                let bootstrap = self.stored_node_ids(&pubkey).await;
                match self.gossip.subscribe(topic, bootstrap).await {
                    Ok(topic_handle) => {
                        let subscribed_at = Instant::now();
                        let (sender, mut receiver) = topic_handle.split();
                        self.process_topic_events(&pubkey, &sender, &mut receiver)
                            .await;
                        if subscribed_at.elapsed() >= RESUBSCRIBE_MAX_DELAY {
                            delay = RESUBSCRIBE_MIN_DELAY;
                        }
                        tracing::warn!(
                            "[ingestion] gossip stream ended for {}, subscribing again in {}s",
                            short_id(&pubkey),
                            delay.as_secs()
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "[ingestion] failed to subscribe to {}: {e}, next attempt in {}s",
                            short_id(&pubkey),
                            delay.as_secs()
                        );
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(RESUBSCRIBE_MAX_DELAY);
            }
        })
    }

    /// Process events of one topic subscription until its stream ends.
    ///
    /// While the topic has no neighbor, `join_peers` is called with the NodeIds stored
    /// at that time, first after `REJOIN_MIN_DELAY` and then with a doubling delay.
    async fn process_topic_events(
        self: &Arc<Self>,
        pubkey: &str,
        sender: &GossipSender,
        receiver: &mut GossipReceiver,
    ) {
        let mut reject_log = RejectLog::new();
        let mut budgets = GossipSenderBudgets::new(
            self.stored_node_ids(pubkey).await.into_iter().collect(),
            std::time::Instant::now(),
        );
        let mut rejoin_delay = REJOIN_MIN_DELAY;
        let mut rejoin_at = Instant::now() + rejoin_delay;
        let mut last_sync_request: Option<Instant> = None;

        loop {
            let has_neighbors = receiver.is_joined();
            tokio::select! {
                event = receiver.try_next() => match event {
                    Ok(Some(Event::Received(message))) => {
                        if !budgets.take(message.delivered_from, std::time::Instant::now()) {
                            reject_log.record(pubkey, "message rate limit of the sender reached");
                        } else if let Err(reason) =
                            self.handle_gossip_message(pubkey, &message.content).await
                        {
                            reject_log.record(pubkey, &reason);
                        }
                    }
                    Ok(Some(Event::NeighborUp(_))) => {
                        rejoin_delay = REJOIN_MIN_DELAY;
                        // Messages broadcast while the topic had no neighbor were not
                        // received; a sync fetches them.
                        if !has_neighbors {
                            self.request_event_sync(pubkey, &mut last_sync_request);
                        }
                    }
                    Ok(Some(Event::NeighborDown(_))) => {
                        if !receiver.is_joined() {
                            tracing::debug!(
                                "[ingestion] topic of {} has no neighbors",
                                short_id(pubkey)
                            );
                            rejoin_delay = REJOIN_MIN_DELAY;
                            rejoin_at = Instant::now() + rejoin_delay;
                        }
                    }
                    Ok(Some(Event::Lagged)) => {
                        tracing::warn!(
                            "[ingestion] missed gossip events for {}",
                            short_id(pubkey)
                        );
                        self.request_event_sync(pubkey, &mut last_sync_request);
                    }
                    // Events a later iroh-gossip release adds.
                    Ok(Some(_)) => {}
                    Ok(None) => return,
                    Err(e) => {
                        tracing::error!("[ingestion] gossip error for {}: {e}", short_id(pubkey));
                        return;
                    }
                },
                _ = tokio::time::sleep_until(rejoin_at), if !has_neighbors => {
                    let bootstrap = self.stored_node_ids(pubkey).await;
                    budgets.set_author_nodes(bootstrap.iter().copied().collect());
                    if !bootstrap.is_empty()
                        && let Err(e) = sender.join_peers(bootstrap).await
                    {
                        tracing::error!(
                            "[ingestion] join_peers failed for {}: {e}",
                            short_id(pubkey)
                        );
                        return;
                    }
                    rejoin_delay = (rejoin_delay * 2).min(REJOIN_MAX_DELAY);
                    rejoin_at = Instant::now() + rejoin_delay;
                }
            }
        }
    }

    /// Request a sync unless this task requested one within `EVENT_SYNC_MIN_INTERVAL`.
    fn request_event_sync(self: &Arc<Self>, pubkey: &str, last_request: &mut Option<Instant>) {
        if last_request.is_some_and(|at| at.elapsed() < EVENT_SYNC_MIN_INTERVAL) {
            return;
        }
        *last_request = Some(Instant::now());
        self.request_sync(pubkey);
    }

    /// Start a task that syncs `pubkey` once a sync permit is free.
    pub(crate) fn request_sync(self: &Arc<Self>, pubkey: &str) {
        tokio::spawn(self.clone().sync_task(pubkey.to_string()));
    }

    fn sync_task(self: Arc<Self>, pubkey: String) -> Task {
        Box::pin(async move { self.sync_user(&pubkey).await })
    }

    /// Lowest timestamp of content that is stored, from the retention setting.
    pub(crate) fn min_timestamp(&self) -> u64 {
        self.limits
            .retention_ms
            .map_or(0, |retention| now_millis().saturating_sub(retention))
    }

    /// Check that a post names `author`, passes `validate_post` and is inside the
    /// retention period. Does not verify the signature.
    pub(crate) fn check_post_fields(&self, post: &Post, author: &str) -> Result<(), String> {
        if post.author != author {
            return Err("post author differs from the expected author".to_string());
        }
        validate_post(post)?;
        if post.timestamp < self.min_timestamp() {
            return Err("post is older than the retention period".to_string());
        }
        Ok(())
    }

    /// Check that an interaction names `author`, passes `validate_interaction` and is
    /// inside the retention period. Does not verify the signature.
    pub(crate) fn check_interaction_fields(
        &self,
        interaction: &Interaction,
        author: &str,
    ) -> Result<(), String> {
        if interaction.author != author {
            return Err("interaction author differs from the expected author".to_string());
        }
        validate_interaction(interaction)?;
        if interaction.timestamp < self.min_timestamp() {
            return Err("interaction is older than the retention period".to_string());
        }
        Ok(())
    }

    /// Verify one message received on the feed topic of `topic_owner` and apply it.
    /// Content is applied only if its author is `topic_owner` and its signature was made
    /// by the key `topic_owner`. Returns the reason if the message was rejected.
    async fn handle_gossip_message(
        self: &Arc<Self>,
        topic_owner: &str,
        data: &[u8],
    ) -> Result<(), String> {
        let message: GossipMessage =
            serde_json::from_slice(data).map_err(|e| format!("unparseable message: {e}"))?;
        let storage_error = |e: anyhow::Error| {
            tracing::error!(
                "[ingestion] storage error for {}: {e}",
                short_id(topic_owner)
            );
        };

        match message {
            GossipMessage::NewPost(post) => {
                self.check_post_fields(&post, topic_owner)?;
                verify_post_signature(&post)?;
                match self
                    .storage
                    .insert_posts(
                        topic_owner,
                        std::slice::from_ref(&post),
                        self.limits.max_posts_per_user,
                    )
                    .await
                {
                    Ok(count) if count.stored > 0 => tracing::info!(
                        "[ingestion] stored post {} from {}",
                        post.id,
                        short_id(topic_owner)
                    ),
                    Ok(count) if count.over_limit > 0 => {
                        return Err("post limit of the user is reached".to_string());
                    }
                    Ok(_) => {}
                    Err(e) => storage_error(e),
                }
            }
            GossipMessage::DeletePost {
                id,
                author,
                signature,
            } => {
                if author != topic_owner {
                    return Err("delete-post author is not the topic owner".to_string());
                }
                if !is_valid_id(&id) {
                    return Err("invalid post id in delete-post".to_string());
                }
                verify_delete_post_signature(&id, &author, &signature)?;
                match self.storage.apply_tombstones(topic_owner, &[&id]).await {
                    Ok(0) => {}
                    Ok(_) => tracing::info!(
                        "[ingestion] deleted post {id} from {}",
                        short_id(topic_owner)
                    ),
                    Err(e) => storage_error(e),
                }
            }
            GossipMessage::ProfileUpdate(profile) => {
                validate_profile(&profile)?;
                verify_profile_signature(&profile, topic_owner)?;
                let change = RegistrationChange::PeerProfile {
                    master_pubkey: topic_owner,
                    profile: &profile,
                };
                match self.apply_registration_change(change).await {
                    Ok(outcome) if !outcome.profile_stored => {
                        return Err("profile is not newer than the stored one".to_string());
                    }
                    Ok(outcome) => tracing::info!(
                        "[ingestion] stored profile of {} (active={}, visibility={})",
                        short_id(topic_owner),
                        outcome.is_active,
                        outcome.visibility
                    ),
                    Err(ChangeError::Rejected(rejection)) => return Err(rejection.to_string()),
                    Err(ChangeError::Storage(e)) => storage_error(e),
                }
            }
            GossipMessage::NewInteraction(interaction) => {
                self.check_interaction_fields(&interaction, topic_owner)?;
                verify_interaction_signature(&interaction)?;
                match self
                    .storage
                    .insert_interactions(
                        topic_owner,
                        std::slice::from_ref(&interaction),
                        self.limits.max_interactions_per_user,
                    )
                    .await
                {
                    Ok(count) if count.over_limit > 0 => {
                        return Err("interaction limit of the user is reached".to_string());
                    }
                    Ok(_) => {}
                    Err(e) => storage_error(e),
                }
            }
            GossipMessage::DeleteInteraction {
                id,
                author,
                signature,
            } => {
                if author != topic_owner {
                    return Err("delete-interaction author is not the topic owner".to_string());
                }
                if !is_valid_id(&id) {
                    return Err("invalid interaction id in delete-interaction".to_string());
                }
                verify_delete_interaction_signature(&id, &author, &signature)?;
                if let Err(e) = self.storage.delete_interaction(&id, &author).await {
                    storage_error(e);
                }
            }
            GossipMessage::LinkedDevices(announcement) => {
                if announcement.master_pubkey != topic_owner {
                    return Err("device announcement is not from the topic owner".to_string());
                }
                verify_linked_devices_announcement(&announcement)?;
                match self.storage.cache_announcement(&announcement).await {
                    Ok(AnnouncementOutcome::Stored) => tracing::info!(
                        "[ingestion] stored device announcement of {} v{} ({} devices)",
                        short_id(topic_owner),
                        announcement.version,
                        announcement.devices.len()
                    ),
                    Ok(AnnouncementOutcome::NotNewer) => {}
                    Err(ChangeError::Rejected(rejection)) => return Err(rejection.to_string()),
                    Err(ChangeError::Storage(e)) => storage_error(e),
                }
            }
            // Keep-alive from the publisher; nothing to store.
            GossipMessage::Heartbeat => {}
            // Stage events are not indexed by the server.
            GossipMessage::StageAnnouncement { .. } | GossipMessage::StageEnded { .. } => {}
        }
        Ok(())
    }

    /// Start a gossip task for every active public registration. With `startup_sync`,
    /// also request one sync per user.
    pub async fn start(self: &Arc<Self>, startup_sync: bool) {
        let pubkeys = match self.storage.get_active_public_pubkeys().await {
            Ok(pubkeys) => pubkeys,
            Err(e) => {
                tracing::error!("[ingestion] failed to get public users: {e}");
                return;
            }
        };
        tracing::info!("[ingestion] subscribing to {} public users", pubkeys.len());
        for pubkey in &pubkeys {
            self.reconcile_subscription(pubkey, startup_sync).await;
        }
    }

    /// Every `interval_minutes`: make the gossip task of every active public user and
    /// of every user with a registered task match storage, then sync every active
    /// public user whose retry delay has passed (`sync_user`). At most
    /// `sync.max_concurrent_syncs` syncs of proven users and a quarter of that of other
    /// users run at the same time.
    pub fn start_periodic_sync(self: &Arc<Self>, interval_minutes: u64) {
        let manager = self.clone();
        tokio::spawn(async move {
            let interval = Duration::from_secs(interval_minutes.saturating_mul(60));
            loop {
                tokio::time::sleep(interval).await;
                let pubkeys = match manager.storage.get_active_public_pubkeys().await {
                    Ok(pubkeys) => pubkeys,
                    Err(e) => {
                        tracing::error!("[periodic-sync] failed to get users: {e}");
                        continue;
                    }
                };

                // A task whose user is no longer public remains only if an earlier
                // storage read in `reconcile_subscription` failed.
                let public: HashSet<&str> = pubkeys.iter().map(String::as_str).collect();
                manager
                    .sync_health
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retain(|pubkey, _| public.contains(pubkey.as_str()));
                let not_public: Vec<String> = manager
                    .subscriptions
                    .lock()
                    .await
                    .keys()
                    .filter(|pubkey| !public.contains(pubkey.as_str()))
                    .cloned()
                    .collect();
                for pubkey in &not_public {
                    manager.reconcile_subscription(pubkey, false).await;
                }

                let mut syncs = JoinSet::new();
                for pubkey in pubkeys {
                    manager.reconcile_subscription(&pubkey, false).await;
                    syncs.spawn(manager.clone().sync_task(pubkey));
                }
                while let Some(result) = syncs.join_next().await {
                    if let Err(e) = result {
                        tracing::error!("[periodic-sync] sync task failed: {e}");
                    }
                }
            }
        });
    }

    /// True if a gossip task that has not finished is registered for `pubkey`.
    #[cfg(test)]
    pub(crate) async fn has_subscription(&self, pubkey: &str) -> bool {
        self.subscriptions
            .lock()
            .await
            .get(pubkey)
            .is_some_and(|s| !s.handle.is_finished())
    }

    /// Abort every gossip task.
    pub async fn shutdown(&self) {
        let mut subscriptions = self.subscriptions.lock().await;
        for (_, subscription) in subscriptions.drain() {
            subscription.handle.abort();
        }
    }
}

/// True if `outcome` describes an active registration with public visibility.
pub(crate) fn is_public(outcome: &ChangeOutcome) -> bool {
    outcome.is_active && outcome.visibility == Visibility::Public
}
