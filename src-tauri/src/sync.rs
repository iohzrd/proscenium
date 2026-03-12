use crate::constants::SYNC_TIMEOUT;
use crate::framing::{read_frame, write_frame};
use crate::storage::Storage;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, PublicKey, endpoint::Connection, protocol::AcceptError,
};
use iroh_social_types::{
    Interaction, PEER_ALPN, PeerRequest, Post, Profile, SyncFrame, SyncMode, SyncRequest,
    SyncSummary, Visibility, parse_mentions, short_id, validate_interaction, validate_post,
    verify_interaction_signature, verify_post_signature,
};
use tauri::{AppHandle, Emitter};

const BATCH_SIZE: usize = 200;

/// Server-side sync handler, dispatched from the unified PeerHandler.
/// The initial PeerRequest::Sync has already been read; `send` is the response side
/// of the first bi-stream, and `conn` is available for opening additional streams.
pub async fn handle_sync(
    storage: &Storage,
    node_id: &str,
    remote_str: &str,
    conn: &Connection,
    mut send: iroh::endpoint::SendStream,
    req: SyncRequest,
) -> Result<(), AcceptError> {
    // Resolve transport NodeId to master pubkey for access control checks
    let remote_pubkey = storage
        .get_master_pubkey_for_transport(remote_str)
        .await
        .unwrap_or_else(|| remote_str.to_string());

    // Enforce visibility-based sync access control
    let visibility = storage
        .get_visibility(node_id)
        .await
        .unwrap_or(Visibility::Public);
    match visibility {
        Visibility::Public => {} // anyone can sync
        Visibility::Listed => {
            if !storage.is_follower(&remote_pubkey).await.unwrap_or(false) {
                log::warn!(
                    "[sync-server] rejecting non-follower {} (listed profile)",
                    short_id(&remote_pubkey)
                );
                return Err(AcceptError::from_err(std::io::Error::other("listed")));
            }
        }
        Visibility::Private => {
            if !storage.is_mutual(&remote_pubkey).await.unwrap_or(false) {
                log::warn!(
                    "[sync-server] rejecting non-mutual {} (private profile)",
                    short_id(&remote_pubkey)
                );
                return Err(AcceptError::from_err(std::io::Error::other("private")));
            }
        }
    }

    let map_err = |e: anyhow::Error| AcceptError::from_err(std::io::Error::other(e.to_string()));

    let server_post_count = storage
        .count_posts_by_author(&req.author)
        .await
        .map_err(map_err)?;
    let server_interaction_count = storage
        .count_interactions_by_author(&req.author)
        .await
        .map_err(map_err)?;
    let posts_after_count = if req.newest_timestamp > 0 {
        storage
            .count_posts_after(&req.author, req.newest_timestamp)
            .await
            .map_err(map_err)?
    } else {
        server_post_count
    };
    let interactions_after_count = if req.newest_interaction_timestamp > 0 {
        storage
            .count_interactions_after(&req.author, req.newest_interaction_timestamp)
            .await
            .map_err(map_err)?
    } else {
        server_interaction_count
    };

    // Determine sync mode
    let posts_match = req.post_count == server_post_count;
    let interactions_match = req.interaction_count == server_interaction_count;
    let mode = if posts_match && interactions_match {
        SyncMode::UpToDate
    } else if server_post_count >= req.post_count
        && (server_post_count - req.post_count) == posts_after_count
    {
        SyncMode::TimestampCatchUp
    } else {
        SyncMode::NeedIdDiff
    };

    let profile = storage.get_profile(node_id).await.ok().flatten();

    log::info!(
        "[sync-server] author={}, client=({}/{}/ts={}/its={}), server=({}/{}/after={}/iafter={}), mode={:?}",
        short_id(&req.author),
        req.post_count,
        req.interaction_count,
        req.newest_timestamp,
        req.newest_interaction_timestamp,
        server_post_count,
        server_interaction_count,
        posts_after_count,
        interactions_after_count,
        mode,
    );

    let summary = SyncSummary {
        server_post_count,
        server_interaction_count,
        posts_after_count,
        interactions_after_count,
        mode,
        profile,
    };
    let summary_bytes = serde_json::to_vec(&summary).map_err(AcceptError::from_err)?;

    // Send Phase 1 summary
    send.write_all(&summary_bytes)
        .await
        .map_err(AcceptError::from_err)?;
    send.finish().map_err(AcceptError::from_err)?;

    if mode == SyncMode::UpToDate {
        conn.closed().await;
        return Ok(());
    }

    // Phase 2 or 3: Open a new bi-stream for streaming data.
    let (mut data_send, mut data_recv) = conn.accept_bi().await?;

    let known_ids: Vec<String> = if mode == SyncMode::NeedIdDiff {
        let ids_bytes = data_recv
            .read_to_end(5_000_000)
            .await
            .map_err(AcceptError::from_err)?;
        serde_json::from_slice(&ids_bytes).map_err(AcceptError::from_err)?
    } else {
        Vec::new()
    };

    // Stream posts
    let mut offset = 0;
    let mut total_sent = 0u64;
    loop {
        let batch = match mode {
            SyncMode::TimestampCatchUp => storage
                .get_posts_after(&req.author, req.newest_timestamp, BATCH_SIZE, offset)
                .await
                .map_err(map_err)?,
            SyncMode::NeedIdDiff => storage
                .get_posts_not_in(&req.author, &known_ids, BATCH_SIZE, offset)
                .await
                .map_err(map_err)?,
            SyncMode::UpToDate => break,
        };

        if batch.is_empty() {
            break;
        }

        total_sent += batch.len() as u64;
        offset += batch.len();

        let frame = SyncFrame::Posts(batch);
        let frame_bytes = serde_json::to_vec(&frame).map_err(AcceptError::from_err)?;
        write_frame(&mut data_send, &frame_bytes)
            .await
            .map_err(map_err)?;
    }

    // Stream interactions
    if !interactions_match {
        let interaction_catchup = server_interaction_count >= req.interaction_count
            && (server_interaction_count - req.interaction_count) == interactions_after_count;

        let mut ioffset = 0;
        loop {
            let batch = if interaction_catchup {
                storage
                    .get_interactions_after(
                        &req.author,
                        req.newest_interaction_timestamp,
                        BATCH_SIZE,
                        ioffset,
                    )
                    .await
                    .map_err(map_err)?
            } else {
                storage
                    .get_interactions_paged(&req.author, BATCH_SIZE, ioffset)
                    .await
                    .map_err(map_err)?
            };

            if batch.is_empty() {
                break;
            }

            ioffset += batch.len();

            let frame = SyncFrame::Interactions(batch);
            let frame_bytes = serde_json::to_vec(&frame).map_err(AcceptError::from_err)?;
            write_frame(&mut data_send, &frame_bytes)
                .await
                .map_err(map_err)?;
        }
    }

    // End-of-stream marker
    write_frame(&mut data_send, &[]).await.map_err(map_err)?;
    data_send.finish().map_err(AcceptError::from_err)?;

    log::info!(
        "[sync-server] streamed {} posts to {} (mode={:?})",
        total_sent,
        short_id(remote_str),
        mode,
    );

    conn.closed().await;
    Ok(())
}

