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
        signature: "sig".to_string(),
    }
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
    assert!(s.delete_post("p1").await.unwrap());
    assert!(s.get_post_by_id("p1").await.unwrap().is_none());
    assert!(!s.delete_post("p1").await.unwrap()); // already deleted
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
async fn post_get_after_with_pagination() {
    let s = test_storage().await;
    for i in 1..=10 {
        s.insert_post(&make_post(&format!("p{i}"), "a", i * 100))
            .await
            .unwrap();
    }

    let batch1 = s.get_posts_after("a", 0, 3, 0).await.unwrap();
    assert_eq!(batch1.len(), 3);
    assert_eq!(batch1[0].id, "p1"); // ASC order

    let batch2 = s.get_posts_after("a", 0, 3, 3).await.unwrap();
    assert_eq!(batch2.len(), 3);
    assert_eq!(batch2[0].id, "p4");
}

#[tokio::test]
async fn post_get_not_in() {
    let s = test_storage().await;
    s.insert_post(&make_post("p1", "a", 1000)).await.unwrap();
    s.insert_post(&make_post("p2", "a", 2000)).await.unwrap();
    s.insert_post(&make_post("p3", "a", 3000)).await.unwrap();

    let unknown = s
        .get_posts_not_in("a", &["p1".into(), "p3".into()], 100, 0)
        .await
        .unwrap();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].id, "p2");

    // Empty known_ids returns all
    let all = s.get_posts_not_in("a", &[], 100, 0).await.unwrap();
    assert_eq!(all.len(), 3);
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

    let feed = s
        .get_feed(&FeedQuery {
            limit: 10,
            before: None,
        })
        .await
        .unwrap();
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

    let page1 = s
        .get_feed(&FeedQuery {
            limit: 2,
            before: None,
        })
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page1[0].id, "p3");

    let page2 = s
        .get_feed(&FeedQuery {
            limit: 2,
            before: Some(page1.last().unwrap().timestamp),
        })
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

    s.mute_user("muted_user").await.unwrap();
    s.block_user("blocked_user").await.unwrap();

    let feed = s
        .get_feed(&FeedQuery {
            limit: 10,
            before: None,
        })
        .await
        .unwrap();
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
    repost.quote_of = Some("target_post".to_string());
    repost.quote_of_author = Some("b".to_string());
    s.insert_post(&repost).await.unwrap();

    let deleted_id = s.delete_repost_by_target("a", "target_post").await.unwrap();
    assert_eq!(deleted_id, Some("rp1".to_string()));
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
    assert!(!s.is_mutual("me", "peer").await.unwrap()); // not a follower yet

    s.upsert_follower("me", "peer", 2000).await.unwrap();
    assert!(s.is_mutual("me", "peer").await.unwrap()); // now mutual
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

    let page2 = s
        .get_notifications(2, Some(page1.last().unwrap().timestamp))
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert!(page2[0].timestamp < page1.last().unwrap().timestamp);
}

// ── Messaging (DMs) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn messaging_conversation_id_is_symmetric() {
    let id1 = Storage::conversation_id("alice", "bob");
    let id2 = Storage::conversation_id("bob", "alice");
    assert_eq!(id1, id2);
}

#[tokio::test]
async fn messaging_conversation_id_is_deterministic() {
    let id1 = Storage::conversation_id("a", "b");
    let id2 = Storage::conversation_id("a", "b");
    assert_eq!(id1, id2);
}

#[tokio::test]
async fn messaging_upsert_and_get_conversations() {
    let s = test_storage().await;
    s.upsert_conversation("bob", "me", 1000, "hello!")
        .await
        .unwrap();
    s.upsert_conversation("carol", "me", 2000, "hey!")
        .await
        .unwrap();

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos.len(), 2);
    assert_eq!(convos[0].peer_pubkey, "carol"); // newest first
    assert_eq!(convos[0].last_message_preview, "hey!");
}

#[tokio::test]
async fn messaging_upsert_updates_preview() {
    let s = test_storage().await;
    s.upsert_conversation("bob", "me", 1000, "first message")
        .await
        .unwrap();
    s.upsert_conversation("bob", "me", 2000, "second message")
        .await
        .unwrap();

    let convos = s.get_conversations().await.unwrap();
    assert_eq!(convos.len(), 1);
    assert_eq!(convos[0].last_message_preview, "second message");
}

#[tokio::test]
async fn messaging_insert_and_get_messages() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    s.upsert_conversation("bob", "me", 1000, "hello")
        .await
        .unwrap();
    let msg = make_stored_message("m1", &conv_id, "me", "bob", "hello", 1000);
    s.insert_dm_message(&msg).await.unwrap();

    let messages = s.get_dm_messages(&conv_id, 50, None).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "hello");
    assert!(!messages[0].read);
    assert!(!messages[0].delivered);
}

