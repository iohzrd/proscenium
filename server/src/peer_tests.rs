//! Tests of sync and gossip ingestion against an in-process peer. Both endpoints bind
//! UDP sockets on 127.0.0.1 with ports chosen by the OS and use no relay or DNS.

use crate::config::Config;
use crate::ingestion::IngestionManager;
use crate::node::{GOSSIP_MAX_MESSAGE_SIZE, Node};
use crate::storage::{RegistrationChange, Storage};
use crate::test_util::{TestUser, loopback_endpoint, node_id};
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Connection, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, SecretKey, TransportAddr};
use iroh_gossip::Gossip;
use iroh_gossip::api::GossipTopic;
use proscenium_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, MediaAttachment, PEER_ALPN, PeerRequest,
    Post, PostTombstone, Profile, SyncFrame, SyncMode, SyncSummary, Visibility, now_millis,
    post_ids_digest, sign_delete_post, sign_post, user_feed_topic,
};
use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const URL: &str = "https://server.test";
/// Upper bound for every wait in these tests.
const WAIT: Duration = Duration::from_secs(30);

/// What the peer answers sync requests with.
#[derive(Default)]
struct PeerContent {
    posts: Vec<Post>,
    interactions: Vec<Interaction>,
    tombstones: Vec<PostTombstone>,
    profile: Option<Profile>,
    announcement: Option<LinkedDevicesAnnouncement>,
    /// Mode of each answered sync request, in order.
    modes: Vec<SyncMode>,
}

/// Responder side of the sync protocol over `PeerContent`.
#[derive(Clone)]
struct FakePeer {
    content: Arc<Mutex<PeerContent>>,
}

impl std::fmt::Debug for FakePeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakePeer").finish()
    }
}

async fn write_frame(send: &mut SendStream, frame: &SyncFrame) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(frame)?;
    send.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

fn digest_of<'a>(posts: impl Iterator<Item = &'a Post>) -> String {
    let mut ids: Vec<String> = posts.map(|p| p.id.clone()).collect();
    post_ids_digest(&mut ids)
}

impl FakePeer {
    async fn respond(&self, conn: &Connection) -> anyhow::Result<()> {
        let (mut send, mut recv) = conn.accept_bi().await?;
        let request: PeerRequest = serde_json::from_slice(&recv.read_to_end(1 << 20).await?)?;
        let PeerRequest::Sync(req) = request else {
            anyhow::bail!("unexpected request");
        };

        let (posts, interactions, tombstones, profile, announcement) = {
            let content = self.content.lock().unwrap();
            (
                content.posts.clone(),
                content.interactions.clone(),
                content.tombstones.clone(),
                content.profile.clone(),
                content.announcement.clone(),
            )
        };
        let post_count = posts.len() as u64;
        let interaction_count = interactions.len() as u64;
        let tombstone_count = tombstones.len() as u64;
        let posts_after: Vec<&Post> = posts
            .iter()
            .filter(|p| p.timestamp > req.newest_timestamp)
            .collect();
        let interactions_after: Vec<&Interaction> = interactions
            .iter()
            .filter(|i| i.timestamp > req.newest_interaction_timestamp)
            .collect();

        let mode = if req.post_count == post_count
            && req.post_ids_digest == digest_of(posts.iter())
            && req.interaction_count == interaction_count
            && req.tombstone_count == tombstone_count
        {
            SyncMode::UpToDate
        } else if req.post_ids_digest
            == digest_of(posts.iter().filter(|p| p.timestamp <= req.newest_timestamp))
        {
            SyncMode::TimestampCatchUp
        } else {
            SyncMode::NeedIdDiff
        };

        let summary = SyncSummary {
            server_post_count: post_count,
            server_interaction_count: interaction_count,
            posts_after_count: posts_after.len() as u64,
            interactions_after_count: interactions_after.len() as u64,
            mode,
            profile,
            active_stage: None,
            announcement,
            server_tombstone_count: tombstone_count,
        };
        send.write_all(&serde_json::to_vec(&summary)?).await?;
        send.finish()?;
        self.content.lock().unwrap().modes.push(mode);

        if mode != SyncMode::UpToDate {
            let (mut send, mut recv) = conn.accept_bi().await?;
            let id_bytes = recv.read_to_end(8 << 20).await?;
            let to_send: Vec<Post> = if mode == SyncMode::NeedIdDiff {
                let known: HashSet<String> = serde_json::from_slice::<Vec<String>>(&id_bytes)?
                    .into_iter()
                    .collect();
                posts
                    .iter()
                    .filter(|p| !known.contains(&p.id))
                    .cloned()
                    .collect()
            } else {
                anyhow::ensure!(id_bytes.is_empty(), "id list sent for timestamp catch-up");
                posts_after.into_iter().cloned().collect()
            };

            if req.tombstone_count != tombstone_count {
                write_frame(&mut send, &SyncFrame::Tombstones(tombstones)).await?;
            }
            // Two posts per frame, so a sync of several posts spans several frames.
            for batch in to_send.chunks(2) {
                write_frame(&mut send, &SyncFrame::Posts(batch.to_vec())).await?;
            }
            if req.interaction_count != interaction_count {
                let catch_up = interaction_count.checked_sub(req.interaction_count)
                    == Some(interactions_after.len() as u64);
                let batch: Vec<Interaction> = if catch_up {
                    interactions_after.into_iter().cloned().collect()
                } else {
                    interactions
                };
                write_frame(&mut send, &SyncFrame::Interactions(batch)).await?;
            }
            send.write_all(&0u32.to_be_bytes()).await?;
            send.finish()?;
        }
        conn.closed().await;
        Ok(())
    }
}

