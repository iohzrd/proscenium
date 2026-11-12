use super::*;
use proscenium_types::*;

async fn test_storage() -> Storage {
    Storage::open_in_memory().await.expect("open in-memory db")
}

fn make_post(id: &str, author: &str, ts: u64) -> Post {
    Post {
        id: id.to_string(),
        author: author.to_string(),
        content: format!("content of {id}"),
        timestamp: ts,
        media: vec![],
        reply_to: None,
        reply_to_author: None,
        quote_of: None,
        quote_of_author: None,
        signature: "deadbeef".repeat(16),
    }
}

fn make_interaction(
    id: &str,
    author: &str,
    target_post: &str,
    target_author: &str,
    ts: u64,
) -> Interaction {
    Interaction {
        id: id.to_string(),
        author: author.to_string(),
        kind: InteractionKind::Like,
        target_post_id: target_post.to_string(),
        target_author: target_author.to_string(),
        timestamp: ts,
        signature: "deadbeef".repeat(16),
    }
}

fn make_profile(name: &str) -> Profile {
    Profile {
        display_name: name.to_string(),
        bio: "test bio".to_string(),
        avatar_hash: None,
        avatar_ticket: None,
        visibility: Visibility::Public,
        updated_at: 0,
        signature: "sig".to_string(),
    }
}

/// A `VerifiedIdentity` with the given device NodeIds and version, sent by the first
/// NodeId. Storage does not check signatures, so the signature fields are placeholders.
fn make_identity(master: &str, nodes: &[&str], version: u64) -> VerifiedIdentity {
    VerifiedIdentity {
        master_pubkey: master.to_string(),
        announcement: LinkedDevicesAnnouncement {
            master_pubkey: master.to_string(),
            devices: nodes
                .iter()
                .map(|n| DeviceEntry {
                    node_id: n.to_string(),
                    device_name: "device".to_string(),
                    added_at: 1,
                })
                .collect(),
            removed: vec![],
            version,
            signature: "sig".to_string(),
        },
        profile: None,
        node_id: nodes.first().map(|n| n.to_string()).unwrap_or_default(),
    }
}

async fn follow(s: &Storage, me: &str, pubkey: &str) {
    let entry = SocialGraphEntry {
        pubkey: pubkey.to_string(),
        followed_at: 1,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow(me, &entry).await.unwrap();
}

fn ids(posts: &[Post]) -> Vec<&str> {
    posts.iter().map(|p| p.id.as_str()).collect()
}

fn make_stored_message(
    id: &str,
    conv_id: &str,
    from: &str,
    to: &str,
    content: &str,
    ts: u64,
) -> StoredMessage {
    StoredMessage {
        id: id.to_string(),
        conversation_id: conv_id.to_string(),
        from_pubkey: from.to_string(),
        to_pubkey: to.to_string(),
        content: content.to_string(),
        timestamp: ts,
        media: vec![],
        read: false,
        delivered: false,
        reply_to: None,
    }
}

// ── Profiles ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn profile_save_and_get() {
    let s = test_storage().await;
    let profile = make_profile("Alice");
    s.save_profile("pk_alice", &profile).await.unwrap();

    let loaded = s.get_profile("pk_alice").await.unwrap().unwrap();
    assert_eq!(loaded.display_name, "Alice");
    assert_eq!(loaded.bio, "test bio");
    assert_eq!(loaded.visibility, Visibility::Public);
}

#[tokio::test]
async fn profile_upsert_updates() {
    let s = test_storage().await;
    s.save_profile("pk_a", &make_profile("V1")).await.unwrap();
    s.save_profile("pk_a", &make_profile("V2")).await.unwrap();

    let loaded = s.get_profile("pk_a").await.unwrap().unwrap();
    assert_eq!(loaded.display_name, "V2");
}

#[tokio::test]
async fn profile_get_nonexistent_returns_none() {
    let s = test_storage().await;
    assert!(s.get_profile("no_such_key").await.unwrap().is_none());
}

#[tokio::test]
async fn profile_visibility_round_trip() {
    let s = test_storage().await;
    let mut profile = make_profile("Private User");
    profile.visibility = Visibility::Private;
    s.save_profile("pk_priv", &profile).await.unwrap();

    let loaded = s.get_profile("pk_priv").await.unwrap().unwrap();
    assert_eq!(loaded.visibility, Visibility::Private);
}

// ── Posts ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn post_insert_and_get() {
    let s = test_storage().await;
    let post = make_post("p1", "author_a", 1000);
    s.insert_post(&post).await.unwrap();

    let loaded = s.get_post_by_id("p1").await.unwrap().unwrap();
    assert_eq!(loaded.id, "p1");
    assert_eq!(loaded.author, "author_a");
    assert_eq!(loaded.timestamp, 1000);
}

#[tokio::test]
async fn post_insert_duplicate_is_ignored() {
    let s = test_storage().await;
    let post = make_post("p1", "author_a", 1000);
    s.insert_post(&post).await.unwrap();
    s.insert_post(&post).await.unwrap(); // should not error
    assert_eq!(s.count_posts_by_author("author_a").await.unwrap(), 1);
}

#[tokio::test]
async fn post_delete() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    assert!(s.delete_post("a", "p1", "sig").await.unwrap());
    assert!(s.get_post_by_id("p1").await.unwrap().is_none());
    // The signed deletion is stored in the same transaction.
    assert!(s.is_post_tombstoned("a", "p1").await.unwrap());
    assert!(!s.delete_post("a", "p1", "sig").await.unwrap()); // already deleted
}

#[tokio::test]
async fn received_tombstone_removes_the_rows_that_refer_to_the_post() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.save_interaction(&make_interaction("i1", "liker", "p1", "a", 2000))
        .await
        .unwrap();
    s.toggle_bookmark("p1").await.unwrap();
    let tombstone = PostTombstone {
        id: "p1".to_string(),
        author: "a".to_string(),
        signature: "sig".to_string(),
    };
    let outcome = s
        .apply_post_tombstones("a", &[tombstone], 3000)
        .await
        .unwrap();
    assert_eq!(outcome.posts_removed, 1);
    assert_eq!(s.get_post_counts("me", "p1").await.unwrap().likes, 0);
    assert!(!s.is_bookmarked("p1").await.unwrap());
}

#[tokio::test]
async fn post_delete_by_author() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    s.insert_post(&make_post("p3", "b", 3000)).await.unwrap();

    let deleted = s.delete_posts_by_author("a").await.unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(s.count_posts_by_author("a").await.unwrap(), 0);
    assert_eq!(s.count_posts_by_author("b").await.unwrap(), 1);
}

#[tokio::test]
async fn post_count_and_newest_timestamp() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    s.insert_post(&make_post("p3", "a", 3000)).await.unwrap();

    assert_eq!(s.count_posts_by_author("a").await.unwrap(), 3);
    assert_eq!(s.newest_post_timestamp("a").await.unwrap(), 3000);
    assert_eq!(s.newest_post_timestamp("nobody").await.unwrap(), 0);
}

#[tokio::test]
async fn post_count_after() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    s.insert_post(&make_post("p3", "a", 3000)).await.unwrap();

    assert_eq!(s.count_posts_after("a", 1500).await.unwrap(), 2);
    assert_eq!(s.count_posts_after("a", 3000).await.unwrap(), 0);
    assert_eq!(s.count_posts_after("a", 0).await.unwrap(), 3);
}

#[tokio::test]
async fn post_get_ids_by_author() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();

    let ids = s.get_post_ids_by_author("a").await.unwrap();
    assert_eq!(ids, vec!["p1", "p2"]);
}

#[tokio::test]
async fn post_feed_returns_desc_order() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    s.insert_post(&make_post("p3", "b", 1500)).await.unwrap();
    follow(&s, "me", "a").await;
    follow(&s, "me", "b").await;

    let feed = s.get_feed("me", 10, None).await.unwrap();
    assert_eq!(feed.len(), 3);
    assert_eq!(feed[0].id, "p2"); // newest first
    assert_eq!(feed[2].id, "p1"); // oldest last
}

#[tokio::test]
async fn post_feed_cursor_pagination() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    s.insert_post(&make_post("p3", "a", 3000)).await.unwrap();
    follow(&s, "me", "a").await;

    let page1 = s.get_feed("me", 2, None).await.unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].id, "p3");

    let last = page1.last().unwrap();
    let page2 = s
        .get_feed("me", 2, Some((last.timestamp, &last.id)))
        .await
        .unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].id, "p1");
}

#[tokio::test]
async fn post_feed_excludes_muted_and_blocked() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "good", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "muted_user", 2000))
        .await
        .unwrap();
    s.insert_post(&make_post("p3", "blocked_user", 3000))
        .await
        .unwrap();

    for author in ["good", "muted_user", "blocked_user"] {
        follow(&s, "me", author).await;
    }
    s.mute_user("muted_user").await.unwrap();
    s.block_user("blocked_user").await.unwrap();

    let feed = s.get_feed("me", 10, None).await.unwrap();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].author, "good");
}

#[tokio::test]
async fn post_replies() {
    let s = test_storage().await;
    let parent = make_post("parent", "a", 1000);
    s.insert_post(&parent).await.unwrap();

    let mut reply1 = make_post("r1", "b", 2000);
    reply1.reply_to = Some("parent".to_string());
    reply1.reply_to_author = Some("a".to_string());
    s.insert_post(&reply1).await.unwrap();

    let mut reply2 = make_post("r2", "c", 3000);
    reply2.reply_to = Some("parent".to_string());
    reply2.reply_to_author = Some("a".to_string());
    s.insert_post(&reply2).await.unwrap();

    let replies = s.get_replies("parent", 10, None).await.unwrap();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].id, "r1"); // ASC order
}

#[tokio::test]
async fn post_delete_repost_by_target() {
    let s = test_storage().await;
    let mut repost = make_post("rp1", "a", 2000);
    repost.content = String::new();
    repost.quote_of = Some("target_post".to_string());
    repost.quote_of_author = Some("b".to_string());
    s.insert_post(&repost).await.unwrap();

    let deleted = s
        .delete_repost_by_target("a", "target_post", |_| "sig".to_string())
        .await
        .unwrap();
    assert_eq!(deleted, vec!["rp1"]);
    assert!(s.get_post_by_id("rp1").await.unwrap().is_none());
}

#[tokio::test]
async fn post_media_filter() {
    let s = test_storage().await;

    let text_post = make_post("t1", "a", 1000);
    s.insert_post(&text_post).await.unwrap();

    let mut img_post = make_post("i1", "a", 2000);
    img_post.media = vec![MediaAttachment {
        hash: "h1".into(),
        ticket: "t1".into(),
        mime_type: "image/png".into(),
        filename: "photo.png".into(),
        size: 1024,
    }];
    s.insert_post(&img_post).await.unwrap();

    let text_only = s
        .get_posts_by_author("a", 10, None, Some("text"))
        .await
        .unwrap();
    assert_eq!(text_only.len(), 1);
    assert_eq!(text_only[0].id, "t1");

    let images = s
        .get_posts_by_author("a", 10, None, Some("images"))
        .await
        .unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].id, "i1");
}

#[tokio::test]
async fn post_feed_holds_only_followed_authors_and_self() {
    let s = test_storage().await;
    s.insert_post(&make_post("own", "me", 1000)).await.unwrap();
    s.insert_post(&make_post("followed", "friend", 2000))
        .await
        .unwrap();
    s.insert_post(&make_post("pushed", "stranger", 3000))
        .await
        .unwrap();
    s.insert_post(&make_post("old", "unfollowed", 4000))
        .await
        .unwrap();
    follow(&s, "me", "friend").await;
    follow(&s, "me", "unfollowed").await;
    s.unfollow("me", "unfollowed").await.unwrap();
    // A follower row (stranger follows me) does not put the follower's posts in the feed.
    s.upsert_follower("me", "stranger", 1).await.unwrap();

    let feed = s.get_feed("me", 10, None).await.unwrap();
    assert_eq!(ids(&feed), vec!["followed", "own"]);
}

#[tokio::test]
async fn post_feed_cursor_returns_every_post_with_equal_timestamps() {
    let s = test_storage().await;
    follow(&s, "me", "a").await;
    for i in 0..5 {
        s.insert_post(&make_post(&format!("p{i}"), "a", 1000))
            .await
            .unwrap();
    }
    s.insert_post(&make_post("older", "a", 500)).await.unwrap();

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<(u64, String)> = None;
    loop {
        let before = cursor.as_ref().map(|(ts, id)| (*ts, id.as_str()));
        let page = s.get_feed("me", 2, before).await.unwrap();
        let Some(last) = page.last() else { break };
        cursor = Some((last.timestamp, last.id.clone()));
        seen.extend(page.iter().map(|p| p.id.clone()));
    }
    assert_eq!(seen, vec!["p4", "p3", "p2", "p1", "p0", "older"]);
}

#[tokio::test]
async fn post_feed_cursor_without_id_starts_below_the_timestamp() {
    let s = test_storage().await;
    follow(&s, "me", "a").await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p0", "a", 999)).await.unwrap();

    let page = s.get_feed("me", 10, Some((1000, ""))).await.unwrap();
    assert_eq!(ids(&page), vec!["p0"]);
}

#[tokio::test]
async fn post_by_author_cursor_returns_every_post_with_equal_timestamps() {
    let s = test_storage().await;
    for i in 0..5 {
        s.insert_post(&make_post(&format!("p{i}"), "a", 1000))
            .await
            .unwrap();
    }
    s.insert_post(&make_post("other", "b", 1000)).await.unwrap();

    let page1 = s.get_posts_by_author("a", 3, None, None).await.unwrap();
    assert_eq!(ids(&page1), vec!["p4", "p3", "p2"]);
    let last = page1.last().unwrap();
    let page2 = s
        .get_posts_by_author("a", 3, Some((last.timestamp, &last.id)), None)
        .await
        .unwrap();
    assert_eq!(ids(&page2), vec!["p1", "p0"]);
}