#[tokio::test]
async fn messaging_mark_delivered_and_read() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    s.upsert_conversation("bob", "me", 1000, "hello")
        .await
        .unwrap();
    let msg = make_stored_message("m1", &conv_id, "bob", "me", "hello", 1000);
    s.insert_dm_message(&msg).await.unwrap();

    s.mark_dm_delivered("m1").await.unwrap();
    let messages = s.get_dm_messages(&conv_id, 50, None).await.unwrap();
    assert!(messages[0].delivered);
    assert!(!messages[0].read);

    s.mark_dm_read_by_id("m1").await.unwrap();
    let messages = s.get_dm_messages(&conv_id, 50, None).await.unwrap();
    assert!(messages[0].read);
}

#[tokio::test]
async fn messaging_delete_message() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    s.upsert_conversation("bob", "me", 1000, "oops")
        .await
        .unwrap();
    let msg = make_stored_message("m1", &conv_id, "me", "bob", "oops", 1000);
    s.insert_dm_message(&msg).await.unwrap();

    assert!(s.delete_dm_message("m1").await.unwrap());
    assert!(!s.delete_dm_message("m1").await.unwrap()); // already deleted

    let messages = s.get_dm_messages(&conv_id, 50, None).await.unwrap();
    assert!(messages.is_empty());
}

#[tokio::test]
async fn messaging_unread_count() {
    let s = test_storage().await;
    s.upsert_conversation("bob", "me", 1000, "msg1")
        .await
        .unwrap();
    assert_eq!(s.get_total_unread_count().await.unwrap(), 0);

    // Simulate receiving: use receive_dm_message_atomically which increments unread
    let conv_id = Storage::conversation_id("me", "bob");
    let msg = make_stored_message("m1", &conv_id, "bob", "me", "hello", 2000);
    s.receive_dm_message_atomically("dm_pk", "bob", "{}", 2000, &msg, "hello")
        .await
        .unwrap();
    assert_eq!(s.get_total_unread_count().await.unwrap(), 1);

    s.mark_conversation_read("bob", "me").await.unwrap();
    assert_eq!(s.get_total_unread_count().await.unwrap(), 0);
}

#[tokio::test]
async fn messaging_outbox() {
    let s = test_storage().await;
    s.insert_outbox_message("ob1", "peer_a", r#"{"encrypted":"data"}"#, 1000, "msg1")
        .await
        .unwrap();
    s.insert_outbox_message("ob2", "peer_b", r#"{"encrypted":"data2"}"#, 2000, "msg2")
        .await
        .unwrap();

    let entries = s.get_all_outbox_messages().await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, "ob1"); // oldest first

    s.remove_outbox_message("ob1").await.unwrap();
    assert_eq!(s.get_all_outbox_messages().await.unwrap().len(), 1);
}

#[tokio::test]
async fn messaging_message_pagination() {
    let s = test_storage().await;
    let conv_id = Storage::conversation_id("me", "bob");
    s.upsert_conversation("bob", "me", 5000, "msg 5")
        .await
        .unwrap();
    for i in 1..=5 {
        let msg = make_stored_message(
            &format!("m{i}"),
            &conv_id,
            "me",
            "bob",
            &format!("msg {i}"),
            i * 1000,
        );
        s.insert_dm_message(&msg).await.unwrap();
    }

    let page1 = s.get_dm_messages(&conv_id, 2, None).await.unwrap();
    assert_eq!(page1.len(), 2);
    // Messages are returned in ASC order after reversal
    assert_eq!(page1[0].id, "m4");
    assert_eq!(page1[1].id, "m5");

    let page2 = s
        .get_dm_messages(&conv_id, 2, Some(page1[0].timestamp))
        .await
        .unwrap();
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].id, "m2");
    assert_eq!(page2[1].id, "m3");
}

// ── Peer Delegations ─────────────────────────────────────────────────────────

#[tokio::test]
async fn peer_delegation_cache_and_retrieve() {
    let s = test_storage().await;
    let delegation = SigningKeyDelegation {
        master_pubkey: "master_pk".to_string(),
        signing_pubkey: "signing_pk".to_string(),
        key_index: 0,
        dm_pubkey: "dm_pk".to_string(),
        dm_key_index: 0,
        issued_at: 1000,
        signature: "sig".to_string(),
    };
    let response = IdentityResponse {
        master_pubkey: "master_pk".to_string(),
        delegation,
        transport_node_ids: vec!["transport_1".to_string(), "transport_2".to_string()],
        profile: None,
    };
    s.cache_peer_identity(&response).await.unwrap();

    // Signing pubkey
    let signing = s
        .get_peer_signing_pubkey("master_pk")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(signing, "signing_pk");

    // DM pubkey
    let dm = s.get_peer_dm_pubkey("master_pk").await.unwrap().unwrap();
    assert_eq!(dm, "dm_pk");

    // Transport node IDs
    let ids = s.get_peer_transport_node_ids("master_pk").await.unwrap();
    assert_eq!(ids, vec!["transport_1", "transport_2"]);

    // Delegation round-trip
    let d = s.get_peer_delegation("master_pk").await.unwrap().unwrap();
    assert_eq!(d.signing_pubkey, "signing_pk");
    assert_eq!(d.key_index, 0);
}

