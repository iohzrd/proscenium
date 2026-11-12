//! Sync of an author's content from a mirror, over loopback QUIC: `handle_sync` on the
//! mirror and `sync_from_peer` on the requester.

use super::client::{SyncSource, sync_from_peer};
use super::handle_sync;
use super::orchestration::SyncOneResult;
use crate::error::AppError;
use crate::stage::StageActorHandle;
use crate::state::{Identity, SharedIdentity, test_support::loopback_endpoint};
use crate::storage::Storage;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr};
use proscenium_types::{
    FollowEntry, Interaction, InteractionKind, PEER_ALPN, PeerRequest, Post, PostTombstone,
    Profile, SyncFrame, Visibility, now_millis, sign_delete_post, sign_interaction, sign_post,
    sign_profile,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

const LIMIT: Duration = Duration::from_secs(30);

fn identity(master: u8, transport: u8) -> SharedIdentity {
    Arc::new(tokio::sync::RwLock::new(Identity::new(
        SecretKey::from_bytes(&[master; 32]),
        SecretKey::from_bytes(&[transport; 32]),
    )))
}

fn loopback_addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr {
        id: endpoint.id(),
        addrs: endpoint
            .bound_sockets()
            .into_iter()
            .map(TransportAddr::Ip)
            .collect(),
    }
}

struct Author {
    key: SecretKey,
    pubkey: String,
}

impl Author {
    fn new(seed: u8) -> Self {
        let key = SecretKey::from_bytes(&[seed; 32]);
        let pubkey = key.public().to_string();
        Self { key, pubkey }
    }

    fn post(&self, content: &str, timestamp: u64) -> Post {
        let mut post = Post {
            id: String::new(),
            author: self.pubkey.clone(),
            content: content.to_string(),
            timestamp,
            media: vec![],
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: String::new(),
        };
        sign_post(&mut post, &self.key);
        post
    }

    fn deletion(&self, post: &Post) -> PostTombstone {
        PostTombstone {
            id: post.id.clone(),
            author: self.pubkey.clone(),
            signature: sign_delete_post(&post.id, &self.pubkey, &self.key),
        }
    }

    fn like(&self, target: &Post, n: u32) -> Interaction {
        let mut like = Interaction {
            id: format!("{n:032x}"),
            author: self.pubkey.clone(),
            kind: InteractionKind::Like,
            target_post_id: target.id.clone(),
            target_author: target.author.clone(),
            timestamp: 1_000 + u64::from(n),
            signature: String::new(),
        };
        sign_interaction(&mut like, &self.key);
        like
    }

    fn profile(&self, visibility: Visibility, updated_at: u64) -> Profile {
        let mut profile = Profile {
            display_name: "author".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility,
            updated_at,
            signature: String::new(),
        };
        sign_profile(&mut profile, &self.pubkey, &self.key);
        profile
    }
}

/// Serve one request on `endpoint` with `handle_sync`, as `PeerHandler::accept` does
/// after it read the request.
async fn serve_one(
    endpoint: Endpoint,
    storage: Arc<Storage>,
    identity: SharedIdentity,
) -> Result<(), String> {
    let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
    let conn = incoming.await.map_err(|e| e.to_string())?;
    let remote = conn.remote_id().to_string();
    let (send, mut recv) = conn.accept_bi().await.map_err(|e| e.to_string())?;
    let bytes = recv
        .read_to_end(64 * 1024)
        .await
        .map_err(|e| e.to_string())?;
    let PeerRequest::Sync(req) = serde_json::from_slice(&bytes).map_err(|e| e.to_string())? else {
        return Err("unexpected request".to_string());
    };
    // The mirror path does not query the stage actor.
    let (stage_tx, _stage_rx) = tokio::sync::mpsc::channel(1);
    let stage = StageActorHandle::new(stage_tx);
    handle_sync(&storage, &identity, &remote, &conn, send, req, &stage)
        .await
        .map_err(|e| e.to_string())
}

/// A mirror storage: the local user `mirror_pubkey` is Public and follows `author`, whose
/// stored profile has `visibility` and was confirmed by a device of the author now.
async fn mirror_storage(mirror_pubkey: &str, author: &Author, visibility: Visibility) -> Storage {
    let storage = Storage::open_in_memory().await.unwrap();
    storage
        .save_profile(
            mirror_pubkey,
            &Profile {
                display_name: "mirror".to_string(),
                bio: String::new(),
                avatar_hash: None,
                avatar_ticket: None,
                visibility: Visibility::Public,
                updated_at: 1,
                signature: String::new(),
            },
        )
        .await
        .unwrap();
    storage
        .merge_follows_lww(
            mirror_pubkey,
            &[FollowEntry {
                pubkey: author.pubkey.clone(),
                followed_at: 1,
                state: "active".to_string(),
                last_changed_at: 1,
            }],
        )
        .await
        .unwrap();
    storage
        .save_profile(&author.pubkey, &author.profile(visibility, 5))
        .await
        .unwrap();
    storage
        .confirm_public_profile(&author.pubkey, now_millis())
        .await
        .unwrap();
    storage
}