#[tokio::test]
async fn post_replies_cursor_returns_later_replies_in_ascending_order() {
    let s = test_storage().await;
    // Six replies: four share one timestamp.
    for (id, ts) in [
        ("r0", 1000),
        ("r1", 2000),
        ("r2", 2000),
        ("r3", 2000),
        ("r4", 2000),
        ("r5", 3000),
    ] {
        let mut reply = make_post(id, "b", ts);
        reply.reply_to = Some("parent".to_string());
        s.insert_post(&reply).await.unwrap();
    }

    let page1 = s.get_replies("parent", 3, None).await.unwrap();
    assert_eq!(ids(&page1), vec!["r0", "r1", "r2"]);
    let last = page1.last().unwrap();
    let page2 = s
        .get_replies("parent", 3, Some((last.timestamp, &last.id)))
        .await
        .unwrap();
    assert_eq!(ids(&page2), vec!["r3", "r4", "r5"]);
    let last = page2.last().unwrap();
    let page3 = s
        .get_replies("parent", 3, Some((last.timestamp, &last.id)))
        .await
        .unwrap();
    assert!(page3.is_empty());
}

#[tokio::test]
async fn post_delete_removes_rows_that_refer_to_the_post() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    for target in ["p1", "p2"] {
        s.save_interaction(&make_interaction(
            &format!("like_{target}"),
            "liker",
            target,
            "a",
            3000,
        ))
        .await
        .unwrap();
        s.toggle_bookmark(target).await.unwrap();
        s.insert_notification("like", "liker", Some(target), None, 3000)
            .await
            .unwrap();
        s.insert_notification("reply", "replier", Some("x"), Some(target), 3000)
            .await
            .unwrap();
    }

    // Another author cannot delete the post or the rows that refer to it.
    assert!(!s.delete_post("b", "p1", "sig").await.unwrap());
    assert!(s.get_post_by_id("p1").await.unwrap().is_some());
    assert_eq!(s.get_post_counts("me", "p1").await.unwrap().likes, 1);
    assert!(s.is_bookmarked("p1").await.unwrap());

    assert!(s.delete_post("a", "p1", "sig").await.unwrap());
    assert!(s.get_post_by_id("p1").await.unwrap().is_none());
    assert_eq!(s.get_post_counts("me", "p1").await.unwrap().likes, 0);
    assert!(!s.is_bookmarked("p1").await.unwrap());

    // Rows that refer to the other post are kept.
    assert_eq!(s.get_post_counts("me", "p2").await.unwrap().likes, 1);
    assert!(s.is_bookmarked("p2").await.unwrap());
    let notifications = s.get_notifications(10, None).await.unwrap();
    assert_eq!(notifications.len(), 2);
    assert!(
        notifications.iter().all(
            |n| n.target_post_id.as_deref() == Some("p2") || n.post_id.as_deref() == Some("p2")
        )
    );
}

#[tokio::test]
async fn post_repost_is_distinct_from_quote() {
    let s = test_storage().await;
    s.insert_post(&make_post("target", "b", 1000))
        .await
        .unwrap();

    // A quote: `quote_of` with text.
    let mut quote = make_post("quote", "me", 2000);
    quote.quote_of = Some("target".to_string());
    s.insert_post(&quote).await.unwrap();

    assert!(!s.has_repost("me", "target").await.unwrap());
    let counts = s.get_post_counts("me", "target").await.unwrap();
    assert_eq!(counts.reposts, 1);
    assert!(!counts.reposted_by_me);
    assert!(
        s.delete_repost_by_target("me", "target", |_| "sig".to_string())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(s.get_post_by_id("quote").await.unwrap().is_some());

    // A quote with media and no text is not a repost either.
    let mut media_quote = make_post("media_quote", "me", 2500);
    media_quote.content = String::new();
    media_quote.quote_of = Some("target".to_string());
    media_quote.media = vec![MediaAttachment {
        hash: "h".into(),
        ticket: "t".into(),
        mime_type: "image/png".into(),
        filename: "a.png".into(),
        size: 1,
    }];
    s.insert_post(&media_quote).await.unwrap();
    assert!(!s.has_repost("me", "target").await.unwrap());

    // Two reposts of the same target (for example from two devices).
    for (id, ts) in [("repost1", 3000), ("repost2", 4000)] {
        let mut repost = make_post(id, "me", ts);
        repost.content = String::new();
        repost.quote_of = Some("target".to_string());
        s.insert_post(&repost).await.unwrap();
    }
    assert!(s.has_repost("me", "target").await.unwrap());
    assert!(!s.has_repost("someone_else", "target").await.unwrap());
    assert!(
        s.get_post_counts("me", "target")
            .await
            .unwrap()
            .reposted_by_me
    );

    let mut deleted = s
        .delete_repost_by_target("me", "target", |_| "sig".to_string())
        .await
        .unwrap();
    deleted.sort();
    assert_eq!(deleted, vec!["repost1", "repost2"]);
    assert!(!s.has_repost("me", "target").await.unwrap());
    assert!(s.get_post_by_id("quote").await.unwrap().is_some());
    assert!(s.get_post_by_id("media_quote").await.unwrap().is_some());
}

#[tokio::test]
async fn post_get_by_id_is_deterministic_when_authors_share_an_id() {
    let s = test_storage().await;
    s.insert_post(&make_post("shared", "b", 1000))
        .await
        .unwrap();
    s.insert_post(&make_post("shared", "a", 2000))
        .await
        .unwrap();

    let post = s.get_post_by_id("shared").await.unwrap().unwrap();
    assert_eq!(post.author, "a");

    // A delete by one author keeps the other author's row.
    assert!(s.delete_post("a", "shared", "sig").await.unwrap());
    let post = s.get_post_by_id("shared").await.unwrap().unwrap();
    assert_eq!(post.author, "b");
}

#[tokio::test]
async fn post_page_size_is_capped() {
    let s = test_storage().await;
    for i in 0..MAX_PAGE_SIZE + 5 {
        s.insert_post(&make_post(&format!("p{i:04}"), "a", i as u64))
            .await
            .unwrap();
    }
    // `usize::MAX as i64` is -1, which SQLite reads as "no limit".
    let page = s
        .get_posts_by_author("a", usize::MAX, None, None)
        .await
        .unwrap();
    assert_eq!(page.len(), MAX_PAGE_SIZE);
}

#[tokio::test]
async fn post_ids_by_author_order_equal_timestamps_by_id() {
    let s = test_storage().await;
    for id in ["c", "a", "d", "b"] {
        s.insert_post(&make_post(id, "author", 1000)).await.unwrap();
    }
    assert_eq!(
        s.get_post_ids_by_author("author").await.unwrap(),
        vec!["a", "b", "c", "d"]
    );
}

#[tokio::test]
async fn post_delete_by_author_keeps_interactions_and_other_authors() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "b", 1000)).await.unwrap();
    s.save_interaction(&make_interaction("i1", "me", "p1", "a", 2000))
        .await
        .unwrap();
    s.insert_notification("mention", "a", None, Some("p1"), 2000)
        .await
        .unwrap();
    s.insert_notification("mention", "b", None, Some("p2"), 2000)
        .await
        .unwrap();

    assert_eq!(s.delete_posts_by_author("a").await.unwrap(), 1);
    assert!(s.get_post_by_id("p2").await.unwrap().is_some());
    assert_eq!(s.count_interactions_by_author("me").await.unwrap(), 1);
    let notifications = s.get_notifications(10, None).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].post_id.as_deref(), Some("p2"));
}

// ── Social (follows/followers) ────────────────────────────────────────────────