/// Result returned from a sync operation.
pub struct SyncResult {
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub profile: Option<Profile>,
    pub remote_post_count: u64,
    pub mode: SyncMode,
}

/// Client: sync posts from a remote peer using the three-phase protocol.
pub async fn sync_from_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    target: EndpointId,
    author: &str,
) -> anyhow::Result<SyncResult> {
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
    let summary: SyncSummary = serde_json::from_slice(&summary_bytes)?;

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
                            if iroh_social_types::verify_linked_devices_announcement(&announcement)
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
    })
}

// ---------------------------------------------------------------------------
// Ingestion helpers (moved from commands/sync.rs -- used by sync, gossip, push)
// ---------------------------------------------------------------------------

/// Resolve the signing public key for a peer from the delegation cache.
/// Falls back to the master pubkey for backward compat with pre-delegation content.
pub async fn resolve_signer(storage: &Storage, master_pubkey: &str) -> Option<PublicKey> {
    // Try cached signing key first (from peer_delegations)
    if let Ok(Some(signing_pubkey)) = storage.get_peer_signing_pubkey(master_pubkey).await
        && let Ok(pk) = signing_pubkey.parse()
    {
        return Some(pk);
    }
    // Fall back to master pubkey (backward compat: pre-delegation content was signed by master key)
    master_pubkey.parse().ok()
}