impl ProtocolHandler for FakePeer {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        self.respond(&conn)
            .await
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))
    }
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

async fn wait_for<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A server node with in-memory storage and one peer whose NodeId is the transport
/// NodeId of `user`.
struct Harness {
    manager: Arc<IngestionManager>,
    storage: Arc<Storage>,
    node: Node,
    peer_router: Router,
    peer_gossip: Gossip,
    content: Arc<Mutex<PeerContent>>,
    user: TestUser,
}

impl Harness {
    async fn new() -> Self {
        let user = TestUser::new(1);
        let peer_endpoint = loopback_endpoint(SecretKey::from_bytes(&[101u8; 32])).await;
        assert_eq!(peer_endpoint.id().to_string(), user.transport_node_id);
        let server_endpoint = loopback_endpoint(SecretKey::from_bytes(&[200u8; 32])).await;

        // Each endpoint resolves the other's NodeId to its loopback socket address.
        let lookup = MemoryLookup::new();
        lookup.add_endpoint_info(loopback_addr(&peer_endpoint));
        server_endpoint.address_lookup().unwrap().add(lookup);
        let lookup = MemoryLookup::new();
        lookup.add_endpoint_info(loopback_addr(&server_endpoint));
        peer_endpoint.address_lookup().unwrap().add(lookup);

        let content = Arc::new(Mutex::new(PeerContent::default()));
        let peer_gossip = Gossip::builder()
            .max_message_size(GOSSIP_MAX_MESSAGE_SIZE)
            .spawn(peer_endpoint.clone());
        let peer_router = Router::builder(peer_endpoint)
            .accept(iroh_gossip::ALPN, peer_gossip.clone())
            .accept(
                PEER_ALPN,
                FakePeer {
                    content: content.clone(),
                },
            )
            .spawn();

        let config = Config::parse(&format!(
            "[server]\npublic_url = \"{URL}\"\n[sync]\nstartup_sync = false\n"
        ))
        .unwrap();
        let storage = Arc::new(Storage::open_in_memory().await.unwrap());
        let node = Node::spawn(server_endpoint);
        let manager = IngestionManager::new(&node, storage.clone(), &config);

        Self {
            manager,
            storage,
            node,
            peer_router,
            peer_gossip,
            content,
            user,
        }
    }

    fn pubkey(&self) -> &str {
        &self.user.master_pubkey
    }