#[tokio::test]
async fn social_follow_and_unfollow() {
    let s = test_storage().await;
    let entry = SocialGraphEntry {
        pubkey: "peer_a".to_string(),
        followed_at: 1000,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow("me", &entry).await.unwrap();

    assert!(s.is_following("me", "peer_a").await.unwrap());
    let follows = s.get_follows("me").await.unwrap();
    assert_eq!(follows.len(), 1);
    assert_eq!(follows[0].pubkey, "peer_a");

    s.unfollow("me", "peer_a").await.unwrap();
    assert!(!s.is_following("me", "peer_a").await.unwrap());
    assert!(s.get_follows("me").await.unwrap().is_empty());
}

#[tokio::test]
async fn social_refollow_after_unfollow() {
    let s = test_storage().await;
    let entry = SocialGraphEntry {
        pubkey: "peer_a".to_string(),
        followed_at: 1000,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow("me", &entry).await.unwrap();
    s.unfollow("me", "peer_a").await.unwrap();
    assert!(!s.is_following("me", "peer_a").await.unwrap());

    // Re-follow
    s.follow("me", &entry).await.unwrap();
    assert!(s.is_following("me", "peer_a").await.unwrap());
}

#[tokio::test]
async fn social_followers() {
    let s = test_storage().await;
    let is_new = s.upsert_follower("me", "f1", 1000).await.unwrap();
    assert!(is_new);

    let is_new2 = s.upsert_follower("me", "f1", 2000).await.unwrap();
    assert!(!is_new2); // already exists

    assert!(s.is_follower("me", "f1").await.unwrap());
    assert!(!s.is_follower("me", "nobody").await.unwrap());

    let followers = s.get_followers("me").await.unwrap();
    assert_eq!(followers.len(), 1);
    assert!(followers[0].is_online);
    assert_eq!(followers[0].last_seen, 2000);

    s.set_follower_offline("me", "f1").await.unwrap();
    let followers = s.get_followers("me").await.unwrap();
    assert!(!followers[0].is_online);
}

#[tokio::test]
async fn social_mutual() {
    let s = test_storage().await;
    let entry = SocialGraphEntry {
        pubkey: "peer".to_string(),
        followed_at: 1000,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow("me", &entry).await.unwrap();
    assert!(!s.is_mutual("me", "peer").await.unwrap()); // not an approved follower yet

    // A follower row alone (for example from the gossip topic) does not make a mutual.
    s.upsert_follower("me", "peer", 2000).await.unwrap();
    assert!(!s.is_mutual("me", "peer").await.unwrap());

    s.record_follow_request("peer", 1, 2000, 9000)
        .await
        .unwrap();
    assert!(s.approve_follow_request("me", "peer", 2000).await.unwrap());
    assert!(s.is_mutual("me", "peer").await.unwrap());

    // Unfollowing ends it even though the follow request stays approved.
    assert!(s.unfollow("me", "peer").await.unwrap());
    assert!(!s.is_mutual("me", "peer").await.unwrap());
}

#[tokio::test]
async fn social_visibility_defaults_to_public() {
    let s = test_storage().await;
    let vis = s.get_visibility("nonexistent").await.unwrap();
    assert_eq!(vis, Visibility::Public);
}

#[tokio::test]
async fn social_visibility_from_profile() {
    let s = test_storage().await;
    let mut profile = make_profile("User");
    profile.visibility = Visibility::Listed;
    s.save_profile("pk_listed", &profile).await.unwrap();

    let vis = s.get_visibility("pk_listed").await.unwrap();
    assert_eq!(vis, Visibility::Listed);
}

#[tokio::test]
async fn social_visibility_invalid_value_is_error() {
    let s = test_storage().await;
    s.save_profile("pk", &make_profile("User")).await.unwrap();
    sqlx::query("UPDATE profiles SET visibility='friends' WHERE pubkey='pk'")
        .execute(&s.pool)
        .await
        .unwrap();
    assert!(s.get_visibility("pk").await.is_err());
}

#[tokio::test]
async fn social_unfollow_reports_whether_a_follow_existed() {
    let s = test_storage().await;
    assert!(!s.unfollow("me", "stranger").await.unwrap());

    let entry = SocialGraphEntry {
        pubkey: "peer_a".to_string(),
        followed_at: 1000,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow("me", &entry).await.unwrap();
    assert!(s.unfollow("me", "peer_a").await.unwrap());
    assert!(!s.unfollow("me", "peer_a").await.unwrap());
}

/// Canonical pubkey string derived from a one-byte seed.
fn test_pubkey(seed: u8) -> String {
    iroh::SecretKey::from_bytes(&[seed; 32])
        .public()
        .to_string()
}

fn remote_entry(pubkey: &str, ts: u64) -> SocialGraphEntry {
    SocialGraphEntry {
        pubkey: pubkey.to_string(),
        followed_at: ts,
        first_seen: ts,
        last_seen: ts,
        is_online: true,
    }
}

#[tokio::test]
async fn social_remote_lists_do_not_change_local_relationships() {
    let s = test_storage().await;
    let me = test_pubkey(1);
    let remote = test_pubkey(2);
    let other = test_pubkey(3);

    // The lists of the remote user contain the local user as followee and as follower,
    // and a third user as followee.
    s.cache_remote_list(
        &remote,
        RemoteListKind::Follows,
        &[remote_entry(&me, 10), remote_entry(&other, 20)],
        false,
    )
    .await
    .unwrap();
    s.cache_remote_list(
        &remote,
        RemoteListKind::Followers,
        &[remote_entry(&me, 30)],
        false,
    )
    .await
    .unwrap();

    assert!(!s.is_follower(&me, &remote).await.unwrap());
    assert!(!s.is_following(&me, &remote).await.unwrap());
    assert!(!s.is_mutual(&me, &remote).await.unwrap());
    assert!(s.get_follows(&me).await.unwrap().is_empty());
    assert!(s.get_followers(&me).await.unwrap().is_empty());
    assert!(s.get_follows(&remote).await.unwrap().is_empty());

    let follows = s
        .get_cached_remote_list(&remote, RemoteListKind::Follows)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(follows.entries.len(), 2);
    assert_eq!(follows.entries[0].pubkey, other); // followed_at DESC
    assert!(!follows.hidden);
    assert!(follows.fetched_at.is_some());
    let followers = s
        .get_cached_remote_list(&remote, RemoteListKind::Followers)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(followers.entries.len(), 1);
}

#[tokio::test]
async fn social_remote_list_refetch_keeps_local_follow_and_follower() {
    let s = test_storage().await;
    let me = test_pubkey(1);
    let remote = test_pubkey(2);

    s.follow(&me, &remote_entry(&remote, 100)).await.unwrap();
    s.upsert_follower(&me, &remote, 200).await.unwrap();

    // Two fetches of both lists, the second one without the local user.
    for entries in [vec![remote_entry(&me, 10)], vec![]] {
        s.cache_remote_list(&remote, RemoteListKind::Follows, &entries, false)
            .await
            .unwrap();
        s.cache_remote_list(&remote, RemoteListKind::Followers, &entries, false)
            .await
            .unwrap();
    }

    assert!(s.is_following(&me, &remote).await.unwrap());
    assert!(s.is_follower(&me, &remote).await.unwrap());
    assert_eq!(s.get_followers(&me).await.unwrap().len(), 1);
    let cached = s
        .get_cached_remote_list(&remote, RemoteListKind::Follows)
        .await
        .unwrap()
        .unwrap();
    assert!(cached.entries.is_empty());
}

#[tokio::test]
async fn social_remote_list_skips_invalid_pubkeys_and_is_absent_before_fetch() {
    let s = test_storage().await;
    let remote = test_pubkey(2);
    assert!(
        s.get_cached_remote_list(&remote, RemoteListKind::Follows)
            .await
            .unwrap()
            .is_none()
    );

    s.cache_remote_list(
        &remote,
        RemoteListKind::Follows,
        &[
            remote_entry("not a key", 1),
            remote_entry(&test_pubkey(3), 2),
        ],
        true,
    )
    .await
    .unwrap();
    let cached = s
        .get_cached_remote_list(&remote, RemoteListKind::Follows)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cached.entries.len(), 1);
    assert!(cached.hidden);
    // The follower list of the same user was not fetched.
    assert!(
        s.get_cached_remote_list(&remote, RemoteListKind::Followers)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn social_push_recipients_follow_visibility() {
    let s = test_storage().await;
    for requester in ["approved_followed", "approved_only", "pending", "blocked"] {
        s.record_follow_request(requester, 1, 1000, 9000)
            .await
            .unwrap();
    }
    for requester in ["approved_followed", "approved_only", "blocked"] {
        assert!(
            s.approve_follow_request("me", requester, 1000)
                .await
                .unwrap()
        );
    }
    s.block_user("blocked").await.unwrap();
    let entry = SocialGraphEntry {
        pubkey: "approved_followed".to_string(),
        followed_at: 1000,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow("me", &entry).await.unwrap();
    // A follower recorded from the gossip topic without an approved request.
    s.upsert_follower("me", "gossip_follower", 1000)
        .await
        .unwrap();

    assert!(
        s.get_push_recipients("me", Visibility::Public)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.get_push_recipients("me", Visibility::Listed)
            .await
            .unwrap(),
        vec!["approved_followed", "approved_only"]
    );
    assert_eq!(
        s.get_push_recipients("me", Visibility::Private)
            .await
            .unwrap(),
        vec!["approved_followed"]
    );
}

#[tokio::test]
async fn social_migration_removes_rows_written_from_remote_lists() {
    let s = test_storage().await;
    // Local follow, verified follower, and four rows as the earlier remote list cache
    // wrote them into social_graph.
    s.follow("me", &remote_entry("followee", 100))
        .await
        .unwrap();
    s.upsert_follower("me", "follower", 200).await.unwrap();
    for (follower, followee, followed_at, first_seen) in [
        ("remote", "third", Some(5), None),
        ("remote", "me", Some(6), None),
        ("third", "remote", None, Some(7)),
        ("me", "remote", None, Some(8)),
    ] {
        sqlx::query(
            "INSERT INTO social_graph (follower, followee, followed_at, state, last_changed_at, first_seen, last_seen, is_online)
             VALUES (?1, ?2, ?3, 'active', 0, ?4, 0, 0)",
        )
        .bind(follower)
        .bind(followee)
        .bind(followed_at)
        .bind(first_seen)
        .execute(&s.pool)
        .await
        .unwrap();
    }
    // A remote follower list contained the local user as a follower of "followee".
    sqlx::query("UPDATE social_graph SET first_seen=9 WHERE follower='me' AND followee='followee'")
        .execute(&s.pool)
        .await
        .unwrap();

    // 014 reads `peer_delegations`, which 002 creates and 017 drops.
    sqlx::raw_sql(include_str!("../../migrations/002_peer_delegations.sql"))
        .execute(&s.pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/014_social.sql"))
        .execute(&s.pool)
        .await
        .unwrap();

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM social_graph")
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(rows, 2);
    assert!(s.is_following("me", "followee").await.unwrap());
    assert!(!s.is_following("me", "remote").await.unwrap());
    assert!(s.is_follower("me", "follower").await.unwrap());
    assert!(!s.is_follower("me", "remote").await.unwrap());
    // The local follow row is not a follower row of "followee".
    assert!(!s.is_follower("followee", "me").await.unwrap());
}

// ── Interactions ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn interaction_save_and_delete() {
    let s = test_storage().await;
    let i = make_interaction("i1", "liker", "p1", "author", 1000);
    s.save_interaction(&i).await.unwrap();

    assert_eq!(s.count_interactions_by_author("liker").await.unwrap(), 1);
    assert!(s.delete_interaction("i1", "liker").await.unwrap());
    assert_eq!(s.count_interactions_by_author("liker").await.unwrap(), 0);
}

#[tokio::test]
async fn interaction_duplicate_ignored() {
    let s = test_storage().await;
    let i = make_interaction("i1", "liker", "p1", "author", 1000);
    s.save_interaction(&i).await.unwrap();
    s.save_interaction(&i).await.unwrap(); // no error
    assert_eq!(s.count_interactions_by_author("liker").await.unwrap(), 1);
}

#[tokio::test]
async fn interaction_delete_by_target() {
    let s = test_storage().await;
    let i = make_interaction("i1", "liker", "p1", "author", 1000);
    s.save_interaction(&i).await.unwrap();

    let deleted_id = s
        .delete_interaction_by_target("liker", "Like", "p1")
        .await
        .unwrap();
    assert_eq!(deleted_id, Some("i1".to_string()));
    assert_eq!(s.count_interactions_by_author("liker").await.unwrap(), 0);
}

#[tokio::test]
async fn interaction_post_counts() {
    let s = test_storage().await;

    // Create a target post
    s.insert_post(&make_post("p1", "author_a", 1000))
        .await
        .unwrap();

    // Likes from different users
    s.save_interaction(&make_interaction("i1", "liker1", "p1", "author_a", 2000))
        .await
        .unwrap();
    s.save_interaction(&make_interaction("i2", "liker2", "p1", "author_a", 3000))
        .await
        .unwrap();
    s.save_interaction(&make_interaction("i3", "me", "p1", "author_a", 4000))
        .await
        .unwrap();

    // A reply
    let mut reply = make_post("r1", "replier", 5000);
    reply.reply_to = Some("p1".to_string());
    s.insert_post(&reply).await.unwrap();

    // A repost (quote)
    let mut repost = make_post("q1", "quoter", 6000);
    repost.quote_of = Some("p1".to_string());
    s.insert_post(&repost).await.unwrap();

    let counts = s.get_post_counts("me", "p1").await.unwrap();
    assert_eq!(counts.likes, 3);
    assert_eq!(counts.replies, 1);
    assert_eq!(counts.reposts, 1);
    assert!(counts.liked_by_me);
    assert!(!counts.reposted_by_me);
}

#[tokio::test]
async fn interaction_newest_timestamp() {
    let s = test_storage().await;
    s.save_interaction(&make_interaction("i1", "a", "p1", "b", 1000))
        .await
        .unwrap();
    s.save_interaction(&make_interaction("i2", "a", "p2", "b", 5000))
        .await
        .unwrap();

    assert_eq!(s.newest_interaction_timestamp("a").await.unwrap(), 5000);
    assert_eq!(s.newest_interaction_timestamp("nobody").await.unwrap(), 0);
}

#[tokio::test]
async fn interaction_count_and_get_after() {
    let s = test_storage().await;
    s.save_interaction(&make_interaction("i1", "a", "p1", "b", 1000))
        .await
        .unwrap();
    s.save_interaction(&make_interaction("i2", "a", "p2", "b", 2000))
        .await
        .unwrap();
    s.save_interaction(&make_interaction("i3", "a", "p3", "b", 3000))
        .await
        .unwrap();

    assert_eq!(s.count_interactions_after("a", 1500).await.unwrap(), 2);

    let after = s.get_interactions_after("a", 1500, 10, 0).await.unwrap();
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].id, "i2"); // ASC
}

#[tokio::test]
async fn interaction_get_by_target() {
    let s = test_storage().await;
    assert!(
        s.get_interaction_by_target("liker", "Like", "p1")
            .await
            .unwrap()
            .is_none()
    );
    s.save_interaction(&make_interaction("i1", "liker", "p1", "author", 1000))
        .await
        .unwrap();
    // A second id for the same author and target is not stored.
    assert!(
        !s.save_interaction(&make_interaction("i2", "liker", "p1", "author", 2000))
            .await
            .unwrap()
    );

    let stored = s
        .get_interaction_by_target("liker", "Like", "p1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.id, "i1");
    assert_eq!(stored.timestamp, 1000);
    assert!(
        s.get_interaction_by_target("other", "Like", "p1")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn interaction_delete_removes_like_notification() {
    let s = test_storage().await;
    for (id, liker) in [("i1", "liker1"), ("i2", "liker2"), ("i3", "liker3")] {
        s.save_interaction(&make_interaction(id, liker, "p1", "me", 1000))
            .await
            .unwrap();
        s.insert_notification("like", liker, Some("p1"), None, 1000)
            .await
            .unwrap();
    }
    s.insert_notification("reply", "liker1", Some("p1"), Some("r1"), 1000)
        .await
        .unwrap();

    assert!(s.delete_interaction("i1", "liker1").await.unwrap());
    assert_eq!(
        s.delete_interaction_by_target("liker2", "Like", "p1")
            .await
            .unwrap(),
        Some("i2".to_string())
    );
    // An id that belongs to another author deletes nothing.
    assert!(!s.delete_interaction("i3", "liker1").await.unwrap());

    let mut remaining: Vec<(String, String)> = s
        .get_notifications(10, None)
        .await
        .unwrap()
        .into_iter()
        .map(|n| (n.kind, n.actor))
        .collect();
    remaining.sort();
    assert_eq!(
        remaining,
        vec![
            ("like".to_string(), "liker3".to_string()),
            ("reply".to_string(), "liker1".to_string()),
        ]
    );
}

#[tokio::test]
async fn interaction_paged_orders_equal_timestamps_by_id() {
    let s = test_storage().await;
    for (id, target) in [("c", "p1"), ("a", "p2"), ("d", "p3"), ("b", "p4")] {
        s.save_interaction(&make_interaction(id, "liker", target, "author", 1000))
            .await
            .unwrap();
    }
    let page_ids = |page: Vec<Interaction>| page.into_iter().map(|i| i.id).collect::<Vec<_>>();
    let first = s.get_interactions_paged("liker", 3, 0).await.unwrap();
    let second = s.get_interactions_paged("liker", 3, 3).await.unwrap();
    assert_eq!(page_ids(first), vec!["a", "b", "c"]);
    assert_eq!(page_ids(second), vec!["d"]);
    let after = s.get_interactions_after("liker", 999, 10, 2).await.unwrap();
    assert_eq!(page_ids(after), vec!["c", "d"]);
}

// ── Moderation (mute/block/bookmark) ─────────────────────────────────────────

#[tokio::test]
async fn moderation_mute_unmute() {
    let s = test_storage().await;
    assert!(!s.is_muted("bad_user").await.unwrap());

    s.mute_user("bad_user").await.unwrap();
    assert!(s.is_muted("bad_user").await.unwrap());
    assert_eq!(s.get_muted_pubkeys().await.unwrap(), vec!["bad_user"]);

    s.unmute_user("bad_user").await.unwrap();
    assert!(!s.is_muted("bad_user").await.unwrap());
    assert!(s.get_muted_pubkeys().await.unwrap().is_empty());
}

#[tokio::test]
async fn moderation_block_unblock() {
    let s = test_storage().await;
    assert!(!s.is_blocked("bad_user").await.unwrap());

    s.block_user("bad_user").await.unwrap();
    assert!(s.is_blocked("bad_user").await.unwrap());
    assert_eq!(s.get_blocked_pubkeys().await.unwrap(), vec!["bad_user"]);

    s.unblock_user("bad_user").await.unwrap();
    assert!(!s.is_blocked("bad_user").await.unwrap());
}

#[tokio::test]
async fn moderation_is_hidden() {
    let s = test_storage().await;
    assert!(!s.is_hidden("user").await.unwrap());

    s.mute_user("user").await.unwrap();
    assert!(s.is_hidden("user").await.unwrap());

    s.unmute_user("user").await.unwrap();
    assert!(!s.is_hidden("user").await.unwrap());

    s.block_user("user").await.unwrap();
    assert!(s.is_hidden("user").await.unwrap());
}

#[tokio::test]
async fn moderation_remute_after_unmute() {
    let s = test_storage().await;
    s.mute_user("user").await.unwrap();
    s.unmute_user("user").await.unwrap();
    s.mute_user("user").await.unwrap();
    assert!(s.is_muted("user").await.unwrap());
}

#[tokio::test]
async fn moderation_bookmark_toggle() {
    let s = test_storage().await;
    assert!(!s.is_bookmarked("p1").await.unwrap());

    let is_now = s.toggle_bookmark("p1").await.unwrap();
    assert!(is_now);
    assert!(s.is_bookmarked("p1").await.unwrap());

    let is_now = s.toggle_bookmark("p1").await.unwrap();
    assert!(!is_now);
    assert!(!s.is_bookmarked("p1").await.unwrap());
}

// ── Notifications ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn notification_insert_and_get() {
    let s = test_storage().await;
    s.insert_notification("like", "actor_a", Some("p1"), None, 1000)
        .await
        .unwrap();
    s.insert_notification("reply", "actor_b", Some("p1"), Some("r1"), 2000)
        .await
        .unwrap();

    let notifications = s.get_notifications(10, None).await.unwrap();
    assert_eq!(notifications.len(), 2);
    assert_eq!(notifications[0].kind, "reply"); // DESC order
    assert!(!notifications[0].read);
}

#[tokio::test]
async fn notification_deduplication() {
    let s = test_storage().await;
    // Same inputs produce same hash-based ID
    s.insert_notification("like", "actor", Some("p1"), None, 1000)
        .await
        .unwrap();
    s.insert_notification("like", "actor", Some("p1"), None, 1000)
        .await
        .unwrap();
    assert_eq!(s.get_unread_notification_count().await.unwrap(), 1);
}

#[tokio::test]
async fn notification_mark_read() {
    let s = test_storage().await;
    s.insert_notification("like", "a", Some("p1"), None, 1000)
        .await
        .unwrap();
    s.insert_notification("reply", "b", Some("p1"), Some("r1"), 2000)
        .await
        .unwrap();

    assert_eq!(s.get_unread_notification_count().await.unwrap(), 2);
    s.mark_notifications_read().await.unwrap();
    assert_eq!(s.get_unread_notification_count().await.unwrap(), 0);

    let notifications = s.get_notifications(10, None).await.unwrap();
    assert!(notifications.iter().all(|n| n.read));
}

#[tokio::test]
async fn notification_excludes_muted_and_blocked() {
    let s = test_storage().await;
    s.insert_notification("like", "good_actor", Some("p1"), None, 1000)
        .await
        .unwrap();
    s.insert_notification("like", "muted_actor", Some("p2"), None, 2000)
        .await
        .unwrap();
    s.insert_notification("like", "blocked_actor", Some("p3"), None, 3000)
        .await
        .unwrap();

    s.mute_user("muted_actor").await.unwrap();
    s.block_user("blocked_actor").await.unwrap();

    let notifications = s.get_notifications(10, None).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].actor, "good_actor");
}

#[tokio::test]
async fn notification_cursor_pagination() {
    let s = test_storage().await;
    for i in 1..=5 {
        s.insert_notification(
            "like",
            &format!("actor_{i}"),
            Some(&format!("p{i}")),
            None,
            i * 1000,
        )
        .await
        .unwrap();
    }

    let page1 = s.get_notifications(2, None).await.unwrap();
    assert_eq!(page1.len(), 2);

    let last = page1.last().unwrap();
    let page2 = s
        .get_notifications(2, Some((last.timestamp, &last.id)))
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page2[0].timestamp < page1.last().unwrap().timestamp);
}

#[tokio::test]
async fn notification_cursor_returns_every_row_with_equal_timestamps() {
    let s = test_storage().await;
    for i in 0..5 {
        s.insert_notification("like", &format!("actor_{i}"), Some("p1"), None, 1000)
            .await
            .unwrap();
    }

    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<(u64, String)> = None;
    loop {
        let before = cursor.as_ref().map(|(ts, id)| (*ts, id.as_str()));
        let page = s.get_notifications(2, before).await.unwrap();
        let Some(last) = page.last() else { break };
        cursor = Some((last.timestamp, last.id.clone()));
        for n in &page {
            assert!(seen.insert(n.actor.clone()), "row returned twice");
        }
    }
    assert_eq!(seen.len(), 5);
}

#[tokio::test]
async fn notification_table_keeps_newest_rows_up_to_the_cap() {
    let s = test_storage().await;
    let cap = super::notifications::MAX_NOTIFICATIONS as u64;
    for i in 0..cap + 3 {
        s.insert_notification("like", &format!("actor_{i}"), Some("p1"), None, 1000 + i)
            .await
            .unwrap();
    }
    assert_eq!(s.get_unread_notification_count().await.unwrap() as u64, cap);

    let newest = s.get_notifications(1, None).await.unwrap();
    assert_eq!(newest[0].actor, format!("actor_{}", cap + 2));
    // The three oldest rows were deleted.
    let oldest: i64 = sqlx::query_scalar("SELECT MIN(timestamp) FROM notifications")
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(oldest, 1003);
}

// ── Messaging (DMs) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn messaging_conversation_id_is_symmetric() {
    let id1 = Storage::conversation_id("alice", "bob");
    let id2 = Storage::conversation_id("bob", "alice");
    assert_eq!(id1, id2);
}

#[tokio::test]
async fn messaging_conversation_id_differs_per_peer_pair() {
    let ab = Storage::conversation_id("a", "b");
    assert_ne!(ab, Storage::conversation_id("a", "c"));
    // The two keys are hashed with a separator, so ("a", "bc") and ("ab", "c") differ.
    assert_ne!(
        Storage::conversation_id("a", "bc"),
        Storage::conversation_id("ab", "c")
    );
    assert_eq!(ab.len(), 64);
    assert!(ab.bytes().all(|b| b.is_ascii_hexdigit()));
}

/// Store an outgoing message from "me" with an outbox entry that is due at `next_attempt_at`.
async fn send_message(
    s: &Storage,
    id: &str,
    to: &str,
    content: &str,
    ts: u64,
    next_attempt_at: u64,
) {
    let conv_id = Storage::conversation_id("me", to);
    let msg = make_stored_message(id, &conv_id, "me", to, content, ts);
    s.insert_outgoing_dm(
        &msg,
        content,
        &format!(r#"{{"payload":"{id}"}}"#),
        next_attempt_at,
    )
    .await
    .unwrap();
}

/// Store a message received from `from`. Returns whether the message was inserted.
async fn receive_message(s: &Storage, id: &str, from: &str, content: &str, ts: u64) -> bool {
    let conv_id = Storage::conversation_id("me", from);
    let msg = make_stored_message(id, &conv_id, from, "me", content, ts);
    s.receive_dm_message(from, &msg, content).await.unwrap()
}

#[tokio::test]
async fn messaging_send_and_get_conversations() {
    let s = test_storage().await;
    send_message(&s, "m1", "bob", "hello!", 1000, 0).await;
    send_message(&s, "m2", "carol", "hey!", 2000, 0).await;

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos.len(), 2);
    assert_eq!(convos[0].peer_pubkey, "carol"); // newest first
    assert_eq!(convos[0].last_message_preview, "hey!");
}

#[tokio::test]
async fn messaging_send_updates_preview() {
    let s = test_storage().await;
    send_message(&s, "m1", "bob", "first message", 1000, 0).await;
    send_message(&s, "m2", "bob", "second message", 2000, 0).await;

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos.len(), 1);
    assert_eq!(convos[0].last_message_preview, "second message");
    assert_eq!(convos[0].last_message_at, 2000);
    assert_eq!(convos[0].unread_count, 0);
}

#[tokio::test]
async fn messaging_insert_and_get_messages() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    send_message(&s, "m1", "bob", "hello", 1000, 0).await;

    let messages = s.get_dm_messages(&conv_id, 50, None, None).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "hello");
    assert!(!messages[0].read);
    assert!(!messages[0].delivered);
}