/// Validate, verify signature, and store a single incoming post.
/// Returns `true` if the post was stored successfully.
pub async fn process_incoming_post(
    storage: &Storage,
    post: &Post,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> bool {
    if let Err(reason) = validate_post(post) {
        log::error!("[{label}] rejected post {}: {reason}", &post.id);
        return false;
    }
    let signer = match resolve_signer(storage, &post.author).await {
        Some(pk) => pk,
        None => {
            log::error!(
                "[{label}] rejected post {} (cannot resolve signer for {})",
                &post.id,
                short_id(&post.author),
            );
            return false;
        }
    };
    if let Err(reason) = verify_post_signature(post, &signer) {
        log::error!("[{label}] rejected post {} (bad sig): {reason}", &post.id);
        return false;
    }
    if let Err(e) = storage.insert_post(post).await {
        log::error!("[{label}] failed to store post: {e}");
        return false;
    }
    if post.author != my_id {
        if parse_mentions(&post.content).contains(&my_id.to_string()) {
            let _ = storage
                .insert_notification(
                    "mention",
                    &post.author,
                    None,
                    Some(&post.id),
                    post.timestamp,
                )
                .await;
            let _ = app_handle.emit("mentioned-in-post", post);
            let _ = app_handle.emit("notification-received", ());
        }
        if post.reply_to_author.as_deref() == Some(my_id) {
            let _ = storage
                .insert_notification(
                    "reply",
                    &post.author,
                    post.reply_to.as_deref(),
                    Some(&post.id),
                    post.timestamp,
                )
                .await;
            let _ = app_handle.emit("notification-received", ());
        }
        if post.quote_of_author.as_deref() == Some(my_id) {
            let _ = storage
                .insert_notification(
                    "quote",
                    &post.author,
                    post.quote_of.as_deref(),
                    Some(&post.id),
                    post.timestamp,
                )
                .await;
            let _ = app_handle.emit("notification-received", ());
        }
    }
    true
}

/// Validate, verify signature, and store a single incoming interaction.
pub async fn process_incoming_interaction(
    storage: &Storage,
    interaction: &Interaction,
    expected_author: &str,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) {
    if interaction.author != expected_author {
        return;
    }
    if let Err(reason) = validate_interaction(interaction) {
        log::error!(
            "[{label}] rejected interaction {}: {reason}",
            &interaction.id
        );
        return;
    }
    let signer = match resolve_signer(storage, &interaction.author).await {
        Some(pk) => pk,
        None => {
            log::error!(
                "[{label}] rejected interaction {} (cannot resolve signer for {})",
                &interaction.id,
                short_id(&interaction.author),
            );
            return;
        }
    };
    if let Err(reason) = verify_interaction_signature(interaction, &signer) {
        log::error!(
            "[{label}] rejected interaction {} (bad sig): {reason}",
            &interaction.id
        );
        return;
    }
    let _ = storage.save_interaction(interaction).await;
    if interaction.target_author == my_id && interaction.author != my_id {
        let _ = storage
            .insert_notification(
                "like",
                &interaction.author,
                Some(&interaction.target_post_id),
                None,
                interaction.timestamp,
            )
            .await;
        let _ = app_handle.emit("notification-received", ());
    }
}

/// Validate and store posts/interactions/profile from a sync result.
/// Returns the number of posts actually stored.
pub async fn process_sync_result(
    storage: &Storage,
    pubkey: &str,
    result: &SyncResult,
    label: &str,
    my_id: &str,
    app_handle: &AppHandle,
) -> usize {
    let mut stored = 0;
    for post in &result.posts {
        if process_incoming_post(storage, post, label, my_id, app_handle).await {
            stored += 1;
        }
    }
    if let Some(profile) = &result.profile
        && let Err(e) = storage.save_profile(pubkey, profile).await
    {
        log::error!("[{label}] failed to store profile: {e}");
    }
    for interaction in &result.interactions {
        process_incoming_interaction(storage, interaction, pubkey, label, my_id, app_handle).await;
    }
    stored
}

// ---------------------------------------------------------------------------
// Unified sync-one-peer (used by startup sync, drip sync, manual sync, follow sync)
// ---------------------------------------------------------------------------

/// Stats returned from `sync_one_peer`.
#[allow(dead_code)]
pub struct SyncOneResult {
    pub stored: usize,
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub remote_post_count: u64,
    pub mode: SyncMode,
    pub profile: Option<Profile>,
}

/// Sync from a single peer: resolve transport NodeIds, try each, run sync protocol,
/// process and store results. Returns `SyncOneResult` on first successful NodeId.
pub async fn sync_one_peer(
    endpoint: &Endpoint,
    storage: &Storage,
    pubkey: &str,
    my_id: &str,
    app_handle: &AppHandle,
    label: &str,
) -> anyhow::Result<SyncOneResult> {
    let node_ids = storage.get_peer_transport_node_ids(pubkey).await?;
    if node_ids.is_empty() {
        anyhow::bail!("no cached transport NodeId for {}", short_id(pubkey));
    }

    let mut last_err = String::new();
    for node_id in &node_ids {
        let target: EndpointId = match node_id.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("[{label}] bad transport NodeId {}: {e}", short_id(node_id));
                continue;
            }
        };

        let result = tokio::time::timeout(
            SYNC_TIMEOUT,
            sync_from_peer(endpoint, storage, target, pubkey),
        )
        .await;

        match result {
            Ok(Ok(sync_result)) => {
                let stored =
                    process_sync_result(storage, pubkey, &sync_result, label, my_id, app_handle)
                        .await;
                log::info!(
                    "[{label}] stored {stored}/{} posts from {} via {} (mode={:?})",
                    sync_result.posts.len(),
                    short_id(pubkey),
                    short_id(node_id),
                    sync_result.mode,
                );
                return Ok(SyncOneResult {
                    stored,
                    posts: sync_result.posts,
                    interactions: sync_result.interactions,
                    remote_post_count: sync_result.remote_post_count,
                    mode: sync_result.mode,
                    profile: sync_result.profile,
                });
            }
            Ok(Err(e)) => {
                log::warn!("[{label}] failed via {}: {e}", short_id(node_id));
                last_err = e.to_string();
            }
            Err(_) => {
                log::warn!("[{label}] timed out via {}", short_id(node_id));
                last_err = "timeout".to_string();
            }
        }
    }

    anyhow::bail!(
        "sync failed for {} (tried {} node(s)): {last_err}",
        short_id(pubkey),
        node_ids.len(),
    )
}