    async fn register(&self) {
        let request = self
            .user
            .registration(URL, now_millis(), Visibility::Public, None);
        self.manager
            .apply_registration_change(RegistrationChange::Register(&request))
            .await
            .unwrap();
    }

    /// Ids of the posts stored for the user, sorted.
    async fn stored_post_ids(&self) -> Vec<String> {
        let mut ids = self
            .storage
            .get_sync_cursor(self.pubkey())
            .await
            .unwrap()
            .post_ids;
        ids.sort();
        ids
    }

    /// Run syncs until the stored post ids equal the ids of `expected`.
    async fn sync_until_posts(&self, what: &str, expected: &[&Post]) {
        let mut expected: Vec<String> = expected.iter().map(|p| p.id.clone()).collect();
        expected.sort();
        wait_for(what, || async {
            self.manager.sync_user(self.pubkey()).await;
            self.stored_post_ids().await == expected
        })
        .await;
    }

    async fn shutdown(self) {
        self.manager.shutdown().await;
        self.node.shutdown().await;
        let _ = tokio::time::timeout(WAIT, self.peer_router.shutdown()).await;
    }
}

fn sorted_ids(posts: &[&Post]) -> Vec<String> {
    let mut ids: Vec<String> = posts.iter().map(|p| p.id.clone()).collect();
    ids.sort();
    ids
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_follows_the_protocol_against_a_peer() {
    let harness = Harness::new().await;
    let user = &harness.user;
    let pk = harness.pubkey().to_string();
    let now = now_millis();
    let peer_node_id = user.transport_node_id.clone();

    let p1 = user.post("first", now - 5_000);
    let p2 = user.post("second", now - 4_000);
    let p3 = user.post("third", now - 3_000);
    {
        let mut content = harness.content.lock().unwrap();
        content.posts = vec![p1.clone(), p2.clone(), p3.clone()];
        content.interactions = vec![user.like(&p1, 1, now - 2_500)];
        content.profile = Some(user.profile("Alice", Visibility::Public, now - 10_000));
        content.announcement = Some(user.announcement(1, std::slice::from_ref(&peer_node_id)));
    }

    // Registration starts the gossip task and requests the first sync.
    harness.register().await;
    assert!(harness.manager.has_subscription(&pk).await);
    wait_for("first sync", || async {
        harness.stored_post_ids().await == sorted_ids(&[&p1, &p2, &p3])
    })
    .await;
    wait_for("interaction and profile of first sync", || async {
        let cursor = harness.storage.get_sync_cursor(&pk).await.unwrap();
        let user_info = harness.storage.get_user_info(&pk).await.unwrap().unwrap();
        cursor.interaction_count == 1 && user_info.display_name.as_deref() == Some("Alice")
    })
    .await;
    // The server stored nothing before, so the first request is a timestamp catch-up.
    assert_eq!(
        harness.content.lock().unwrap().modes.first(),
        Some(&SyncMode::TimestampCatchUp)
    );

    // Equal state: the peer answers UpToDate.
    wait_for("up-to-date sync", || async {
        harness.manager.sync_user(&pk).await;
        harness.content.lock().unwrap().modes.last() == Some(&SyncMode::UpToDate)
    })
    .await;

    // The peer deletes p2 and publishes p4. The digests differ, so the peer asks for the
    // id list, then sends the tombstone and the one post the server lacks.
    let p4 = user.post("fourth", now - 2_000);
    {
        let mut content = harness.content.lock().unwrap();
        content.posts.retain(|p| p.id != p2.id);
        content.posts.push(p4.clone());
        content.tombstones.push(PostTombstone {
            id: p2.id.clone(),
            author: pk.clone(),
            signature: sign_delete_post(&p2.id, &pk, &user.key),
        });
    }
    harness
        .sync_until_posts("tombstone and new post", &[&p1, &p3, &p4])
        .await;
    assert!(
        harness
            .content
            .lock()
            .unwrap()
            .modes
            .contains(&SyncMode::NeedIdDiff)
    );
    assert_eq!(
        harness
            .storage
            .get_sync_cursor(&pk)
            .await
            .unwrap()
            .tombstone_count,
        1
    );

    // Content that fails a check is not stored: a post of another author, a post whose
    // content was changed after signing, a post signed by another key, a tombstone with
    // a signature for another id, and an interaction of another author. The valid post
    // p5 is sent after them.
    let other = TestUser::new(2);
    let foreign = other.post("from another author", now - 1_900);
    let mut altered = user.post("original", now - 1_800);
    altered.content = "altered".to_string();
    let mut wrong_key = user.post("signed by another key", now - 1_700);
    sign_post(&mut wrong_key, &other.key);
    let p5 = user.post("fifth", now - 1_600);
    {
        let mut content = harness.content.lock().unwrap();
        content.posts.extend([
            foreign.clone(),
            altered.clone(),
            wrong_key.clone(),
            p5.clone(),
        ]);
        content.tombstones.push(PostTombstone {
            id: p1.id.clone(),
            author: pk.clone(),
            signature: sign_delete_post(&p3.id, &pk, &user.key),
        });
        content.interactions.push(other.like(&p1, 2, now - 1_500));
    }
    harness
        .sync_until_posts("valid post after invalid content", &[&p1, &p3, &p4, &p5])
        .await;
    let cursor = harness.storage.get_sync_cursor(&pk).await.unwrap();
    assert_eq!(cursor.interaction_count, 1);
    assert_eq!(cursor.tombstone_count, 1);
    assert_eq!(harness.storage.total_post_count().await.unwrap(), 4);

    // A device announcement with a higher version in the summary replaces the stored
    // device NodeIds.
    let devices = vec![peer_node_id.clone(), node_id(60)];
    harness.content.lock().unwrap().announcement = Some(user.announcement(2, &devices));
    wait_for("device announcement from the summary", || async {
        harness.manager.sync_user(&pk).await;
        harness.storage.get_device_node_ids(&pk).await.unwrap() == Some(devices.clone())
    })
    .await;

    // A device announcement signed by another key is not stored. A sync handles the
    // announcement in the summary before the posts, so once p6 is stored, at least one
    // sync has handled the forged announcement.
    let forged = other
        .forging_as(user)
        .announcement(3, std::slice::from_ref(&peer_node_id));
    let p6 = user.post("sixth", now - 1_000);
    {
        let mut content = harness.content.lock().unwrap();
        content.announcement = Some(forged);
        content.posts.push(p6.clone());
    }
    harness
        .sync_until_posts(
            "post after a forged announcement",
            &[&p1, &p3, &p4, &p5, &p6],
        )
        .await;
    assert_eq!(
        harness.storage.get_device_node_ids(&pk).await.unwrap(),
        Some(devices)
    );

    // A listed profile in the summary ends the public registration: stored content is
    // deleted and the gossip task is stopped.
    harness.content.lock().unwrap().profile = Some(user.profile("Alice", Visibility::Listed, now));
    wait_for("listed profile applied", || async {
        harness.manager.sync_user(&pk).await;
        harness
            .storage
            .get_subscription_target(&pk)
            .await
            .unwrap()
            .is_none()
    })
    .await;
    assert!(harness.stored_post_ids().await.is_empty());
    assert!(!harness.manager.has_subscription(&pk).await);
    let user_info = harness.storage.get_user_info(&pk).await.unwrap().unwrap();
    assert_eq!(user_info.visibility, "listed");

    harness.shutdown().await;
}

async fn broadcast(topic: &mut GossipTopic, message: &GossipMessage) {
    let bytes = serde_json::to_vec(message).unwrap();
    topic.broadcast(bytes.into()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gossip_messages_are_verified_and_applied() {
    let harness = Harness::new().await;
    let user = &harness.user;
    let pk = harness.pubkey().to_string();
    let now = now_millis();

    // The peer subscribes to its own feed topic without bootstrap peers, as a client
    // does; the server dials it after registration.
    let mut topic = harness
        .peer_gossip
        .subscribe(user_feed_topic(&pk), vec![])
        .await
        .unwrap();
    harness.register().await;
    tokio::time::timeout(WAIT, topic.joined())
        .await
        .expect("server did not join the feed topic")
        .unwrap();

    let p1 = user.post("over gossip", now - 5_000);
    broadcast(&mut topic, &GossipMessage::NewPost(p1.clone())).await;
    wait_for("post from gossip", || async {
        harness.stored_post_ids().await == sorted_ids(&[&p1])
    })
    .await;

    // Rejected: a post of another author, a post signed by another key, and a deletion
    // of p1 with a signature for another id. The valid post p2 is broadcast after them.
    let other = TestUser::new(2);
    let foreign = other.post("from another author", now - 4_000);
    let mut wrong_key = user.post("signed by another key", now - 3_900);
    sign_post(&mut wrong_key, &other.key);
    let p2 = user.post("second over gossip", now - 3_800);
    broadcast(&mut topic, &GossipMessage::NewPost(foreign)).await;
    broadcast(&mut topic, &GossipMessage::NewPost(wrong_key)).await;
    broadcast(
        &mut topic,
        &GossipMessage::DeletePost {
            id: p1.id.clone(),
            author: pk.clone(),
            signature: sign_delete_post(&p2.id, &pk, &user.key),
        },
    )
    .await;
    broadcast(&mut topic, &GossipMessage::NewPost(p2.clone())).await;
    wait_for("valid post after rejected messages", || async {
        harness.stored_post_ids().await == sorted_ids(&[&p1, &p2])
    })
    .await;
    assert_eq!(harness.storage.total_post_count().await.unwrap(), 2);

    // A post near the maximum size is larger than the default gossip frame limit.
    let mut large = Post {
        id: String::new(),
        author: pk.clone(),
        content: "x".repeat(10_000),
        timestamp: now - 3_000,
        media: (0..10)
            .map(|i| MediaAttachment {
                hash: format!("{i:064x}"),
                ticket: "t".repeat(2_048),
                mime_type: "image/png".to_string(),
                filename: format!("file-{i}.png"),
                size: 1,
            })
            .collect(),
        reply_to: None,
        reply_to_author: None,
        quote_of: None,
        quote_of_author: None,
        signature: String::new(),
    };
    sign_post(&mut large, &user.key);
    assert!(
        serde_json::to_vec(&GossipMessage::NewPost(large.clone()))
            .unwrap()
            .len()
            > 30_000
    );
    broadcast(&mut topic, &GossipMessage::NewPost(large.clone())).await;
    wait_for("large post from gossip", || async {
        harness.stored_post_ids().await == sorted_ids(&[&p1, &p2, &large])
    })
    .await;

    // A signed deletion removes the post and records a tombstone.
    broadcast(
        &mut topic,
        &GossipMessage::DeletePost {
            id: p1.id.clone(),
            author: pk.clone(),
            signature: sign_delete_post(&p1.id, &pk, &user.key),
        },
    )
    .await;
    wait_for("deletion from gossip", || async {
        harness.stored_post_ids().await == sorted_ids(&[&p2, &large])
    })
    .await;
    assert_eq!(
        harness
            .storage
            .get_sync_cursor(&pk)
            .await
            .unwrap()
            .tombstone_count,
        1
    );

    // A listed profile received on the topic ends the public registration. The gossip
    // task that processes the message is the task that is stopped.
    let listed = user.profile("Alice", Visibility::Listed, now);
    broadcast(&mut topic, &GossipMessage::ProfileUpdate(listed)).await;
    wait_for("listed profile from gossip", || async {
        !harness.manager.has_subscription(&pk).await
    })
    .await;
    assert!(harness.stored_post_ids().await.is_empty());
    let user_info = harness.storage.get_user_info(&pk).await.unwrap().unwrap();
    assert_eq!(user_info.visibility, "listed");
    assert_eq!(user_info.display_name.as_deref(), Some("Alice"));

    harness.shutdown().await;
}