#[tokio::test]
async fn messaging_mark_delivered_and_read() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    send_message(&s, "m1", "bob", "hello", 1000, 0).await;

    assert!(s.mark_dm_delivered(&conv_id, "me", "m1").await.unwrap());
    let messages = s.get_dm_messages(&conv_id, 50, None, None).await.unwrap();
    assert!(messages[0].delivered);
    assert!(!messages[0].read);

    assert!(s.mark_dm_read_by_id(&conv_id, "me", "m1").await.unwrap());
    let messages = s.get_dm_messages(&conv_id, 50, None, None).await.unwrap();
    assert!(messages[0].read);
}

#[tokio::test]
async fn messaging_delete_message() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    send_message(&s, "m1", "bob", "oops", 1000, 0).await;

    assert!(s.delete_dm_message("m1").await.unwrap());
    assert!(!s.delete_dm_message("m1").await.unwrap()); // already deleted

    let messages = s.get_dm_messages(&conv_id, 50, None, None).await.unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn messaging_unread_count() {
    let s = test_storage().await;
    send_message(&s, "m0", "bob", "msg1", 1000, 0).await;
    assert_eq!(s.get_total_unread_count().await.unwrap(), 0);

    // Simulate receiving: use receive_dm_message_atomically which increments unread
    assert!(receive_message(&s, "m1", "bob", "hello", 2000).await);
    assert_eq!(s.get_total_unread_count().await.unwrap(), 1);

    s.mark_conversation_read("bob", "me").await.unwrap();
    assert_eq!(s.get_total_unread_count().await.unwrap(), 0);
}

#[tokio::test]
async fn messaging_outbox() {
    let s = test_storage().await;
    send_message(&s, "msg1", "peer_a", "one", 1000, 0).await;
    send_message(&s, "msg2", "peer_b", "two", 2000, 0).await;

    let entries = s.get_due_outbox_entries(5000, false, 10).await.unwrap();
    assert_eq!(
        entries,
        vec![
            ("msg1".to_string(), "peer_a".to_string(), 0),
            ("msg2".to_string(), "peer_b".to_string(), 0),
        ]
    ); // oldest first
    assert_eq!(
        s.get_outbox_payload("msg1").await.unwrap().as_deref(),
        Some(r#"{"payload":"msg1"}"#)
    );

    s.remove_outbox_message("msg1").await.unwrap();
    assert!(s.get_outbox_payload("msg1").await.unwrap().is_none());
    assert_eq!(
        s.get_due_outbox_entries(5000, false, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn messaging_message_pagination() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    for i in 1..=5 {
        send_message(
            &s,
            &format!("m{i}"),
            "bob",
            &format!("msg {i}"),
            i * 1000,
            0,
        )
        .await;
    }

    let page1 = s.get_dm_messages(&conv_id, 2, None, None).await.unwrap();
    assert_eq!(page1.len(), 2);
    // Messages are returned in ASC order after reversal
    assert_eq!(page1[0].id, "m4");
    assert_eq!(page1[1].id, "m5");

    let page2 = s
        .get_dm_messages(&conv_id, 2, Some(page1[0].timestamp), None)
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].id, "m2");
    assert_eq!(page2[1].id, "m3");
}

#[tokio::test]
async fn messaging_pagination_cursor_separates_equal_timestamps() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    // Five messages with the same timestamp, one older and one newer.
    send_message(&s, "old", "bob", "old", 500, 0).await;
    for id in ["a", "b", "c", "d", "e"] {
        send_message(&s, id, "bob", id, 1000, 0).await;
    }
    send_message(&s, "new", "bob", "new", 2000, 0).await;

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<(u64, String)> = None;
    loop {
        let page = s
            .get_dm_messages(
                &conv_id,
                2,
                cursor.as_ref().map(|c| c.0),
                cursor.as_ref().map(|c| c.1.as_str()),
            )
            .await
            .unwrap();
        let Some(oldest) = page.first() else { break };
        cursor = Some((oldest.timestamp, oldest.id.clone()));
        // Each page is ascending; collect newest first.
        seen.extend(page.iter().rev().map(|m| m.id.clone()));
    }
    assert_eq!(seen, ["new", "e", "d", "c", "b", "a", "old"]);

    // A timestamp-only cursor skips every message with that timestamp.
    let page = s
        .get_dm_messages(&conv_id, 10, Some(1000), None)
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, "old");
}

#[tokio::test]
async fn messaging_duplicate_message_id_changes_nothing() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    assert!(receive_message(&s, "m1", "bob", "original", 2000).await);

    // Same id again with other content and a later timestamp.
    let duplicate = make_stored_message("m1", &conv_id, "bob", "me", "replaced", 9000);
    let inserted = s
        .receive_dm_message("bob", &duplicate, "replaced")
        .await
        .unwrap();
    assert!(!inserted);

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos.len(), 1);
    assert_eq!(convos[0].unread_count, 1);
    assert_eq!(convos[0].last_message_preview, "original");
    assert_eq!(convos[0].last_message_at, 2000);
    let messages = s.get_dm_messages(&conv_id, 50, None, None).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "original");

    // An id that exists in another conversation creates no conversation row.
    let carol_conv = Storage::conversation_id("me", "carol");
    let other = make_stored_message("m1", &carol_conv, "carol", "me", "mine now", 9500);
    assert!(
        !s.receive_dm_message("carol", &other, "mine now")
            .await
            .unwrap()
    );
    assert_eq!(s.get_conversations().await.unwrap().len(), 1);
}