/// Sync the content of `author` on `requester` from `mirror`, with `SyncSource::Mirror`.
async fn sync_from_mirror(
    mirror: (&Arc<Storage>, &SharedIdentity, u8),
    requester: (&Storage, &SharedIdentity, u8),
    author: &str,
) -> (Result<SyncOneResult, AppError>, Result<(), String>) {
    let ep_mirror = loopback_endpoint(SecretKey::from_bytes(&[mirror.2; 32]), PEER_ALPN).await;
    let ep_requester =
        loopback_endpoint(SecretKey::from_bytes(&[requester.2; 32]), PEER_ALPN).await;
    let lookup = MemoryLookup::new();
    lookup.add_endpoint_info(loopback_addr(&ep_mirror));
    ep_requester.address_lookup().unwrap().add(lookup);
    let server = tokio::spawn(serve_one(
        ep_mirror.clone(),
        mirror.0.clone(),
        mirror.1.clone(),
    ));

    let app = tauri::test::mock_app();
    let result = tokio::time::timeout(
        LIMIT,
        sync_from_peer(
            &ep_requester,
            requester.0,
            requester.1,
            ep_mirror.id(),
            author,
            app.handle(),
            "test",
            Instant::now() + LIMIT,
            SyncSource::Mirror,
        ),
    )
    .await
    .expect("requester timed out");
    let served = tokio::time::timeout(LIMIT, server)
        .await
        .expect("mirror timed out")
        .expect("mirror panicked");
    ep_requester.close().await;
    ep_mirror.close().await;
    (result, served)
}

#[tokio::test]
async fn mirror_serves_a_followed_public_author() {
    let author = Author::new(1);
    let mirror_identity = identity(2, 22);
    let requester_identity = identity(3, 23);
    let mirror_pubkey = mirror_identity.read().await.master_pubkey.clone();

    let kept = author.post("kept", 1_000);
    let deleted = author.post("deleted", 2_000);
    let other = Author::new(4).post("other", 500);

    // The mirror follows the author and holds its Public profile, one post, the deletion
    // of the other post, and one like.
    let mirror = Arc::new(mirror_storage(&mirror_pubkey, &author, Visibility::Public).await);
    mirror.insert_post(&kept).await.unwrap();
    mirror
        .apply_post_tombstones(&author.pubkey, &[author.deletion(&deleted)], 1)
        .await
        .unwrap();
    mirror
        .save_interaction(&author.like(&other, 1))
        .await
        .unwrap();

    // The requester holds the deleted post and a like the mirror lacks.
    let requester = Storage::open_in_memory().await.unwrap();
    requester.insert_post(&deleted).await.unwrap();
    let own_like = author.like(&other, 2);
    requester.save_interaction(&own_like).await.unwrap();

    let (result, served) = sync_from_mirror(
        (&mirror, &mirror_identity, 22),
        (&requester, &requester_identity, 23),
        &author.pubkey,
    )
    .await;
    served.unwrap();
    let result = result.unwrap();

    // Post and deletion arrived; the mirror's count is not reported as the author's.
    assert_eq!(result.stored, 1);
    assert_eq!(result.remote_post_count, None);
    assert_eq!(
        requester
            .get_post_ids_by_author(&author.pubkey)
            .await
            .unwrap(),
        vec![kept.id.clone()]
    );
    assert_eq!(
        requester
            .count_post_tombstones(&author.pubkey)
            .await
            .unwrap(),
        1
    );
    // The profile arrived.
    let profile = requester
        .get_profile(&author.pubkey)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(profile.visibility, Visibility::Public);
    // No interaction was sent, and the like the mirror lacks was kept.
    let likes: Vec<String> = requester
        .get_interactions_paged(&author.pubkey, 10, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(likes, vec![own_like.id]);
    // The mirror's NodeId is not mapped to the author.
    let mirror_node = SecretKey::from_bytes(&[22u8; 32]).public().to_string();
    assert_eq!(
        requester
            .try_get_master_pubkey_for_transport(&mirror_node)
            .await
            .unwrap(),
        None
    );
    // The requester sent no device list: the mirror maps no NodeId to it.
    let requester_node = SecretKey::from_bytes(&[23u8; 32]).public().to_string();
    assert_eq!(
        mirror
            .try_get_master_pubkey_for_transport(&requester_node)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn interactions_from_a_mirror_are_discarded() {
    let author = Author::new(1);
    let storage = Storage::open_in_memory().await.unwrap();
    let app = tauri::test::mock_app();
    let like = author.like(&Author::new(4).post("other", 500), 1);
    let frame = || SyncFrame::Interactions(vec![like.clone()]);

    let mut from_mirror = super::processing::FrameIngest::new(
        &storage,
        &author.pubkey,
        "test",
        "me",
        app.handle(),
        false,
        false,
    );
    from_mirror.process(frame()).await.unwrap();
    assert_eq!(
        storage
            .count_interactions_by_author(&author.pubkey)
            .await
            .unwrap(),
        0
    );

    let mut from_author = super::processing::FrameIngest::new(
        &storage,
        &author.pubkey,
        "test",
        "me",
        app.handle(),
        false,
        true,
    );
    from_author.process(frame()).await.unwrap();
    assert_eq!(
        storage
            .count_interactions_by_author(&author.pubkey)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn mirror_refuses_a_listed_author() {
    let author = Author::new(1);
    let mirror_identity = identity(2, 22);
    let requester_identity = identity(3, 23);
    let mirror_pubkey = mirror_identity.read().await.master_pubkey.clone();

    let mirror = Arc::new(mirror_storage(&mirror_pubkey, &author, Visibility::Listed).await);
    mirror
        .insert_post(&author.post("for followers", 1_000))
        .await
        .unwrap();

    let requester = Storage::open_in_memory().await.unwrap();
    let (result, served) = sync_from_mirror(
        (&mirror, &mirror_identity, 22),
        (&requester, &requester_identity, 23),
        &author.pubkey,
    )
    .await;
    assert!(served.is_err());
    assert!(result.is_err());
    assert!(
        requester
            .get_post_ids_by_author(&author.pubkey)
            .await
            .unwrap()
            .is_empty()
    );
}
