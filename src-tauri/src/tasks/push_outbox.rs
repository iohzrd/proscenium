use crate::constants::PUSH_OUTBOX_FLUSH_INTERVAL;
use crate::push;
use crate::storage::Storage;
use iroh::Endpoint;
use iroh_social_types::short_id;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub async fn push_outbox_flush_task(
    endpoint: Endpoint,
    storage: Arc<Storage>,
    my_id: String,
    token: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(PUSH_OUTBOX_FLUSH_INTERVAL) => {}
        }

        let peers = match storage.get_push_outbox_peers().await {
            Ok(p) => p,
            Err(e) => {
                log::error!("[push-outbox] failed to get peers: {e}");
                continue;
            }
        };
        for peer in peers {
            let peer_node_ids = storage
                .get_peer_transport_node_ids(&peer)
                .await
                .unwrap_or_default();
            let targets: Vec<iroh::EndpointId> = peer_node_ids
                .iter()
                .filter_map(|id| id.parse().ok())
                .collect();
            if targets.is_empty() {
                continue;
            }

            let profile_entries = storage
                .get_pending_push_profile_ids(&peer)
                .await
                .unwrap_or_default();
            let post_entries = storage
                .get_pending_push_post_ids(&peer)
                .await
                .unwrap_or_default();
            let interaction_entries = storage
                .get_pending_push_interaction_ids(&peer)
                .await
                .unwrap_or_default();

            if profile_entries.is_empty()
                && post_entries.is_empty()
                && interaction_entries.is_empty()
            {
                continue;
            }

            let mut posts = Vec::new();
            let mut post_outbox_ids = Vec::new();
            for (outbox_id, post_id) in &post_entries {
                if let Ok(Some(post)) = storage.get_post_by_id(post_id).await {
                    posts.push(post);
                }
                post_outbox_ids.push(*outbox_id);
            }

            let mut interactions = Vec::new();
            let mut interaction_outbox_ids = Vec::new();
            for (outbox_id, interaction_id) in &interaction_entries {
                if let Ok(Some(interaction)) = storage.get_interaction_by_id(interaction_id).await {
                    interactions.push(interaction);
                }
                interaction_outbox_ids.push(*outbox_id);
            }

            let profile = if !profile_entries.is_empty() {
                storage.get_profile(&my_id).await.ok().flatten()
            } else {
                None
            };

            let msg = iroh_social_types::PushMessage {
                author: my_id.clone(),
                posts,
                interactions,
                profile,
            };

            let mut all_ids: Vec<i64> = post_outbox_ids
                .iter()
                .chain(interaction_outbox_ids.iter())
                .copied()
                .collect();
            all_ids.extend_from_slice(&profile_entries);

            let mut delivered = false;
            for target in &targets {
                match push::push_to_peer(&endpoint, *target, &msg).await {
                    Ok(ack) => {
                        log::info!(
                            "[push-outbox] delivered {} posts, {} interactions to {}{}",
                            ack.received_post_ids.len(),
                            ack.received_interaction_ids.len(),
                            short_id(&peer),
                            if profile_entries.is_empty() {
                                ""
                            } else {
                                " (+ profile)"
                            },
                        );
                        delivered = true;
                        break;
                    }
                    Err(e) => {
                        log::debug!(
                            "[push-outbox] failed to push to {} device: {e}",
                            short_id(&peer)
                        );
                    }
                }
            }
            if delivered {
                let _ = storage.remove_push_outbox_entries(&all_ids).await;
            } else {
                log::error!(
                    "[push-outbox] failed to push to {} (tried {} devices)",
                    short_id(&peer),
                    targets.len()
                );
                let _ = storage.mark_push_attempted(&all_ids).await;
            }
        }
    }
}