#[tokio::test]
async fn synced_messages_count_as_unread_only_if_incoming_and_unread() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    let incoming = make_stored_message("m1", &conv_id, "bob", "me", "in", 1000);
    let mut read_incoming = make_stored_message("m2", &conv_id, "bob", "me", "read", 2000);
    read_incoming.read = true;
    let outgoing = make_stored_message("m3", &conv_id, "me", "bob", "out", 3000);

    assert!(s.import_synced_dm("bob", &incoming, true).await.unwrap());
    assert!(
        s.import_synced_dm("bob", &read_incoming, true)
            .await
            .unwrap()
    );
    assert!(s.import_synced_dm("bob", &outgoing, false).await.unwrap());
    // A known id is skipped.
    assert!(!s.import_synced_dm("bob", &incoming, true).await.unwrap());

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos.len(), 1);
    assert_eq!(convos[0].peer_pubkey, "bob");
    assert_eq!(convos[0].unread_count, 1);
    assert_eq!(convos[0].last_message_preview, "out");
    assert_eq!(convos[0].last_message_at, 3000);
}

#[tokio::test]
async fn newest_dm_ids_and_lookup_by_id() {
    let s = test_storage().await;
    for (id, ts) in [("m1", 1000), ("m2", 3000), ("m3", 2000)] {
        assert!(receive_message(&s, id, "bob", id, ts).await);
    }
    assert_eq!(
        s.get_newest_dm_ids(10).await.unwrap(),
        vec!["m2", "m3", "m1"]
    );
    assert_eq!(s.get_newest_dm_ids(2).await.unwrap(), vec!["m2", "m3"]);

    let ids = ["m2".to_string(), "missing".to_string(), "m1".to_string()];
    let found: Vec<String> = s
        .get_dm_messages_by_ids(&ids)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(found, vec!["m1", "m2"]);
    assert!(s.get_dm_messages_by_ids(&[]).await.unwrap().is_empty());
}

#[tokio::test]
async fn messaging_older_message_keeps_newer_preview() {
    let s = test_storage().await;
    assert!(receive_message(&s, "m2", "bob", "newer", 5000).await);
    // A message queued by the sender earlier arrives later.
    assert!(receive_message(&s, "m1", "bob", "older", 1000).await);

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos[0].last_message_preview, "newer");
    assert_eq!(convos[0].last_message_at, 5000);
    assert_eq!(convos[0].unread_count, 2);
}

#[tokio::test]
async fn messaging_receipts_are_scoped_to_conversation_and_sender() {
    let s = test_storage().await;
    let bob_conv = Storage::conversation_id("me", "bob");
    let carol_conv = Storage::conversation_id("me", "carol");
    send_message(&s, "to_bob", "bob", "for bob", 1000, 0).await;
    send_message(&s, "to_carol", "carol", "for carol", 1100, 0).await;
    assert!(receive_message(&s, "from_bob", "bob", "hi", 1200).await);

    // Bob names a message of the conversation with Carol.
    assert!(
        !s.mark_dm_delivered(&bob_conv, "me", "to_carol")
            .await
            .unwrap()
    );
    assert!(
        !s.mark_dm_read_by_id(&bob_conv, "me", "to_carol")
            .await
            .unwrap()
    );
    // Bob names a message he sent himself.
    assert!(
        !s.mark_dm_delivered(&bob_conv, "me", "from_bob")
            .await
            .unwrap()
    );
    assert!(
        !s.mark_dm_read_by_id(&bob_conv, "me", "from_bob")
            .await
            .unwrap()
    );
    // Unknown id.
    assert!(
        !s.mark_dm_delivered(&bob_conv, "me", "nothing")
            .await
            .unwrap()
    );

    let carol_msgs = s
        .get_dm_messages(&carol_conv, 50, None, None)
        .await
        .unwrap();
    assert!(!carol_msgs[0].delivered && !carol_msgs[0].read);
    assert!(s.get_outbox_payload("to_carol").await.unwrap().is_some());
    let bob_msgs = s.get_dm_messages(&bob_conv, 50, None, None).await.unwrap();
    let from_bob = bob_msgs.iter().find(|m| m.id == "from_bob").unwrap();
    assert!(!from_bob.read);

    // The message the local user sent to Bob is updated, and its outbox entry removed.
    assert!(
        s.mark_dm_delivered(&bob_conv, "me", "to_bob")
            .await
            .unwrap()
    );
    assert!(
        s.mark_dm_read_by_id(&bob_conv, "me", "to_bob")
            .await
            .unwrap()
    );
    let bob_msgs = s.get_dm_messages(&bob_conv, 50, None, None).await.unwrap();
    let to_bob = bob_msgs.iter().find(|m| m.id == "to_bob").unwrap();
    assert!(to_bob.delivered && to_bob.read);
    assert!(s.get_outbox_payload("to_bob").await.unwrap().is_none());
}

#[tokio::test]
async fn messaging_delete_removes_outbox_entry() {
    let s = test_storage().await;
    send_message(&s, "m1", "bob", "queued", 1000, 0).await;
    send_message(&s, "m2", "bob", "kept", 2000, 0).await;
    assert!(s.get_outbox_payload("m1").await.unwrap().is_some());

    assert!(s.delete_dm_message("m1").await.unwrap());
    assert!(s.get_outbox_payload("m1").await.unwrap().is_none());
    let due = s.get_due_outbox_entries(u64::MAX, true, 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].0, "m2");
}

#[tokio::test]
async fn messaging_outbox_new_entry_is_not_due_before_its_time() {
    let s = test_storage().await;
    send_message(&s, "m1", "bob", "first attempt running", 1000, 60_000).await;

    assert!(
        s.get_due_outbox_entries(59_999, false, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // A forced pass selects only entries with a failed attempt.
    assert!(
        s.get_due_outbox_entries(59_999, true, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(s.make_peer_outbox_due("bob", 2000).await.unwrap(), 0);
    assert_eq!(
        s.get_due_outbox_entries(60_000, false, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn messaging_outbox_failure_sets_attempts_and_delays_the_peer() {
    let s = test_storage().await;
    send_message(&s, "b1", "bob", "1", 1000, 0).await;
    send_message(&s, "b2", "bob", "2", 2000, 0).await;
    send_message(&s, "c1", "carol", "3", 3000, 0).await;

    s.record_outbox_failure("b1", "bob", 50_000).await.unwrap();

    // Both entries for bob wait until 50_000; carol's entry is unaffected.
    let due = s.get_due_outbox_entries(10_000, false, 10).await.unwrap();
    assert_eq!(due, vec![("c1".to_string(), "carol".to_string(), 0)]);
    let due = s.get_due_outbox_entries(50_000, false, 10).await.unwrap();
    assert_eq!(
        due,
        vec![
            ("b1".to_string(), "bob".to_string(), 1),
            ("b2".to_string(), "bob".to_string(), 0),
            ("c1".to_string(), "carol".to_string(), 0),
        ]
    );

    // A later time of another entry is not moved earlier.
    s.record_outbox_failure("b2", "bob", 20_000).await.unwrap();
    let due = s.get_due_outbox_entries(20_000, false, 10).await.unwrap();
    assert_eq!(
        due.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        ["b2", "c1"]
    );

    // A forced pass includes entries with a failed attempt regardless of their time.
    let forced = s.get_due_outbox_entries(10_000, true, 10).await.unwrap();
    assert_eq!(
        forced.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        ["b1", "b2", "c1"]
    );
    // The limit applies after ordering by age.
    let limited = s.get_due_outbox_entries(10_000, true, 1).await.unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].0, "b1");

    // After the peer was seen, its failed entries are due at once.
    assert_eq!(s.make_peer_outbox_due("bob", 10_000).await.unwrap(), 2);
    assert_eq!(s.make_peer_outbox_due("bob", 10_000).await.unwrap(), 0);
    let due = s.get_due_outbox_entries(10_000, false, 10).await.unwrap();
    assert_eq!(due.len(), 3);
}

#[tokio::test]
async fn messaging_outbox_expiry() {
    let s = test_storage().await;
    send_message(&s, "old", "bob", "old", 1000, 0).await;
    send_message(&s, "new", "bob", "new", 9000, 0).await;

    assert_eq!(s.delete_expired_outbox(1000).await.unwrap(), 0);
    assert_eq!(s.delete_expired_outbox(5000).await.unwrap(), 1);
    assert!(s.get_outbox_payload("old").await.unwrap().is_none());
    assert!(s.get_outbox_payload("new").await.unwrap().is_some());

    // The message row stays; only the queued copy is discarded.
    let conv_id = Storage::conversation_id("me", "bob");
    assert_eq!(
        s.get_dm_messages(&conv_id, 50, None, None)
            .await
            .unwrap()
            .len(),
        2
    );
}

// ── Peer Identities ──────────────────────────────────────────────────────────

#[tokio::test]
async fn peer_identity_cache_and_retrieve() {
    let s = test_storage().await;
    let response = make_identity("master_pk", &["transport_1", "transport_2"], 1);
    s.cache_peer_identity(&response).await.unwrap();

    let ids = s.get_peer_transport_node_ids("master_pk").await.unwrap();
    assert_eq!(ids, vec!["transport_1", "transport_2"]);
}

#[tokio::test]
async fn peer_announcement_keeps_device_list_unless_version_increases() {
    let s = test_storage().await;
    let v10 = make_identity("master_pk", &["transport_1"], 10);
    assert_eq!(
        s.cache_peer_announcement(&v10.announcement, None)
            .await
            .unwrap(),
        AnnouncementOutcome::Stored
    );

    // Equal and lower versions do not change the NodeIds.
    for version in [10, 5] {
        let old = make_identity("master_pk", &["old_device"], version);
        assert_eq!(
            s.cache_peer_announcement(&old.announcement, None)
                .await
                .unwrap(),
            AnnouncementOutcome::NotNewer
        );
    }
    assert_eq!(
        s.get_peer_transport_node_ids("master_pk").await.unwrap(),
        vec!["transport_1"]
    );

    let v11 = make_identity("master_pk", &["transport_2"], 11);
    assert_eq!(
        s.cache_peer_announcement(&v11.announcement, None)
            .await
            .unwrap(),
        AnnouncementOutcome::Stored
    );
    assert_eq!(
        s.get_peer_transport_node_ids("master_pk").await.unwrap(),
        vec!["transport_2"]
    );
}

#[tokio::test]
async fn profile_is_replaced_only_by_equal_or_newer_version() {
    let s = test_storage().await;
    let mut newer = make_profile("newer");
    newer.updated_at = 20;
    assert!(s.save_profile("pk", &newer).await.unwrap());

    let mut older = make_profile("older");
    older.updated_at = 10;
    assert!(!s.save_profile("pk", &older).await.unwrap());
    assert_eq!(
        s.get_profile("pk").await.unwrap().unwrap().display_name,
        "newer"
    );

    let mut newest = make_profile("newest");
    newest.updated_at = 30;
    assert!(s.save_profile("pk", &newest).await.unwrap());
    let stored = s.get_profile("pk").await.unwrap().unwrap();
    assert_eq!(stored.display_name, "newest");
    assert_eq!(stored.updated_at, 30);
}

#[tokio::test]
async fn peer_identity_reverse_lookups() {
    let s = test_storage().await;
    let response = make_identity("master_pk", &["transport_node_1"], 1);
    s.cache_peer_identity(&response).await.unwrap();

    // Transport NodeId -> master pubkey
    let master = s
        .get_master_pubkey_for_transport("transport_node_1")
        .await
        .unwrap();
    assert_eq!(master, "master_pk");

    // Non-existent lookup
    assert!(
        s.get_master_pubkey_for_transport("nonexistent")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn peer_identity_empty_returns() {
    let s = test_storage().await;
    assert!(
        s.get_peer_transport_node_ids("nope")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn peer_transport_lookup_maps_only_the_delivering_device() {
    let s = test_storage().await;
    let lookup = |node: &'static str| {
        let s = &s;
        async move { s.get_master_pubkey_for_transport(node).await }
    };
    // node_1 delivers a list of node_1 and node_2: only node_1 is mapped.
    let v1 = make_identity("master_a", &["node_1", "node_2"], 1);
    s.cache_peer_identity(&v1).await.unwrap();
    assert_eq!(lookup("node_1").await.as_deref(), Some("master_a"));
    assert!(lookup("node_2").await.is_none());

    // node_2 delivers the cached version: mapped.
    let mut from_node_2 = v1.clone();
    from_node_2.node_id = "node_2".to_string();
    s.cache_peer_identity(&from_node_2).await.unwrap();
    assert_eq!(lookup("node_2").await.as_deref(), Some("master_a"));

    // A newer list without node_1 removes its mapping and keeps node_2.
    let mut v2 = make_identity("master_a", &["node_2", "node_3"], 2);
    v2.node_id = "node_3".to_string();
    s.cache_peer_identity(&v2).await.unwrap();
    assert!(lookup("node_1").await.is_none());
    assert_eq!(lookup("node_2").await.as_deref(), Some("master_a"));
    assert_eq!(lookup("node_3").await.as_deref(), Some("master_a"));

    // node_1 delivers the older list again; the cached list does not contain it.
    s.cache_peer_identity(&v1).await.unwrap();
    assert!(lookup("node_1").await.is_none());
}

#[tokio::test]
async fn device_list_of_another_master_does_not_take_over_a_node() {
    let s = test_storage().await;
    let a = make_identity("master_a", &["node_1"], 1);
    s.cache_peer_identity(&a).await.unwrap();

    // master_b names node_1 in a newer list, delivered by its own device or by gossip.
    let b = make_identity("master_b", &["node_9", "node_1"], 5);
    s.cache_peer_identity(&b).await.unwrap();
    s.cache_peer_announcement(&b.announcement, None)
        .await
        .unwrap();
    assert_eq!(
        s.get_master_pubkey_for_transport("node_1").await.as_deref(),
        Some("master_a")
    );
    assert_eq!(
        s.get_master_pubkey_for_transport("node_9").await.as_deref(),
        Some("master_b")
    );
    // The forward list of master_b still names both NodeIds.
    assert_eq!(
        s.get_peer_transport_node_ids("master_b").await.unwrap(),
        vec!["node_9", "node_1"]
    );

    // node_1 delivers the list of master_b itself: the device moved to master_b.
    let mut from_node_1 = b;
    from_node_1.node_id = "node_1".to_string();
    s.cache_peer_identity(&from_node_1).await.unwrap();
    assert_eq!(
        s.get_master_pubkey_for_transport("node_1").await.as_deref(),
        Some("master_b")
    );
}

// ── Follow Requests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn follow_request_insert_and_list() {
    let s = test_storage().await;
    let outcome = s
        .record_follow_request("requester_a", 1000, 1001, 9999)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::NewPending);

    let requests = s.get_follow_requests(1001).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].pubkey, "requester_a");
    assert_eq!(requests[0].status, "pending");
    assert_eq!(s.get_pending_follow_request_count(1001).await.unwrap(), 1);

    // A repeated request leaves the row unchanged.
    let outcome = s
        .record_follow_request("requester_a", 2000, 2001, 19999)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::StillPending);
    let requests = s.get_follow_requests(2001).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].timestamp, 1000);
    assert_eq!(requests[0].expires_at, 9999);
}

#[tokio::test]
async fn follow_request_approve() {
    let s = test_storage().await;
    s.record_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();
    assert!(!s.is_approved_follower("r1").await.unwrap());
    assert!(!s.is_follower("me", "r1").await.unwrap());

    assert!(s.approve_follow_request("me", "r1", 1002).await.unwrap());
    assert!(s.is_approved_follower("r1").await.unwrap());
    // The follower row is written in the same transaction.
    assert!(s.is_follower("me", "r1").await.unwrap());
    assert_eq!(s.get_pending_follow_request_count(1002).await.unwrap(), 0);

    // Approving again reports that no pending request exists.
    assert!(!s.approve_follow_request("me", "r1", 1003).await.unwrap());
}

#[tokio::test]
async fn follow_request_deny() {
    let s = test_storage().await;
    s.record_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();

    assert!(s.deny_follow_request("r1", 1002).await.unwrap());
    assert!(!s.is_approved_follower("r1").await.unwrap());
    assert_eq!(s.get_pending_follow_request_count(1002).await.unwrap(), 0);
}

#[tokio::test]
async fn follow_request_denied_stays_denied_until_expiry() {
    let s = test_storage().await;
    s.record_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();
    assert!(s.deny_follow_request("r1", 1002).await.unwrap());

    // A repeated request does not return the row to pending.
    let outcome = s
        .record_follow_request("r1", 2000, 2001, 19999)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::Denied);
    assert_eq!(s.get_pending_follow_request_count(2001).await.unwrap(), 0);
    assert!(!s.approve_follow_request("me", "r1", 2002).await.unwrap());
    assert_eq!(
        s.approve_follower_directly("me", "r1", 2000, 2003, 19999)
            .await
            .unwrap(),
        DirectApproval::Denied
    );
    assert!(!s.is_approved_follower("r1").await.unwrap());

    // After the denial expired the row is deleted and a new request is pending.
    let outcome = s
        .record_follow_request("r1", 10_000, 10_000, 20_000)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::NewPending);
}