#[tokio::test]
async fn peer_delegation_reverse_lookups() {
    let s = test_storage().await;
    let delegation = SigningKeyDelegation {
        master_pubkey: "master_pk".to_string(),
        signing_pubkey: "signing_pk".to_string(),
        key_index: 0,
        dm_pubkey: "dm_pk_hex".to_string(),
        dm_key_index: 0,
        issued_at: 1000,
        signature: "sig".to_string(),
    };
    let response = IdentityResponse {
        master_pubkey: "master_pk".to_string(),
        delegation,
        transport_node_ids: vec!["transport_node_1".to_string()],
        profile: None,
    };
    s.cache_peer_identity(&response).await.unwrap();

    // DM pubkey -> master pubkey
    let master = s
        .get_master_pubkey_for_dm_pubkey("dm_pk_hex")
        .await
        .unwrap();
    assert_eq!(master, "master_pk");

    // Transport NodeId -> master pubkey
    let master = s
        .get_master_pubkey_for_transport("transport_node_1")
        .await
        .unwrap();
    assert_eq!(master, "master_pk");

    // Non-existent lookups
    assert!(
        s.get_master_pubkey_for_dm_pubkey("nonexistent")
            .await
            .is_none()
    );
    assert!(
        s.get_master_pubkey_for_transport("nonexistent")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn peer_delegation_empty_returns() {
    let s = test_storage().await;
    assert!(s.get_peer_delegation("nope").await.unwrap().is_none());
    assert!(s.get_peer_signing_pubkey("nope").await.unwrap().is_none());
    assert!(s.get_peer_dm_pubkey("nope").await.unwrap().is_none());
    assert!(
        s.get_peer_transport_node_ids("nope")
            .await
            .unwrap()
            .is_empty()
    );
}

// ── Ratchet Sessions ─────────────────────────────────────────────────────────

#[tokio::test]
async fn ratchet_save_and_get() {
    let s = test_storage().await;
    s.save_ratchet_session("peer_dm_pk", r#"{"state":"data"}"#, 1000)
        .await
        .unwrap();

    let loaded = s.get_ratchet_session("peer_dm_pk").await.unwrap().unwrap();
    assert_eq!(loaded, r#"{"state":"data"}"#);
}

#[tokio::test]
async fn ratchet_upsert() {
    let s = test_storage().await;
    s.save_ratchet_session("peer", r#"{"v":1}"#, 1000)
        .await
        .unwrap();
    s.save_ratchet_session("peer", r#"{"v":2}"#, 2000)
        .await
        .unwrap();

    let loaded = s.get_ratchet_session("peer").await.unwrap().unwrap();
    assert_eq!(loaded, r#"{"v":2}"#);
}

#[tokio::test]
async fn ratchet_nonexistent() {
    let s = test_storage().await;
    assert!(s.get_ratchet_session("nobody").await.unwrap().is_none());
}

// ── Follow Requests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn follow_request_insert_and_list() {
    let s = test_storage().await;
    let inserted = s
        .insert_follow_request("requester_a", 1000, 1001, 9999)
        .await
        .unwrap();
    assert!(inserted);

    let requests = s.get_follow_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].pubkey, "requester_a");
    assert_eq!(requests[0].status, "pending");
    assert_eq!(s.get_pending_follow_request_count().await.unwrap(), 1);
}

#[tokio::test]
async fn follow_request_approve() {
    let s = test_storage().await;
    s.insert_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();
    assert!(!s.is_approved_follower("r1").await.unwrap());

    assert!(s.approve_follow_request("r1").await.unwrap());
    assert!(s.is_approved_follower("r1").await.unwrap());
    assert_eq!(s.get_pending_follow_request_count().await.unwrap(), 0);
}

#[tokio::test]
async fn follow_request_deny() {
    let s = test_storage().await;
    s.insert_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();

    assert!(s.deny_follow_request("r1").await.unwrap());
    assert!(!s.is_approved_follower("r1").await.unwrap());
    assert_eq!(s.get_pending_follow_request_count().await.unwrap(), 0);
}

#[tokio::test]
async fn follow_request_skip_if_approved() {
    let s = test_storage().await;
    s.insert_follow_request("r1", 1000, 1001, 9999)
        .await
        .unwrap();
    s.approve_follow_request("r1").await.unwrap();

    // Re-inserting returns false because already approved
    let inserted = s
        .insert_follow_request("r1", 2000, 2001, 9999)
        .await
        .unwrap();
    assert!(!inserted);

    // Still approved
    assert!(s.is_approved_follower("r1").await.unwrap());
}

#[tokio::test]
async fn follow_request_deny_nonexistent() {
    let s = test_storage().await;
    assert!(!s.deny_follow_request("nobody").await.unwrap());
}
