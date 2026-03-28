use super::SyncResult;
use crate::error::AppError;
use crate::framing::read_frame;
use crate::storage::Storage;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use proscenium_types::{PEER_ALPN, PeerRequest, SyncFrame, SyncMode, SyncRequest, short_id};

/// Client: sync posts from a remote peer using the three-phase protocol.
pub async fn sync_from_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    target: EndpointId,
    author: &str,
) -> Result<SyncResult, AppError> {
    let addr = EndpointAddr::from(target);
    log::info!(
        "[sync-client] connecting to {} for sync...",
        short_id(author)
    );
    let start = std::time::Instant::now();
    let conn = match endpoint.connect(addr, PEER_ALPN).await {
        Ok(c) => {
            log::info!(
                "[sync-client] connected to {} in {:.1}s",
                short_id(author),
                start.elapsed().as_secs_f64(),
            );
            c
        }
        Err(e) => {
            log::error!(
                "[sync-client] failed to connect to {} after {:.1}s: {e:?}",
                short_id(author),
                start.elapsed().as_secs_f64(),
            );
            return Err(e.into());
        }
    };

    // Phase 1: Send PeerRequest::Sync
    let (mut send, mut recv) = conn.open_bi().await?;

    let post_count = storage.count_posts_by_author(author).await.unwrap_or(0);
    let interaction_count = storage
        .count_interactions_by_author(author)
        .await
        .unwrap_or(0);
    let newest_timestamp = storage.newest_post_timestamp(author).await.unwrap_or(0);
    let newest_interaction_timestamp = storage
        .newest_interaction_timestamp(author)
        .await
        .unwrap_or(0);

    let req = SyncRequest {
        author: author.to_string(),
        post_count,
        interaction_count,
        newest_timestamp,
        newest_interaction_timestamp,
    };
    let peer_req = PeerRequest::Sync(req);
    let req_bytes = serde_json::to_vec(&peer_req)?;
    send.write_all(&req_bytes).await?;
    send.finish()?;

    // Read Phase 1 summary
    let summary_bytes = recv.read_to_end(65_536).await?;
    let summary: proscenium_types::SyncSummary = serde_json::from_slice(&summary_bytes)?;

    log::info!(
        "[sync-client] {} mode={:?}, remote=({}/{}), local=({}/{})",
        short_id(author),
        summary.mode,
        summary.server_post_count,
        summary.server_interaction_count,
        post_count,
        interaction_count,
    );

    if summary.mode == SyncMode::UpToDate {
        conn.close(0u32.into(), b"done");
        return Ok(SyncResult {
            posts: Vec::new(),
            interactions: Vec::new(),
            profile: summary.profile,
            remote_post_count: summary.server_post_count,
            mode: SyncMode::UpToDate,
            active_stage: summary.active_stage,
        });
    }

    // Phase 2/3: Open data stream
    let (mut data_send, mut data_recv) = conn.open_bi().await?;

    if summary.mode == SyncMode::NeedIdDiff {
        let known_ids = storage
            .get_post_ids_by_author(author)
            .await
            .unwrap_or_default();
        let ids_bytes = serde_json::to_vec(&known_ids)?;
        data_send.write_all(&ids_bytes).await?;
    }
    data_send.finish()?;

    // Read streamed frames
    let mut all_posts = Vec::new();
    let mut all_interactions = Vec::new();
    loop {
        match read_frame(&mut data_recv).await {
            Ok(Some(frame_bytes)) => {
                let frame: SyncFrame = serde_json::from_slice(&frame_bytes)?;
                match frame {
                    SyncFrame::Posts(posts) => all_posts.extend(posts),
                    SyncFrame::Interactions(interactions) => {
                        all_interactions.extend(interactions);
                    }
                    SyncFrame::DeviceAnnouncements(announcements) => {
                        for announcement in announcements {
                            if announcement.master_pubkey != author {
                                continue;
                            }
                            if proscenium_types::verify_linked_devices_announcement(&announcement)
                                .is_err()
                            {
                                continue;
                            }
                            if let Err(e) = storage
                                .cache_peer_device_announcement(author, &announcement)
                                .await
                            {
                                log::error!(
                                    "[sync-client] failed to cache device announcement: {e}"
                                );
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                log::error!(
                    "[sync-client] frame read error from {}: {e:?}",
                    short_id(author)
                );
                break;
            }
        }
    }

    log::info!(
        "[sync-client] received {} posts, {} interactions from {} in {:.1}s (mode={:?})",
        all_posts.len(),
        all_interactions.len(),
        short_id(author),
        start.elapsed().as_secs_f64(),
        summary.mode,
    );

    conn.close(0u32.into(), b"done");

    Ok(SyncResult {
        posts: all_posts,
        interactions: all_interactions,
        profile: summary.profile,
        remote_post_count: summary.server_post_count,
        mode: summary.mode,
        active_stage: summary.active_stage,
    })
}