#[tokio::test]
async fn follow_request_skip_if_approved() {
    let s = test_storage().await;
    s.record_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();
    s.approve_follow_request("me", "r1", 1002).await.unwrap();

    let outcome = s
        .record_follow_request("r1", 2000, 2001, 9999)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::Approved);

    // An approved row does not expire.
    let outcome = s
        .record_follow_request("r1", 50_000, 50_000, 60_000)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::Approved);
    assert!(s.is_approved_follower("r1").await.unwrap());
    assert_eq!(s.get_follow_requests(50_000).await.unwrap().len(), 1);
}

#[tokio::test]
async fn follow_request_expired_pending_is_not_listed_or_approvable() {
    let s = test_storage().await;
    s.record_follow_request("r1", 1000, 1001, 5000)
        .await
        .unwrap();

    assert_eq!(s.get_pending_follow_request_count(5000).await.unwrap(), 0);
    assert!(s.get_follow_requests(5000).await.unwrap().is_empty());
    assert!(!s.approve_follow_request("me", "r1", 5000).await.unwrap());
    assert!(!s.deny_follow_request("r1", 5000).await.unwrap());
    assert!(!s.is_approved_follower("r1").await.unwrap());

    // The next request from anyone deletes the expired row.
    s.record_follow_request("r2", 6000, 6000, 9000)
        .await
        .unwrap();
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM follow_requests")
        .fetch_one(&s.pool)
        .await
        .unwrap();
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn follow_request_pending_rows_are_capped_by_removing_the_oldest() {
    let s = test_storage().await;
    let max = crate::constants::MAX_PENDING_FOLLOW_REQUESTS;
    for i in 0..max {
        let outcome = s
            .record_follow_request(&format!("requester_{i}"), 1, 1000 + i, 90_000)
            .await
            .unwrap();
        assert_eq!(outcome, FollowRequestOutcome::NewPending);
    }
    let outcome = s
        .record_follow_request("one_more", 1, 1000 + max, 90_000)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::NewPending);
    assert_eq!(
        s.get_pending_follow_request_count(1000 + max)
            .await
            .unwrap(),
        max
    );
    // The oldest request was removed; the others are answered from their rows.
    let outcome = s
        .record_follow_request("requester_1", 1, 1000 + max, 90_000)
        .await
        .unwrap();
    assert_eq!(outcome, FollowRequestOutcome::StillPending);
    let requests = s.get_follow_requests(1000 + max).await.unwrap();
    assert!(!requests.iter().any(|r| r.pubkey == "requester_0"));
}

#[tokio::test]
async fn follow_request_direct_approval_writes_request_and_follower() {
    let s = test_storage().await;
    assert_eq!(
        s.approve_follower_directly("me", "r1", 1000, 1001, 9999)
            .await
            .unwrap(),
        DirectApproval::New
    );
    assert!(s.is_approved_follower("r1").await.unwrap());
    assert!(s.is_follower("me", "r1").await.unwrap());
    // A second request of the same follower.
    assert_eq!(
        s.approve_follower_directly("me", "r1", 2000, 2001, 9999)
            .await
            .unwrap(),
        DirectApproval::Repeated
    );

    // A pending row is approved by the same call.
    s.record_follow_request("r2", 1000, 1001, 9999)
        .await
        .unwrap();
    assert_eq!(
        s.approve_follower_directly("me", "r2", 1000, 1002, 9999)
            .await
            .unwrap(),
        DirectApproval::New
    );
    assert!(s.is_approved_follower("r2").await.unwrap());
}

#[tokio::test]
async fn follow_request_deny_nonexistent() {
    let s = test_storage().await;
    assert!(!s.deny_follow_request("nobody", 1000).await.unwrap());
}

// ── Servers ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn server_add_get_and_remove() {
    let s = test_storage().await;
    assert!(s.get_server("https://a.test").await.unwrap().is_none());

    s.add_server("https://a.test", "A", "first", "node_a")
        .await
        .unwrap();
    s.add_server("https://b.test", "B", "second", "")
        .await
        .unwrap();

    let a = s.get_server("https://a.test").await.unwrap().unwrap();
    assert_eq!(a.name, "A");
    assert_eq!(a.description, "first");
    assert_eq!(a.node_id, "node_a");
    assert_eq!(a.registered_at, None);
    assert_eq!(a.visibility, "public");
    assert_eq!(s.get_servers().await.unwrap().len(), 2);

    assert!(s.remove_server("https://a.test").await.unwrap());
    assert!(!s.remove_server("https://a.test").await.unwrap());
    assert_eq!(s.get_servers().await.unwrap().len(), 1);
}

#[tokio::test]
async fn servers_without_the_current_profile_are_listed_until_they_accept_it() {
    let s = test_storage().await;
    for url in ["https://a.test", "https://b.test", "https://c.test"] {
        s.add_server(url, "", "", "").await.unwrap();
    }
    s.mark_server_registered("https://a.test", "public")
        .await
        .unwrap();
    s.mark_server_registered("https://b.test", "public")
        .await
        .unwrap();
    // c.test is not registered and is never listed.
    assert_eq!(
        s.get_servers_without_profile(10).await.unwrap(),
        vec!["https://a.test", "https://b.test"]
    );
    s.mark_server_profile_synced("https://a.test", 10)
        .await
        .unwrap();
    assert_eq!(
        s.get_servers_without_profile(10).await.unwrap(),
        vec!["https://b.test"]
    );
    // An older accepted version does not lower the stored one; a newer profile lists
    // the server again.
    s.mark_server_profile_synced("https://a.test", 5)
        .await
        .unwrap();
    assert_eq!(
        s.get_servers_without_profile(10).await.unwrap(),
        vec!["https://b.test"]
    );
    assert_eq!(
        s.get_servers_without_profile(11).await.unwrap(),
        vec!["https://a.test", "https://b.test"]
    );
}

#[tokio::test]
async fn server_registration_state() {
    let s = test_storage().await;
    s.add_server("https://a.test", "A", "", "").await.unwrap();
    let server = || async { s.get_server("https://a.test").await.unwrap().unwrap() };
    assert!(server().await.registered_at.is_none());

    s.mark_server_registered("https://a.test", "listed")
        .await
        .unwrap();
    let registered = server().await;
    assert_eq!(registered.visibility, "listed");
    assert!(registered.registered_at.is_some());

    // Adding the same URL again replaces the description and keeps the registration.
    s.add_server("https://a.test", "A2", "new", "node")
        .await
        .unwrap();
    let a = s.get_server("https://a.test").await.unwrap().unwrap();
    assert_eq!(a.name, "A2");
    assert!(a.registered_at.is_some());
    assert_eq!(a.visibility, "listed");

    s.mark_server_unregistered("https://a.test").await.unwrap();
    assert!(server().await.registered_at.is_none());
}

#[tokio::test]
async fn server_updates_fail_for_unknown_url() {
    let s = test_storage().await;
    s.add_server("https://a.test", "A", "", "").await.unwrap();

    // A URL that differs from the stored one (here by a trailing slash) matches no row.
    assert!(
        s.mark_server_registered("https://a.test/", "public")
            .await
            .is_err()
    );
    assert!(s.mark_server_unregistered("https://b.test").await.is_err());
    assert!(
        s.update_server_info("https://b.test", "B", "", "")
            .await
            .is_err()
    );
    let a = s.get_server("https://a.test").await.unwrap().unwrap();
    assert!(a.registered_at.is_none());
}

// ── Preferences ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn preference_set_get_and_overwrite() {
    let s = test_storage().await;
    assert_eq!(s.get_preference("accent_color").await.unwrap(), None);

    s.set_preference("accent_color", "teal").await.unwrap();
    s.set_preference("accent_color", "amber").await.unwrap();
    assert_eq!(
        s.get_preference("accent_color").await.unwrap().as_deref(),
        Some("amber")
    );
}

#[tokio::test]
async fn preference_bool_and_share_defaults() {
    let s = test_storage().await;
    // An absent boolean preference reads as false.
    assert!(!s.get_bool_preference("mdns_discovery").await.unwrap());
    s.set_bool_preference("mdns_discovery", true).await.unwrap();
    assert!(s.get_bool_preference("mdns_discovery").await.unwrap());
    s.set_bool_preference("mdns_discovery", false)
        .await
        .unwrap();
    assert!(!s.get_bool_preference("mdns_discovery").await.unwrap());

    // The list-sharing preferences read as true until they are set to false.
    assert!(crate::preferences::get_share_follows(&s).await);
    s.set_bool_preference(crate::preferences::SHARE_FOLLOWS, false)
        .await
        .unwrap();
    assert!(!crate::preferences::get_share_follows(&s).await);
    assert!(crate::preferences::get_share_followers(&s).await);
}

// ── Database file, schema, wipe ──────────────────────────────────────────────

#[tokio::test]
async fn storage_open_uses_the_path_literally() {
    // `?` and `#` are URL delimiters; the file must be created under this exact name.
    let dir = std::env::temp_dir().join(format!("proscenium-test-{}", crate::util::generate_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("so?cial#1.db");

    let s = Storage::open(&path).await.unwrap();
    s.set_preference("k", "v").await.unwrap();
    s.pool.close().await;
    assert!(path.is_file());

    let reopened = Storage::open(&path).await.unwrap();
    assert_eq!(
        reopened.get_preference("k").await.unwrap().as_deref(),
        Some("v")
    );
    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
    assert_eq!(journal_mode, "wal");
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&reopened.pool)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
    reopened.pool.close().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn schema_has_content_indexes_and_no_unused_objects() {
    let s = test_storage().await;
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema")
        .fetch_all(&s.pool)
        .await
        .unwrap();
    for index in [
        "idx_posts_id",
        "idx_notifications_post_id",
        "idx_notifications_target_post_id",
    ] {
        assert!(names.iter().any(|n| n == index), "missing {index}");
    }
    assert!(!names.iter().any(|n| n == "push_outbox"));
    let server_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('servers')")
            .fetch_all(&s.pool)
            .await
            .unwrap();
    assert!(!server_columns.iter().any(|c| c == "last_synced_at"));

    // Every migration is recorded once, with a millisecond timestamp.
    let applied: Vec<i64> = sqlx::query_scalar("SELECT applied_at FROM schema_migrations")
        .fetch_all(&s.pool)
        .await
        .unwrap();
    assert_eq!(applied.len(), Storage::MIGRATIONS.len());
    assert!(applied.iter().all(|t| *t > 1_600_000_000_000));
}

#[tokio::test]
async fn migrations_upgrade_a_database_created_before_015() {
    use std::str::FromStr;
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    // State left by the first 11 migrations, with `applied_at` in seconds.
    sqlx::raw_sql(
        "CREATE TABLE schema_migrations (name TEXT PRIMARY KEY, applied_at INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (name, sql) in Storage::MIGRATIONS.iter().take(11) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(*sql))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO schema_migrations (name, applied_at) VALUES (?1, 1700000000)")
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO servers (url, name, added_at, last_synced_at) VALUES ('https://a.test', 'A', 5, 7);
         INSERT INTO push_outbox (recipient, created_at, expires_at) VALUES ('r', 1, 2);
         INSERT INTO posts (id, author, content, timestamp) VALUES ('p1', 'a', 'text', 1000);",
    )
    .execute(&pool)
    .await
    .unwrap();

    Storage::run_migrations(&pool).await.unwrap();
    let s = Storage { pool };

    let server = s.get_server("https://a.test").await.unwrap().unwrap();
    assert_eq!(server.name, "A");
    assert_eq!(server.added_at, 5);
    assert!(s.get_post_by_id("p1").await.unwrap().is_some());
    let old_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schema_migrations WHERE applied_at = 1700000000000",
    )
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(old_rows, 11);
    let in_seconds: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM schema_migrations WHERE applied_at < 100000000000",
    )
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(in_seconds, 0);
}

#[tokio::test]
async fn newest_dm_ids_query_uses_an_index() {
    let s = test_storage().await;
    let rows: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT id FROM dm_messages ORDER BY timestamp DESC, id DESC LIMIT 10",
    )
    .fetch_all(&s.pool)
    .await
    .unwrap();
    let details: Vec<&str> = rows.iter().map(|r| r.3.as_str()).collect();
    assert!(
        details.iter().any(|d| d.contains("idx_dm_messages_time")),
        "{details:?}"
    );
    assert!(
        !details.iter().any(|d| d.contains("TEMP B-TREE")),
        "{details:?}"
    );
}

#[tokio::test]
async fn wipe_all_data_leaves_no_message_text_in_the_database_files() {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).unwrap();
    let dir = std::env::temp_dir().join(format!("proscenium-wipe-{}", hex::encode(random)));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("social.db");
    let marker = "wipe-marker-5f0c1d9e";
    {
        let s = Storage::open(&db).await.unwrap();
        let conv_id = Storage::conversation_id("me", "bob");
        for i in 0..50 {
            let id = format!("m{i}");
            s.insert_outgoing_dm(
                &make_stored_message(&id, &conv_id, "me", "bob", marker, 1000 + i),
                marker,
                "{}",
                0,
            )
            .await
            .unwrap();
        }
        s.wipe_all_data().await.unwrap();
        s.pool.close().await;
    }
    for name in ["social.db", "social.db-wal"] {
        let bytes = std::fs::read(dir.join(name)).unwrap_or_default();
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "{name} still holds the message text"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn wipe_all_data_empties_every_table_in_one_transaction() {
    let s = test_storage().await;
    s.save_profile("me", &make_profile("Me")).await.unwrap();
    s.insert_post(&make_post("p1", "me", 1000)).await.unwrap();
    s.save_interaction(&make_interaction("i1", "me", "p1", "me", 1000))
        .await
        .unwrap();
    s.insert_notification("like", "a", Some("p1"), None, 1000)
        .await
        .unwrap();
    s.toggle_bookmark("p1").await.unwrap();
    s.mute_user("someone").await.unwrap();
    s.add_server("https://a.test", "A", "", "").await.unwrap();
    s.set_preference("k", "v").await.unwrap();
    follow(&s, "me", "friend").await;
    // `dm_messages` has a foreign key to `dm_conversations`; this also fills `dm_outbox`.
    let conv_id = Storage::conversation_id("me", "bob");
    s.insert_outgoing_dm(
        &make_stored_message("m1", &conv_id, "me", "bob", "hello", 1000),
        "hello",
        "{}",
        0,
    )
    .await
    .unwrap();

    s.wipe_all_data().await.unwrap();

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&s.pool)
    .await
    .unwrap();
    assert!(tables.len() > 10);
    for table in tables {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&s.pool)
                .await
                .unwrap();
        if table == "schema_migrations" {
            assert_eq!(count as usize, Storage::MIGRATIONS.len());
        } else {
            assert_eq!(count, 0, "{table} is not empty");
        }
    }
}

#[tokio::test]
async fn count_followers_matches_get_followers() {
    let s = test_storage().await;
    assert_eq!(s.count_followers("me").await.unwrap(), 0);
    s.upsert_follower("me", "f1", 1000).await.unwrap();
    s.upsert_follower("me", "f2", 2000).await.unwrap();
    s.upsert_follower("other", "f3", 3000).await.unwrap();
    // A follow by the local user is not a follower row.
    follow(&s, "me", "f4").await;

    assert_eq!(
        s.count_followers("me").await.unwrap() as usize,
        s.get_followers("me").await.unwrap().len()
    );
    assert_eq!(s.count_followers("me").await.unwrap(), 2);
}

// -- Linked devices, device sync, link bundle --

/// Canonical public key string derived from a one-byte seed.
fn pubkey(seed: u8) -> String {
    iroh::SecretKey::from_bytes(&[seed; 32])
        .public()
        .to_string()
}

fn follow_entry(pubkey: &str, state: &str, last_changed_at: u64) -> FollowEntry {
    FollowEntry {
        pubkey: pubkey.to_string(),
        followed_at: 1,
        state: state.to_string(),
        last_changed_at,
    }
}

fn moderation_entry(
    pubkey: &str,
    kind: &str,
    state: &str,
    last_changed_at: u64,
) -> ModerationEntry {
    ModerationEntry {
        pubkey: pubkey.to_string(),
        kind: kind.to_string(),
        created_at: 1,
        state: state.to_string(),
        last_changed_at,
    }
}

fn bookmark_entry(post_id: &str, state: &str, last_changed_at: u64) -> BookmarkEntry {
    BookmarkEntry {
        post_id: post_id.to_string(),
        created_at: 1,
        state: state.to_string(),
        last_changed_at,
    }
}

fn device_entry(node_id: &str, name: &str) -> DeviceEntry {
    DeviceEntry {
        node_id: node_id.to_string(),
        device_name: name.to_string(),
        added_at: 1,
    }
}

/// A device announcement of the identity with master seed `master_seed`, signed so that
/// `verify_linked_devices_announcement` accepts it.
fn signed_announcement(master_seed: u8, node_seed: u8, version: u64) -> LinkedDevicesAnnouncement {
    let master = iroh::SecretKey::from_bytes(&[master_seed; 32]);
    let mut announcement = LinkedDevicesAnnouncement {
        master_pubkey: master.public().to_string(),
        devices: vec![device_entry(&pubkey(node_seed), "device")],
        removed: vec![],
        version,
        signature: String::new(),
    };
    sign_linked_devices_announcement(&mut announcement, &master);
    announcement
}

#[tokio::test]
async fn linked_devices_insert_missing_keeps_existing_rows() {
    let s = test_storage().await;
    let (own, other, third) = (pubkey(1), pubkey(2), pubkey(3));
    s.upsert_linked_device(&own, "Linux", true, 10)
        .await
        .unwrap();

    let inserted = s
        .insert_missing_linked_devices(&[
            device_entry(&own, "renamed"),
            device_entry(&other, "Android"),
        ])
        .await
        .unwrap();
    assert_eq!(inserted, 1);
    // A second call with one known and one new NodeId inserts only the new one.
    let inserted = s
        .insert_missing_linked_devices(&[
            device_entry(&other, "Android"),
            device_entry(&third, &"n".repeat(200)),
        ])
        .await
        .unwrap();
    assert_eq!(inserted, 1);

    let devices = s.get_linked_devices().await.unwrap();
    assert_eq!(devices.len(), 3);
    let own_row = devices.iter().find(|d| d.node_id == own).unwrap();
    assert_eq!(own_row.device_name, "Linux");
    // A device name is stored with at most MAX_DEVICE_NAME_LEN bytes.
    let third_row = devices.iter().find(|d| d.node_id == third).unwrap();
    assert!(third_row.device_name.len() <= MAX_DEVICE_NAME_LEN);

    // Only rows of other devices are returned for device sync.
    let mut others = s.get_other_device_node_ids().await.unwrap();
    others.sort();
    let mut expected = vec![other.clone(), third];
    expected.sort();
    assert_eq!(others, expected);
    for (node_id, is_other) in [(&own, false), (&other, true), (&pubkey(4), false)] {
        assert_eq!(s.is_other_device(node_id).await.unwrap(), is_other);
    }
}

#[tokio::test]
async fn removed_device_is_not_stored_again() {
    let s = test_storage().await;
    let (own, other) = (pubkey(1), pubkey(2));
    s.upsert_linked_device(&own, "Linux", true, 1)
        .await
        .unwrap();
    s.insert_missing_linked_devices(&[device_entry(&other, "Android")])
        .await
        .unwrap();

    // Only another stored device can be removed.
    assert!(!s.remove_linked_device(&own, 5).await.unwrap());
    assert!(!s.remove_linked_device(&pubkey(3), 5).await.unwrap());
    assert!(s.get_removed_device_ids().await.unwrap().is_empty());

    assert!(s.remove_linked_device(&other, 5).await.unwrap());
    assert!(!s.is_other_device(&other).await.unwrap());
    assert!(s.is_removed_device(&other).await.unwrap());
    assert_eq!(
        s.get_removed_device_ids().await.unwrap(),
        vec![other.clone()]
    );

    // A device list that still lists the removed device does not store it again.
    let inserted = s
        .insert_missing_linked_devices(&[device_entry(&other, "Android")])
        .await
        .unwrap();
    assert_eq!(inserted, 0);
    assert_eq!(s.get_linked_devices().await.unwrap().len(), 1);
}

#[tokio::test]
async fn merged_removals_delete_rows_and_detect_the_local_device() {
    let s = test_storage().await;
    let (own, other, third) = (pubkey(1), pubkey(2), pubkey(3));
    s.upsert_linked_device(&own, "Linux", true, 1)
        .await
        .unwrap();
    s.insert_missing_linked_devices(&[device_entry(&other, "Android")])
        .await
        .unwrap();

    let merged = s
        .merge_removed_devices(&[other.clone(), third.clone()], &own)
        .await
        .unwrap();
    assert_eq!(merged.devices_removed, 1);
    assert!(!merged.local_removed);
    assert!(s.is_removed_device(&third).await.unwrap());
    assert!(!s.is_other_device(&other).await.unwrap());

    // A list that removes the local device stores nothing.
    let fourth = pubkey(4);
    let merged = s
        .merge_removed_devices(&[fourth.clone(), own.clone()], &own)
        .await
        .unwrap();
    assert!(merged.local_removed);
    assert!(!s.is_removed_device(&fourth).await.unwrap());
    assert!(!s.is_removed_device(&own).await.unwrap());
}

#[tokio::test]
async fn removed_devices_keep_the_newest_records() {
    let s = test_storage().await;
    let own = pubkey(1);
    s.upsert_linked_device(&own, "Linux", true, 1)
        .await
        .unwrap();
    let count = MAX_REMOVED_DEVICES as u8 + 3;
    for i in 0..count {
        let node_id = pubkey(10 + i);
        s.insert_missing_linked_devices(&[device_entry(&node_id, "d")])
            .await
            .unwrap();
        assert!(
            s.remove_linked_device(&node_id, 100 + u64::from(i))
                .await
                .unwrap()
        );
    }
    let ids = s.get_removed_device_ids().await.unwrap();
    let expected: Vec<String> = (0..count)
        .rev()
        .take(MAX_REMOVED_DEVICES)
        .map(|i| pubkey(10 + i))
        .collect();
    assert_eq!(ids, expected);
    assert!(!s.is_removed_device(&pubkey(10)).await.unwrap());
}

#[tokio::test]
async fn has_identity_data_detects_own_device_row_or_profile() {
    let s = test_storage().await;
    assert!(!s.has_identity_data().await.unwrap());

    // A row of another device alone does not count.
    s.upsert_linked_device(&pubkey(2), "Android", false, 1)
        .await
        .unwrap();
    assert!(!s.has_identity_data().await.unwrap());
    s.upsert_linked_device(&pubkey(1), "Linux", true, 1)
        .await
        .unwrap();
    assert!(s.has_identity_data().await.unwrap());

    let s = test_storage().await;
    s.save_profile(&pubkey(1), &make_profile("me"))
        .await
        .unwrap();
    assert!(s.has_identity_data().await.unwrap());
}

#[tokio::test]
async fn bookmark_removal_is_kept_as_a_row() {
    let s = test_storage().await;
    let post_id = "ab".repeat(16);
    assert!(s.toggle_bookmark(&post_id).await.unwrap());
    assert!(!s.toggle_bookmark(&post_id).await.unwrap());
    assert!(!s.is_bookmarked(&post_id).await.unwrap());

    let lww = s.load_lww_state(&pubkey(1)).await.unwrap();
    assert_eq!(lww.bookmarks.len(), 1);
    assert_eq!(lww.bookmarks[0].state, "removed");
    assert!(lww.bookmarks[0].last_changed_at > 0);

    // Toggling again reuses the row.
    assert!(s.toggle_bookmark(&post_id).await.unwrap());
    assert!(s.is_bookmarked(&post_id).await.unwrap());
    let lww = s.load_lww_state(&pubkey(1)).await.unwrap();
    assert_eq!(lww.bookmarks.len(), 1);
    assert_eq!(lww.bookmarks[0].state, "active");
}

#[tokio::test]
async fn device_sync_does_not_undo_a_bookmark_removal() {
    let a = test_storage().await;
    let b = test_storage().await;
    let me = pubkey(1);
    let post_id = "cd".repeat(16);

    // Both devices hold the bookmark; device A then removes it.
    assert_eq!(
        b.merge_bookmarks_lww(&[bookmark_entry(&post_id, "active", 100)])
            .await
            .unwrap(),
        1
    );
    a.merge_bookmarks_lww(&[bookmark_entry(&post_id, "active", 100)])
        .await
        .unwrap();
    assert!(!a.toggle_bookmark(&post_id).await.unwrap());

    // B's older active row does not replace A's removal.
    let from_b = b.load_lww_state(&me).await.unwrap().bookmarks;
    assert_eq!(a.merge_bookmarks_lww(&from_b).await.unwrap(), 0);
    assert!(!a.is_bookmarked(&post_id).await.unwrap());

    // A's removal replaces B's active row.
    let from_a = a.load_lww_state(&me).await.unwrap().bookmarks;
    assert_eq!(b.merge_bookmarks_lww(&from_a).await.unwrap(), 1);
    assert!(!b.is_bookmarked(&post_id).await.unwrap());

    // Both devices now hold equal rows.
    assert_eq!(
        a.load_lww_state(&me).await.unwrap().digest(),
        b.load_lww_state(&me).await.unwrap().digest()
    );
}

fn request_entry(pubkey: &str, status: &str, decided_at: u64) -> FollowRequestEntry {
    FollowRequestEntry {
        pubkey: pubkey.to_string(),
        timestamp: 1,
        status: status.to_string(),
        created_at: 1,
        expires_at: u64::MAX,
        decided_at,
    }
}

async fn request_status(s: &Storage, requester: &str) -> Option<String> {
    s.get_follow_requests(now_millis())
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.pubkey == requester)
        .map(|r| r.status)
}

#[tokio::test]
async fn merged_follow_request_keeps_the_later_decision() {
    let s = test_storage().await;
    let (me, requester) = (pubkey(1), pubkey(2));

    // A pending request is copied; an approval replaces it and records the follower.
    let merged = s
        .merge_follow_requests(&me, &[request_entry(&requester, "pending", 0)])
        .await
        .unwrap();
    assert_eq!(merged, 1);
    assert_eq!(
        request_status(&s, &requester).await.as_deref(),
        Some("pending")
    );
    s.merge_follow_requests(&me, &[request_entry(&requester, "approved", 100)])
        .await
        .unwrap();
    assert!(s.is_approved_follower(&requester).await.unwrap());
    assert!(s.is_follower(&me, &requester).await.unwrap());

    // An earlier decision and a pending copy do not replace it; a later one does.
    for entry in [
        request_entry(&requester, "denied", 50),
        request_entry(&requester, "pending", 0),
    ] {
        assert_eq!(s.merge_follow_requests(&me, &[entry]).await.unwrap(), 0);
    }
    assert_eq!(
        request_status(&s, &requester).await.as_deref(),
        Some("approved")
    );
    s.merge_follow_requests(&me, &[request_entry(&requester, "denied", 200)])
        .await
        .unwrap();
    assert_eq!(
        request_status(&s, &requester).await.as_deref(),
        Some("denied")
    );

    // Invalid, own and expired entries are skipped.
    let mut expired = request_entry(&pubkey(3), "pending", 0);
    expired.expires_at = 1;
    let skipped = [
        request_entry("not-a-pubkey", "approved", 1),
        request_entry(&me, "approved", 1),
        request_entry(&pubkey(4), "unknown", 1),
        expired,
    ];
    assert_eq!(s.merge_follow_requests(&me, &skipped).await.unwrap(), 0);
}

#[tokio::test]
async fn merge_follows_lww_newer_row_wins() {
    let s = test_storage().await;
    let (me, followee) = (pubkey(1), pubkey(2));

    assert_eq!(
        s.merge_follows_lww(&me, &[follow_entry(&followee, "active", 100)])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(s.is_following(&me, &followee).await.unwrap());

    // Equal and older timestamps do not replace the row.
    for changed in [100, 50] {
        assert_eq!(
            s.merge_follows_lww(&me, &[follow_entry(&followee, "removed", changed)])
                .await
                .unwrap()
                .len(),
            0
        );
    }
    assert!(s.is_following(&me, &followee).await.unwrap());

    assert_eq!(
        s.merge_follows_lww(&me, &[follow_entry(&followee, "removed", 200)])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!s.is_following(&me, &followee).await.unwrap());
    let lww = s.load_lww_state(&me).await.unwrap();
    assert_eq!(lww.follows.len(), 1);
    assert_eq!(lww.follows[0].state, "removed");
    assert_eq!(lww.follows[0].last_changed_at, 200);
}

#[tokio::test]
async fn merge_follows_lww_skips_invalid_entries_and_limits_timestamps() {
    let s = test_storage().await;
    let (me, followee) = (pubkey(1), pubkey(2));

    let merged = s
        .merge_follows_lww(
            &me,
            &[
                follow_entry("not-a-pubkey", "active", 100),
                follow_entry(&me, "active", 100),
                follow_entry(&pubkey(3), "pending", 100),
            ],
        )
        .await
        .unwrap();
    assert!(merged.is_empty());
    assert!(s.load_lww_state(&me).await.unwrap().follows.is_empty());

    // A timestamp later than the local clock is stored as the local time, so a local
    // change made afterwards has a greater `last_changed_at`.
    s.merge_follows_lww(&me, &[follow_entry(&followee, "active", u64::MAX)])
        .await
        .unwrap();
    let stored = s.load_lww_state(&me).await.unwrap().follows[0].last_changed_at;
    assert!(stored <= now_millis());
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    s.unfollow(&me, &followee).await.unwrap();
    let lww = s.load_lww_state(&me).await.unwrap();
    assert_eq!(lww.follows[0].state, "removed");
    assert!(lww.follows[0].last_changed_at > stored);
}

#[tokio::test]
async fn merge_moderation_lww_newer_row_wins() {
    let s = test_storage().await;
    let target = pubkey(2);

    let merged = s
        .merge_moderation_lww(&[
            moderation_entry(&target, "mute", "active", 100),
            moderation_entry(&target, "block", "active", 100),
            moderation_entry(&target, "ban", "active", 100),
            moderation_entry("not-a-pubkey", "mute", "active", 100),
        ])
        .await
        .unwrap();
    assert_eq!(merged, 2);
    assert!(s.is_muted(&target).await.unwrap());
    assert!(s.is_blocked(&target).await.unwrap());

    // An older removal is ignored, a newer one is applied; the block row is unchanged.
    s.merge_moderation_lww(&[moderation_entry(&target, "mute", "removed", 50)])
        .await
        .unwrap();
    assert!(s.is_muted(&target).await.unwrap());
    s.merge_moderation_lww(&[moderation_entry(&target, "mute", "removed", 150)])
        .await
        .unwrap();
    assert!(!s.is_muted(&target).await.unwrap());
    assert!(s.is_blocked(&target).await.unwrap());
}

#[tokio::test]
async fn lww_state_leaves_out_rows_that_a_merge_would_skip() {
    let s = test_storage().await;
    let me = pubkey(1);
    // Rows written by local commands with keys that are not canonical.
    s.mute_user("bad_user").await.unwrap();
    s.toggle_bookmark("p1").await.unwrap();
    let entry = SocialGraphEntry {
        pubkey: "alice".to_string(),
        followed_at: 1,
        first_seen: 0,
        last_seen: 0,
        is_online: false,
    };
    s.follow(&me, &entry).await.unwrap();

    let lww = s.load_lww_state(&me).await.unwrap();
    assert!(lww.follows.is_empty());
    assert!(lww.moderation.is_empty());
    assert!(lww.bookmarks.is_empty());
}

#[tokio::test]
async fn stale_device_lists_of_unrelated_users_are_pruned() {
    let s = test_storage().await;
    let me = pubkey(1);
    let followed = signed_announcement(20, 21, 5);
    let messaged = signed_announcement(30, 31, 5);
    let stranger = signed_announcement(40, 41, 5);
    for announcement in [&followed, &messaged, &stranger] {
        s.cache_peer_announcement(announcement, Some(&announcement.devices[0].node_id))
            .await
            .unwrap();
    }
    s.merge_follows_lww(&me, &[follow_entry(&followed.master_pubkey, "active", 100)])
        .await
        .unwrap();
    let conv_id = Storage::conversation_id(&me, &messaged.master_pubkey);
    s.insert_outgoing_dm(
        &make_stored_message("m1", &conv_id, &me, &messaged.master_pubkey, "hi", 1000),
        "hi",
        "{}",
        0,
    )
    .await
    .unwrap();

    // Lists cached after the cutoff are kept.
    assert_eq!(s.prune_peer_announcements(&me, 0).await.unwrap(), 0);
    assert_eq!(s.prune_peer_announcements(&me, u64::MAX).await.unwrap(), 1);
    assert!(
        s.get_peer_transport_node_ids(&stranger.master_pubkey)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.get_master_pubkey_for_transport(&pubkey(41))
            .await
            .is_none()
    );
    for kept in [&followed, &messaged] {
        assert_eq!(
            s.get_peer_transport_node_ids(&kept.master_pubkey)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn followed_peer_announcements_are_exported_for_active_follows() {
    let s = test_storage().await;
    let me = pubkey(1);
    let followed = signed_announcement(20, 21, 5);
    let unfollowed = signed_announcement(30, 31, 5);
    let stranger = signed_announcement(40, 41, 5);
    for announcement in [&followed, &unfollowed, &stranger] {
        s.cache_peer_announcement(announcement, None).await.unwrap();
    }
    s.merge_follows_lww(
        &me,
        &[
            follow_entry(&followed.master_pubkey, "active", 100),
            follow_entry(&unfollowed.master_pubkey, "removed", 100),
        ],
    )
    .await
    .unwrap();

    let exported = s.get_followed_peer_announcements(&me).await.unwrap();
    assert_eq!(exported.len(), 1);
    assert_eq!(exported[0].master_pubkey, followed.master_pubkey);
    verify_linked_devices_announcement(&exported[0]).unwrap();
}

#[tokio::test]
async fn link_bundle_import_stores_rows_under_the_bundle_identity() {
    let s = test_storage().await;
    let me = pubkey(1);
    let (existing_node, own_node) = (pubkey(10), pubkey(11));
    let followed = signed_announcement(20, 21, 5);
    let not_followed = signed_announcement(30, 31, 5);
    let mut forged = signed_announcement(40, 41, 5);
    forged.devices[0].node_id = pubkey(42);
    let bookmarked = "ab".repeat(16);
    let unbookmarked = "cd".repeat(16);

    let bundle = LinkBundleData {
        master_secret_key: String::new(),
        announcement: signed_announcement(1, 10, 5),
        profile: Some(make_profile("me")),
        follows: vec![
            follow_entry(&followed.master_pubkey, "active", 100),
            follow_entry(&forged.master_pubkey, "active", 100),
            follow_entry(&pubkey(50), "removed", 100),
        ],
        moderation: vec![
            moderation_entry(&pubkey(60), "block", "active", 100),
            moderation_entry(&pubkey(61), "mute", "active", 100),
            moderation_entry(&pubkey(62), "mute", "removed", 100),
        ],
        bookmarks: vec![
            bookmark_entry(&bookmarked, "active", 100),
            bookmark_entry(&unbookmarked, "removed", 100),
        ],
        follow_requests: vec![],
        peer_announcements: vec![followed.clone(), not_followed.clone(), forged.clone()],
    };
    let own_device = device_entry(&own_node, "Android");

    // Importing twice gives the same rows: bookmarks are inserted, not toggled.
    for _ in 0..2 {
        s.import_link_bundle(&me, &own_device, &bundle)
            .await
            .unwrap();
    }

    assert_eq!(
        s.get_profile(&me).await.unwrap().unwrap().display_name,
        "me"
    );

    let devices = s.get_linked_devices().await.unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(
        s.get_other_device_node_ids().await.unwrap(),
        vec![existing_node]
    );
    assert!(s.has_identity_data().await.unwrap());

    let follows: Vec<String> = s
        .get_follows(&me)
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.pubkey)
        .collect();
    assert_eq!(follows.len(), 2);
    assert!(follows.contains(&followed.master_pubkey));
    assert!(!follows.contains(&pubkey(50)));

    assert!(s.is_blocked(&pubkey(60)).await.unwrap());
    assert!(s.is_muted(&pubkey(61)).await.unwrap());
    assert!(!s.is_muted(&pubkey(62)).await.unwrap());

    assert!(s.is_bookmarked(&bookmarked).await.unwrap());
    assert!(!s.is_bookmarked(&unbookmarked).await.unwrap());

    // The transport NodeIds of a followed user come from its verified announcement.
    assert_eq!(
        s.get_peer_transport_node_ids(&followed.master_pubkey)
            .await
            .unwrap(),
        vec![pubkey(21)]
    );
    // Not followed: not stored. Followed but the signature does not verify: not stored.
    for master in [&not_followed.master_pubkey, &forged.master_pubkey] {
        assert!(
            s.get_peer_transport_node_ids(master)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // The imported rows keep the change times of the bundle.
    let lww = s.load_lww_state(&me).await.unwrap();
    assert!(lww.follows.iter().all(|f| f.last_changed_at == 100));
    assert!(lww.bookmarks.iter().all(|b| b.last_changed_at == 100));
}
