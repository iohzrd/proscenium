use super::*;
use crate::content::fts_query;
use crate::test_util::{TestUser, node_id};
use futures_lite::StreamExt;
use proscenium_types::REGISTRATION_ACTION_UNREGISTER;

const URL: &str = "https://server.test";
const OPEN: RegistrationPolicy = RegistrationPolicy {
    registration_open: true,
    max_registered_users: 100,
};

async fn storage() -> Storage {
    Storage::open_in_memory().await.unwrap()
}

async fn register(
    storage: &Storage,
    user: &TestUser,
    timestamp: u64,
    visibility: Visibility,
) -> Result<ChangeOutcome, ChangeError> {
    let req = user.registration(URL, timestamp, visibility, None);
    storage
        .apply_registration_change(&RegistrationChange::Register(&req), &OPEN)
        .await
}

async fn unregister(
    storage: &Storage,
    user: &TestUser,
    timestamp: u64,
) -> Result<ChangeOutcome, ChangeError> {
    let req = user.registration(
        URL,
        timestamp,
        Visibility::Public,
        Some(REGISTRATION_ACTION_UNREGISTER),
    );
    storage
        .apply_registration_change(&RegistrationChange::Unregister(&req), &OPEN)
        .await
}

async fn put_profile(
    storage: &Storage,
    user: &TestUser,
    timestamp: u64,
    profile: Profile,
) -> Result<ChangeOutcome, ChangeError> {
    let req = user.profile_update(URL, timestamp, profile);
    storage
        .apply_registration_change(&RegistrationChange::UpdateProfile(&req), &OPEN)
        .await
}

async fn peer_profile(
    storage: &Storage,
    user: &TestUser,
    profile: &Profile,
) -> Result<ChangeOutcome, ChangeError> {
    let change = RegistrationChange::PeerProfile {
        master_pubkey: &user.master_pubkey,
        profile,
    };
    storage.apply_registration_change(&change, &OPEN).await
}

fn rejection<T: std::fmt::Debug>(result: Result<T, ChangeError>) -> Rejection {
    match result {
        Err(ChangeError::Rejected(rejection)) => rejection,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

async fn count(storage: &Storage, sql: &'static str, author: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(author)
        .fetch_one(&storage.pool)
        .await
        .unwrap()
}

async fn post_count(storage: &Storage, author: &str) -> i64 {
    count(
        storage,
        "SELECT COUNT(*) FROM posts WHERE author = ?1",
        author,
    )
    .await
}

// --- Registration ---

#[tokio::test]
async fn migrations_apply_once() {
    let storage = storage().await;
    Storage::run_migrations(&storage.pool).await.unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_migrations")
        .fetch_one(&storage.pool)
        .await
        .unwrap();
    assert_eq!(applied, Storage::MIGRATIONS.len() as i64);
}

#[tokio::test]
async fn register_creates_then_updates() {
    let storage = storage().await;
    let user = TestUser::new(1);

    let created = register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    assert!(!created.was_active);
    assert!(created.is_active);
    assert_eq!(created.visibility, Visibility::Public);
    assert_eq!(storage.registration_count().await.unwrap(), 1);
    assert_eq!(
        storage
            .get_subscription_target(&user.master_pubkey)
            .await
            .unwrap(),
        Some(Some(user.transport_node_id.clone()))
    );

    let updated = register(&storage, &user, 2_000, Visibility::Listed)
        .await
        .unwrap();
    assert!(updated.was_active);
    assert_eq!(updated.visibility, Visibility::Listed);
    assert_eq!(updated.registered_at, created.registered_at);
    assert_eq!(storage.registration_count().await.unwrap(), 1);
    assert_eq!(
        storage
            .get_subscription_target(&user.master_pubkey)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn replayed_timestamp_is_rejected_on_every_method() {
    let storage = storage().await;
    let user = TestUser::new(1);
    register(&storage, &user, 5_000, Visibility::Public)
        .await
        .unwrap();

    // Equal and lower timestamps are rejected.
    for timestamp in [5_000, 4_999] {
        assert_eq!(
            rejection(register(&storage, &user, timestamp, Visibility::Public).await),
            Rejection::Replayed
        );
        assert_eq!(
            rejection(unregister(&storage, &user, timestamp).await),
            Rejection::Replayed
        );
        let profile = user.profile("name", Visibility::Public, 1);
        assert_eq!(
            rejection(put_profile(&storage, &user, timestamp, profile).await),
            Rejection::Replayed
        );
    }
    assert_eq!(storage.registration_count().await.unwrap(), 1);

    // Each accepted request raises the floor for all three methods.
    let profile = user.profile("name", Visibility::Public, 1);
    put_profile(&storage, &user, 6_000, profile).await.unwrap();
    assert_eq!(
        rejection(register(&storage, &user, 5_500, Visibility::Public).await),
        Rejection::Replayed
    );
    unregister(&storage, &user, 7_000).await.unwrap();

    // The floor is kept while the registration is inactive: a captured registration
    // request cannot register the user again.
    assert_eq!(
        rejection(register(&storage, &user, 6_500, Visibility::Public).await),
        Rejection::Replayed
    );
    assert_eq!(storage.registration_count().await.unwrap(), 0);
    register(&storage, &user, 7_001, Visibility::Public)
        .await
        .unwrap();
}

#[tokio::test]
async fn registration_policy_applies_to_new_registrations_only() {
    let storage = storage().await;
    let first = TestUser::new(1);
    let second = TestUser::new(2);
    register(&storage, &first, 1_000, Visibility::Public)
        .await
        .unwrap();

    let closed = RegistrationPolicy {
        registration_open: false,
        max_registered_users: 100,
    };
    let req = second.registration(URL, 1_000, Visibility::Public, None);
    assert_eq!(
        rejection(
            storage
                .apply_registration_change(&RegistrationChange::Register(&req), &closed)
                .await
        ),
        Rejection::RegistrationClosed
    );
    // An active registration can still be updated.
    let req = first.registration(URL, 2_000, Visibility::Listed, None);
    storage
        .apply_registration_change(&RegistrationChange::Register(&req), &closed)
        .await
        .unwrap();

    let full = RegistrationPolicy {
        registration_open: true,
        max_registered_users: 1,
    };
    let req = second.registration(URL, 1_000, Visibility::Public, None);
    assert_eq!(
        rejection(
            storage
                .apply_registration_change(&RegistrationChange::Register(&req), &full)
                .await
        ),
        Rejection::UserLimitReached
    );
    assert_eq!(storage.registration_count().await.unwrap(), 1);

    // An inactive registration does not count toward the limit.
    unregister(&storage, &first, 3_000).await.unwrap();
    storage
        .apply_registration_change(&RegistrationChange::Register(&req), &full)
        .await
        .unwrap();
    let req = first.registration(URL, 4_000, Visibility::Public, None);
    assert_eq!(
        rejection(
            storage
                .apply_registration_change(&RegistrationChange::Register(&req), &full)
                .await
        ),
        Rejection::UserLimitReached
    );
}

#[tokio::test]
async fn private_registration_is_rejected() {
    let storage = storage().await;
    let user = TestUser::new(1);
    assert_eq!(
        rejection(register(&storage, &user, 1_000, Visibility::Private).await),
        Rejection::PrivateVisibility
    );
    assert_eq!(storage.registration_count().await.unwrap(), 0);
}

#[tokio::test]
async fn unregister_requires_active_registration_and_clears_identifying_columns() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    assert_eq!(
        rejection(unregister(&storage, &user, 1_000).await),
        Rejection::NotRegistered
    );

    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    let profile = user.profile("Alice", Visibility::Public, 10);
    put_profile(&storage, &user, 2_000, profile).await.unwrap();
    storage
        .cache_announcement(&user.announcement(5, &[node_id(50), node_id(51)]))
        .await
        .unwrap();
    let post = user.post("hello", 100);
    let deleted_post = user.post("deleted", 101);
    storage
        .insert_posts(pk, &[post.clone(), deleted_post.clone()], 100)
        .await
        .unwrap();
    storage
        .apply_tombstones(pk, &[&deleted_post.id])
        .await
        .unwrap();
    storage
        .insert_interactions(pk, &[user.like(&post, 1, 100)], 100)
        .await
        .unwrap();

    let outcome = unregister(&storage, &user, 3_000).await.unwrap();
    assert!(outcome.was_active && !outcome.is_active);

    assert_eq!(post_count(&storage, pk).await, 0);
    assert_eq!(
        count(
            &storage,
            "SELECT COUNT(*) FROM interactions WHERE author = ?1",
            pk
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &storage,
            "SELECT COUNT(*) FROM post_tombstones WHERE author = ?1",
            pk
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &storage,
            "SELECT COUNT(*) FROM registrations WHERE pubkey = ?1 AND is_active = 0
               AND display_name IS NULL AND bio IS NULL AND avatar_hash IS NULL
               AND profile_visibility IS NULL AND transport_node_id IS NULL
               AND device_node_ids_json IS NULL",
            pk
        )
        .await,
        1
    );
    assert!(storage.get_user_info(pk).await.unwrap().is_none());
    assert!(storage.get_device_node_ids(pk).await.unwrap().is_none());
    assert!(storage.get_subscription_target(pk).await.unwrap().is_none());
    assert_eq!(
        rejection(unregister(&storage, &user, 4_000).await),
        Rejection::NotRegistered
    );

    // Content of an inactive registration is not stored.
    let stored = storage.insert_posts(pk, &[post], 100).await.unwrap();
    assert_eq!(stored.stored, 0);
}

// --- Profiles and visibility ---

#[tokio::test]
async fn profile_is_replaced_only_if_not_older() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();

    let outcome = put_profile(
        &storage,
        &user,
        2_000,
        user.profile("v10", Visibility::Public, 10),
    )
    .await
    .unwrap();
    assert!(outcome.profile_stored);
    let name = |info: Option<UserInfo>| info.and_then(|u| u.display_name);
    assert_eq!(
        name(storage.get_user_info(pk).await.unwrap()).as_deref(),
        Some("v10")
    );

    // Older: kept. The request itself is accepted and raises the timestamp floor.
    let outcome = put_profile(
        &storage,
        &user,
        3_000,
        user.profile("v9", Visibility::Public, 9),
    )
    .await
    .unwrap();
    assert!(!outcome.profile_stored);
    assert_eq!(
        name(storage.get_user_info(pk).await.unwrap()).as_deref(),
        Some("v10")
    );
    let old = user.profile("v9", Visibility::Listed, 9);
    let outcome = peer_profile(&storage, &user, &old).await.unwrap();
    assert!(!outcome.profile_stored);
    assert_eq!(outcome.visibility, Visibility::Public);

    // Equal and newer: stored.
    let same = user.profile("v10b", Visibility::Public, 10);
    assert!(
        peer_profile(&storage, &user, &same)
            .await
            .unwrap()
            .profile_stored
    );
    let newer = user.profile("v11", Visibility::Public, 11);
    assert!(
        peer_profile(&storage, &user, &newer)
            .await
            .unwrap()
            .profile_stored
    );
    assert_eq!(
        name(storage.get_user_info(pk).await.unwrap()).as_deref(),
        Some("v11")
    );

    // The floor is kept while the registration is inactive.
    unregister(&storage, &user, 4_000).await.unwrap();
    register(&storage, &user, 5_000, Visibility::Public)
        .await
        .unwrap();
    assert!(
        !peer_profile(&storage, &user, &same)
            .await
            .unwrap()
            .profile_stored
    );
    assert!(
        peer_profile(&storage, &user, &newer)
            .await
            .unwrap()
            .profile_stored
    );
}

#[tokio::test]
async fn profile_change_requires_active_registration() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let profile = user.profile("name", Visibility::Public, 1);
    assert_eq!(
        rejection(peer_profile(&storage, &user, &profile).await),
        Rejection::NotRegistered
    );
    assert_eq!(
        rejection(put_profile(&storage, &user, 1_000, profile).await),
        Rejection::NotRegistered
    );
}

#[tokio::test]
async fn listed_profile_deletes_content_and_public_profile_restores_visibility() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    let post = user.post("hello", 100);
    storage
        .insert_posts(pk, std::slice::from_ref(&post), 100)
        .await
        .unwrap();
    assert_eq!(post_count(&storage, pk).await, 1);

    let listed = user.profile("name", Visibility::Listed, 2);
    let outcome = peer_profile(&storage, &user, &listed).await.unwrap();
    assert!(outcome.is_active);
    assert_eq!(outcome.visibility, Visibility::Listed);
    assert_eq!(post_count(&storage, pk).await, 0);
    assert!(storage.get_subscription_target(pk).await.unwrap().is_none());
    assert_eq!(
        storage.insert_posts(pk, &[post], 100).await.unwrap().stored,
        0
    );
    // The user stays in the directory.
    let info = storage.get_user_info(pk).await.unwrap().unwrap();
    assert_eq!(info.visibility, "listed");

    // A later registration request for public visibility does not override the profile.
    let outcome = register(&storage, &user, 2_000, Visibility::Public)
        .await
        .unwrap();
    assert_eq!(outcome.visibility, Visibility::Listed);

    let public = user.profile("name", Visibility::Public, 3);
    let outcome = peer_profile(&storage, &user, &public).await.unwrap();
    assert_eq!(outcome.visibility, Visibility::Public);
    assert!(storage.get_subscription_target(pk).await.unwrap().is_some());
}

#[tokio::test]
async fn public_profile_does_not_override_listed_registration() {
    let storage = storage().await;
    let user = TestUser::new(1);
    register(&storage, &user, 1_000, Visibility::Listed)
        .await
        .unwrap();
    let outcome = put_profile(
        &storage,
        &user,
        2_000,
        user.profile("n", Visibility::Public, 1),
    )
    .await
    .unwrap();
    assert!(outcome.profile_stored);
    assert_eq!(outcome.visibility, Visibility::Listed);
    assert!(
        storage
            .get_subscription_target(&user.master_pubkey)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn private_profile_deactivates_registration() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    storage
        .insert_posts(pk, &[user.post("hello", 100)], 100)
        .await
        .unwrap();

    let private = user.profile("name", Visibility::Private, 2);
    let outcome = peer_profile(&storage, &user, &private).await.unwrap();
    assert!(!outcome.is_active);
    assert_eq!(post_count(&storage, pk).await, 0);
    assert!(storage.get_user_info(pk).await.unwrap().is_none());
    assert_eq!(storage.registration_count().await.unwrap(), 0);

    // Over HTTP the same profile has the same effect.
    register(&storage, &user, 2_000, Visibility::Public)
        .await
        .unwrap();
    let outcome = put_profile(&storage, &user, 3_000, private).await.unwrap();
    assert!(!outcome.is_active);
    assert_eq!(storage.registration_count().await.unwrap(), 0);
}

// --- Device announcements ---

#[tokio::test]
async fn announcement_device_list_is_replaced_only_by_higher_version() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    let transport = user.transport_node_id.clone();

    let v5 = user.announcement(5, &[node_id(50)]);
    assert_eq!(
        rejection(storage.cache_announcement(&v5).await),
        Rejection::NotRegistered
    );

    register(&storage, &user, 1_000, Visibility::Listed)
        .await
        .unwrap();
    assert_eq!(
        storage.get_device_node_ids(pk).await.unwrap(),
        Some(vec![transport.clone()])
    );

    assert_eq!(
        storage.cache_announcement(&v5).await.unwrap(),
        AnnouncementOutcome::Stored
    );
    assert_eq!(
        storage.get_device_node_ids(pk).await.unwrap(),
        Some(vec![transport.clone(), node_id(50)])
    );

    // Equal and lower versions keep the list.
    for version in [5, 4] {
        let other = user.announcement(version, &[node_id(60)]);
        assert_eq!(
            storage.cache_announcement(&other).await.unwrap(),
            AnnouncementOutcome::NotNewer
        );
    }
    // The registering device is listed once even if the announcement names it.
    let v6 = user.announcement(6, &[transport.clone(), node_id(51)]);
    assert_eq!(
        storage.cache_announcement(&v6).await.unwrap(),
        AnnouncementOutcome::Stored
    );
    assert_eq!(
        storage.get_device_node_ids(pk).await.unwrap(),
        Some(vec![transport.clone(), node_id(51)])
    );

    // A list ahead of the clock does not replace a current one, however often it is
    // delivered.
    let future = user.announcement(u64::MAX / 2, &[transport.clone(), node_id(52)]);
    for _ in 0..2 {
        assert_eq!(
            storage.cache_announcement(&future).await.unwrap(),
            AnnouncementOutcome::NotNewer
        );
    }
    assert_eq!(
        storage.get_device_node_ids(pk).await.unwrap(),
        Some(vec![transport.clone(), node_id(51)])
    );

    // A list that removes the registering device clears it.
    let mut v7 = user.announcement(7, &[node_id(51)]);
    v7.removed = vec![transport];
    assert_eq!(
        storage.cache_announcement(&v7).await.unwrap(),
        AnnouncementOutcome::Stored
    );
    assert_eq!(
        storage.get_device_node_ids(pk).await.unwrap(),
        Some(vec![node_id(51)])
    );
}

// --- Content ---

#[tokio::test]
async fn posts_are_stored_only_for_active_public_authors() {
    let storage = storage().await;
    let public = TestUser::new(1);
    let listed = TestUser::new(2);
    let unknown = TestUser::new(3);
    register(&storage, &public, 1_000, Visibility::Public)
        .await
        .unwrap();
    register(&storage, &listed, 1_000, Visibility::Listed)
        .await
        .unwrap();

    let post = public.post("hello", 100);
    let foreign = listed.post("from another author", 100);
    let stored = storage
        .insert_posts(&public.master_pubkey, &[post.clone(), foreign.clone()], 100)
        .await
        .unwrap();
    // The post of the other author in the batch is skipped.
    assert_eq!(
        stored,
        InsertCount {
            stored: 1,
            over_limit: 0
        }
    );
    // A second insert of the same post stores nothing.
    let again = storage
        .insert_posts(&public.master_pubkey, &[post], 100)
        .await
        .unwrap();
    assert_eq!(again.stored, 0);

    let stored = storage
        .insert_posts(&listed.master_pubkey, &[foreign], 100)
        .await
        .unwrap();
    assert_eq!(stored.stored, 0);
    let stored = storage
        .insert_posts(&unknown.master_pubkey, &[unknown.post("x", 100)], 100)
        .await
        .unwrap();
    assert_eq!(stored.stored, 0);
    assert_eq!(storage.total_post_count().await.unwrap(), 1);
}

#[tokio::test]
async fn post_and_interaction_limits_per_user() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();

    let posts: Vec<Post> = (0..5)
        .map(|i| user.post(&format!("post {i}"), 100 + i))
        .collect();
    let stored = storage.insert_posts(pk, &posts, 3).await.unwrap();
    assert_eq!(
        stored,
        InsertCount {
            stored: 3,
            over_limit: 2
        }
    );
    assert_eq!(post_count(&storage, pk).await, 3);
    // The newest posts are kept.
    for (i, post) in posts.iter().enumerate() {
        let kept = storage.get_post(pk, &post.id).await.unwrap().is_some();
        assert_eq!(kept, i >= 2, "post {i}");
    }
    // At the limit, older posts are not stored again.
    let stored = storage.insert_posts(pk, &posts, 3).await.unwrap();
    assert_eq!(stored.stored, 0);
    // A newer post replaces the oldest one.
    let newer = user.post("post 5", 105);
    let stored = storage
        .insert_posts(pk, std::slice::from_ref(&newer), 3)
        .await
        .unwrap();
    assert_eq!(stored.stored, 1);
    assert!(storage.get_post(pk, &posts[2].id).await.unwrap().is_none());
    // At the limit, the cursor requests no post older than the oldest kept one, nor one
    // older than the retention floor.
    let cursor = storage.get_sync_cursor(pk, 0, 3).await.unwrap();
    assert_eq!(cursor.since, 103);
    assert_eq!(
        cursor.post_ids,
        vec![newer.id.clone(), posts[4].id.clone(), posts[3].id.clone()]
    );
    let cursor = storage.get_sync_cursor(pk, 104, 3).await.unwrap();
    assert_eq!(cursor.since, 104);
    assert_eq!(cursor.post_ids, vec![newer.id.clone(), posts[4].id.clone()]);

    let target = posts.first().unwrap();
    let likes: Vec<Interaction> = (0..4).map(|n| user.like(target, n, 100)).collect();
    let stored = storage.insert_interactions(pk, &likes, 2).await.unwrap();
    assert_eq!(
        stored,
        InsertCount {
            stored: 2,
            over_limit: 2
        }
    );
}

#[tokio::test]
async fn deleted_interaction_is_not_stored_again() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    let target = user.post("target", 100);
    storage
        .insert_posts(pk, std::slice::from_ref(&target), 100)
        .await
        .unwrap();
    let like = user.like(&target, 0, 100);
    let other = user.like(&target, 1, 100);
    storage
        .insert_interactions(pk, std::slice::from_ref(&like), 100)
        .await
        .unwrap();

    assert!(storage.delete_interaction(&like.id, pk).await.unwrap());
    // The signed original is sent again, for example by a sync or a gossip relay.
    let stored = storage
        .insert_interactions(pk, &[like.clone(), other], 100)
        .await
        .unwrap();
    assert_eq!(stored.stored, 1);
    assert!(!storage.delete_interaction(&like.id, pk).await.unwrap());
}

#[tokio::test]
async fn interactions_missing_from_the_complete_set_are_deleted() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let other_user = TestUser::new(2);
    let pk = &user.master_pubkey;
    for u in [&user, &other_user] {
        register(&storage, u, 1_000, Visibility::Public)
            .await
            .unwrap();
    }
    let target = user.post("target", 100);
    storage
        .insert_posts(pk, std::slice::from_ref(&target), 100)
        .await
        .unwrap();
    let likes: Vec<Interaction> = (0..3).map(|n| user.like(&target, n, 100)).collect();
    storage.insert_interactions(pk, &likes, 100).await.unwrap();
    let foreign = other_user.like(&target, 0, 100);
    storage
        .insert_interactions(
            &other_user.master_pubkey,
            std::slice::from_ref(&foreign),
            100,
        )
        .await
        .unwrap();

    // The author's set no longer holds the second like.
    let kept = vec![likes[0].id.clone(), likes[2].id.clone()];
    assert_eq!(
        storage.delete_interactions_not_in(pk, &kept).await.unwrap(),
        1
    );
    assert!(!storage.delete_interaction(&likes[1].id, pk).await.unwrap());
    assert!(storage.delete_interaction(&likes[0].id, pk).await.unwrap());
    // Interactions of other authors are kept.
    assert!(
        storage
            .delete_interaction(&foreign.id, &other_user.master_pubkey)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn tombstone_deletes_post_and_blocks_reinsertion() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    let kept = user.post("kept", 100);
    let deleted = user.post("deleted", 101);
    storage
        .insert_posts(pk, &[kept.clone(), deleted.clone()], 100)
        .await
        .unwrap();

    assert_eq!(
        storage.apply_tombstones(pk, &[&deleted.id]).await.unwrap(),
        1
    );
    assert!(storage.get_post(pk, &deleted.id).await.unwrap().is_none());
    assert!(storage.get_post(pk, &kept.id).await.unwrap().is_some());

    let stored = storage
        .insert_posts(pk, std::slice::from_ref(&deleted), 100)
        .await
        .unwrap();
    assert_eq!(stored.stored, 0);
    assert_eq!(post_count(&storage, pk).await, 1);

    // Applying the same tombstone again changes nothing.
    assert_eq!(
        storage.apply_tombstones(pk, &[&deleted.id]).await.unwrap(),
        0
    );
    let cursor = storage.get_sync_cursor(pk, 0, u32::MAX).await.unwrap();
    assert_eq!(cursor.tombstone_count, 1);
    assert_eq!(cursor.post_ids, vec![kept.id.clone()]);

    // A tombstone for a post that was never stored blocks a later insert of it.
    let unseen = user.post("unseen", 102);
    assert_eq!(
        storage.apply_tombstones(pk, &[&unseen.id]).await.unwrap(),
        0
    );
    assert_eq!(
        storage
            .insert_posts(pk, &[unseen], 100)
            .await
            .unwrap()
            .stored,
        0
    );

    // Tombstones of a user without an active public registration are not stored.
    let other = TestUser::new(2);
    assert_eq!(
        storage
            .apply_tombstones(&other.master_pubkey, &[&kept.id])
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        count(
            &storage,
            "SELECT COUNT(*) FROM post_tombstones WHERE author = ?1",
            &other.master_pubkey
        )
        .await,
        0
    );
}

#[tokio::test]
async fn sync_cursor_reports_stored_rows() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();

    let empty = storage.get_sync_cursor(pk, 0, u32::MAX).await.unwrap();
    assert!(empty.post_ids.is_empty());
    assert_eq!(empty.newest_post_timestamp, 0);
    assert_eq!(empty.interaction_count, 0);
    assert_eq!(empty.newest_interaction_timestamp, 0);

    let older = user.post("older", 100);
    let newer = user.post("newer", 200);
    storage
        .insert_posts(pk, &[older.clone(), newer.clone()], 100)
        .await
        .unwrap();
    storage
        .insert_interactions(
            pk,
            &[user.like(&older, 1, 150), user.like(&newer, 2, 250)],
            100,
        )
        .await
        .unwrap();

    let cursor = storage.get_sync_cursor(pk, 0, u32::MAX).await.unwrap();
    assert_eq!(cursor.post_ids, vec![newer.id, older.id]);
    assert_eq!(cursor.newest_post_timestamp, 200);
    assert_eq!(cursor.interaction_count, 2);
    assert_eq!(cursor.newest_interaction_timestamp, 250);
    assert_eq!(cursor.tombstone_count, 0);
}

#[tokio::test]
async fn prune_deletes_old_posts_and_interactions() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    let old = user.post("old", 100);
    let new = user.post("new", 300);
    storage
        .insert_posts(pk, &[old.clone(), new.clone()], 100)
        .await
        .unwrap();
    storage
        .insert_interactions(pk, &[user.like(&new, 1, 150), user.like(&new, 2, 350)], 100)
        .await
        .unwrap();

    assert_eq!(storage.prune_older_than(200).await.unwrap(), (1, 1));
    assert!(storage.get_post(pk, &old.id).await.unwrap().is_none());
    assert!(storage.get_post(pk, &new.id).await.unwrap().is_some());
    assert_eq!(
        storage
            .get_sync_cursor(pk, 0, u32::MAX)
            .await
            .unwrap()
            .interaction_count,
        1
    );
    // The deleted post is also removed from the full-text index.
    let (found, total) = storage.search_posts("\"old\"", 10, 0).await.unwrap();
    assert!(found.is_empty());
    assert_eq!(total, 0);
}

// --- Queries: visibility filters ---

/// Change `visibility` or `is_active` without the content deletion that
/// `apply_registration_change` performs, so the filters of the read queries are the
/// only thing between the stored rows and the caller.
async fn force_registration_state(storage: &Storage, pubkey: &str, visibility: &str, active: bool) {
    sqlx::query("UPDATE registrations SET visibility = ?2, is_active = ?3 WHERE pubkey = ?1")
        .bind(pubkey)
        .bind(visibility)
        .bind(active)
        .execute(&storage.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn read_queries_return_posts_of_active_public_authors_only() {
    let storage = storage().await;
    let author = TestUser::new(1);
    let liker = TestUser::new(2);
    let pk = &author.master_pubkey;
    register(&storage, &author, 1_000, Visibility::Public)
        .await
        .unwrap();
    register(&storage, &liker, 1_000, Visibility::Public)
        .await
        .unwrap();
    let post = author.post("searchable #words", 100);
    storage
        .insert_posts(pk, std::slice::from_ref(&post), 100)
        .await
        .unwrap();
    storage
        .insert_interactions(&liker.master_pubkey, &[liker.like(&post, 1, 100)], 100)
        .await
        .unwrap();
    let query = fts_query("searchable").unwrap();
    let authors = vec![pk.clone()];

    for (visibility, active, visible) in [
        ("public", true, true),
        ("listed", true, false),
        ("public", false, false),
    ] {
        force_registration_state(&storage, pk, visibility, active).await;
        let expected = usize::from(visible);

        assert_eq!(
            storage.get_post(pk, &post.id).await.unwrap().is_some(),
            visible
        );
        assert_eq!(
            storage.get_user_posts(pk, 50, None).await.unwrap().len(),
            expected
        );
        assert_eq!(
            storage.get_feed(50, None, None).await.unwrap().len(),
            expected
        );
        assert_eq!(
            storage
                .get_feed(50, None, Some(&authors))
                .await
                .unwrap()
                .len(),
            expected
        );
        let (found, total) = storage.search_posts(&query, 50, 0).await.unwrap();
        assert_eq!(found.len(), expected);
        assert_eq!(total, expected as i64);
        assert_eq!(storage.total_post_count().await.unwrap(), expected as i64);
        assert_eq!(
            storage
                .get_post_interactions(pk, &post.id, 50)
                .await
                .unwrap()
                .is_some(),
            visible
        );
        let rows: Vec<_> = storage.hashtag_posts_since(0).collect().await;
        assert_eq!(rows.len(), expected);
    }
}

#[tokio::test]
async fn interactions_of_non_public_authors_are_not_returned() {
    let storage = storage().await;
    let author = TestUser::new(1);
    let liker = TestUser::new(2);
    let pk = &author.master_pubkey;
    register(&storage, &author, 1_000, Visibility::Public)
        .await
        .unwrap();
    register(&storage, &liker, 1_000, Visibility::Public)
        .await
        .unwrap();
    let post = author.post("hello", 100);
    storage
        .insert_posts(pk, std::slice::from_ref(&post), 100)
        .await
        .unwrap();
    storage
        .insert_interactions(&liker.master_pubkey, &[liker.like(&post, 1, 100)], 100)
        .await
        .unwrap();

    let (interactions, likes) = storage
        .get_post_interactions(pk, &post.id, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((interactions.len(), likes), (1, 1));

    force_registration_state(&storage, &liker.master_pubkey, "listed", true).await;
    let (interactions, likes) = storage
        .get_post_interactions(pk, &post.id, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((interactions.len(), likes), (0, 0));

    // Through the change function the liker's interactions are deleted.
    force_registration_state(&storage, &liker.master_pubkey, "public", true).await;
    unregister(&storage, &liker, 2_000).await.unwrap();
    let stats = storage
        .get_post_stats_batch(&[(pk.as_str(), post.id.as_str())])
        .await
        .unwrap();
    assert!(stats.is_empty());
}

#[tokio::test]
async fn feed_is_ordered_and_paginated_by_timestamp() {
    let storage = storage().await;
    let a = TestUser::new(1);
    let b = TestUser::new(2);
    register(&storage, &a, 1_000, Visibility::Public)
        .await
        .unwrap();
    register(&storage, &b, 1_000, Visibility::Public)
        .await
        .unwrap();
    storage
        .insert_posts(
            &a.master_pubkey,
            &[a.post("a1", 100), a.post("a3", 300)],
            100,
        )
        .await
        .unwrap();
    storage
        .insert_posts(&b.master_pubkey, &[b.post("b2", 200)], 100)
        .await
        .unwrap();

    let contents =
        |posts: Vec<StoredPost>| -> Vec<String> { posts.into_iter().map(|p| p.content).collect() };
    assert_eq!(
        contents(storage.get_feed(50, None, None).await.unwrap()),
        ["a3", "b2", "a1"]
    );
    assert_eq!(
        contents(storage.get_feed(1, None, None).await.unwrap()),
        ["a3"]
    );
    assert_eq!(
        contents(storage.get_feed(50, Some(300), None).await.unwrap()),
        ["b2", "a1"]
    );
    let only_b = vec![b.master_pubkey.clone()];
    assert_eq!(
        contents(storage.get_feed(50, None, Some(&only_b)).await.unwrap()),
        ["b2"]
    );
    assert!(
        storage
            .get_feed(50, None, Some(&[]))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        contents(
            storage
                .get_user_posts(&a.master_pubkey, 50, Some(300))
                .await
                .unwrap()
        ),
        ["a1"]
    );
}

#[tokio::test]
async fn post_stats_count_distinct_likers_replies_quotes_and_reposts() {
    let storage = storage().await;
    let author = TestUser::new(1);
    let other = TestUser::new(2);
    let apk = &author.master_pubkey;
    let opk = &other.master_pubkey;
    register(&storage, &author, 1_000, Visibility::Public)
        .await
        .unwrap();
    register(&storage, &other, 1_000, Visibility::Public)
        .await
        .unwrap();

    let target = author.post("target", 100);
    let untouched = author.post("untouched", 101);
    storage
        .insert_posts(apk, &[target.clone(), untouched.clone()], 100)
        .await
        .unwrap();
    let replies = [
        other.post_with("reply 1", 110, Some(&target), None),
        other.post_with("reply 2", 111, Some(&target), None),
        other.post_with("quote", 112, None, Some(&target)),
        other.post_with("", 113, None, Some(&target)),
    ];
    storage.insert_posts(opk, &replies, 100).await.unwrap();

    // One author with three Like rows counts as one like.
    let likes: Vec<Interaction> = (0..3).map(|n| other.like(&target, n, 120)).collect();
    storage.insert_interactions(opk, &likes, 100).await.unwrap();
    storage
        .insert_interactions(apk, &[author.like(&target, 9, 121)], 100)
        .await
        .unwrap();

    let stats = storage
        .get_post_stats_batch(&[
            (apk.as_str(), target.id.as_str()),
            (apk.as_str(), untouched.id.as_str()),
        ])
        .await
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(
        stats.get(&(apk.clone(), target.id.clone())),
        Some(&PostStats {
            likes: 2,
            reposts: 1,
            replies: 2,
            quotes: 1,
        })
    );

    let (interactions, like_count) = storage
        .get_post_interactions(apk, &target.id, 50)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(interactions.len(), 4);
    assert_eq!(like_count, 2);
    let (limited, like_count) = storage
        .get_post_interactions(apk, &target.id, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(like_count, 2);
}

#[tokio::test]
async fn post_stats_query_uses_indexes_and_scans_no_table() {
    let storage = storage().await;
    let sql = format!("EXPLAIN QUERY PLAN {POST_STATS_SQL}");
    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind("[]")
        .fetch_all(&storage.pool)
        .await
        .unwrap();
    let details: Vec<String> = plan.into_iter().map(|row| row.3).collect();
    let plan = details.join("\n");
    for index in [
        "idx_posts_reply_to",
        "idx_posts_quote_of",
        "idx_interactions_target",
    ] {
        assert!(plan.contains(index), "plan does not use {index}:\n{plan}");
    }
    // The only scan is over the rows of the json_each input.
    for detail in &details {
        assert!(
            !detail.starts_with("SCAN") || detail.contains("json_each"),
            "unexpected scan:\n{plan}"
        );
    }
}

#[tokio::test]
async fn users_are_listed_and_searched_among_active_registrations() {
    let storage = storage().await;
    let alice = TestUser::new(1);
    let bob = TestUser::new(2);
    let gone = TestUser::new(3);
    for (user, name) in [(&alice, "Alice 100%"), (&bob, "Bob"), (&gone, "Alice gone")] {
        register(&storage, user, 1_000, Visibility::Public)
            .await
            .unwrap();
        put_profile(
            &storage,
            user,
            2_000,
            user.profile(name, Visibility::Public, 1),
        )
        .await
        .unwrap();
    }
    register(&storage, &bob, 3_000, Visibility::Listed)
        .await
        .unwrap();
    unregister(&storage, &gone, 3_000).await.unwrap();
    storage
        .insert_posts(
            &alice.master_pubkey,
            &[alice.post("one", 100), alice.post("two", 200)],
            100,
        )
        .await
        .unwrap();

    let (users, total) = storage.list_users(50, 0).await.unwrap();
    assert_eq!(total, 2);
    assert_eq!(users.len(), 2);
    let info = users
        .iter()
        .find(|u| u.pubkey == alice.master_pubkey)
        .unwrap();
    assert_eq!(info.post_count, 2);
    assert_eq!(info.latest_post_at, Some(200));
    assert_eq!(
        info.transport_node_id.as_deref(),
        Some(alice.transport_node_id.as_str())
    );
    let info = users
        .iter()
        .find(|u| u.pubkey == bob.master_pubkey)
        .unwrap();
    assert_eq!(info.visibility, "listed");
    assert_eq!(info.post_count, 0);
    assert_eq!(info.latest_post_at, None);
    assert_eq!(storage.list_users(1, 1).await.unwrap().0.len(), 1);
    assert!(storage.list_users(50, 2).await.unwrap().0.is_empty());

    let found = storage.search_users("alice", 50).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found.first().map(|u| u.pubkey.as_str()),
        Some(alice.master_pubkey.as_str())
    );
    // `%` and `_` match themselves.
    assert_eq!(storage.search_users("100%", 50).await.unwrap().len(), 1);
    assert!(storage.search_users("%", 50).await.unwrap().len() == 1);
    assert!(storage.search_users("_", 50).await.unwrap().is_empty());

    let names = storage
        .get_display_names(&[
            alice.master_pubkey.as_str(),
            bob.master_pubkey.as_str(),
            gone.master_pubkey.as_str(),
        ])
        .await
        .unwrap();
    assert_eq!(names.len(), 2);
    assert_eq!(
        names.get(&alice.master_pubkey).map(String::as_str),
        Some("Alice 100%")
    );
    assert!(!names.contains_key(&gone.master_pubkey));
    assert!(
        storage
            .get_user_info(&gone.master_pubkey)
            .await
            .unwrap()
            .is_none()
    );
}

// --- Full-text search ---

#[tokio::test]
async fn search_accepts_every_query_built_from_user_input() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    storage
        .insert_posts(
            pk,
            &[
                user.post("learning c++ and #rust today", 100),
                user.post("don't panic", 101),
                user.post("say \"abc\" twice", 102),
                user.post("rust is a fungus", 103),
            ],
            100,
        )
        .await
        .unwrap();

    for (input, expected) in [
        ("c++", 1),
        ("don't", 1),
        ("\"abc", 1),
        ("#rust", 2),
        ("rust fungus", 1),
        ("rust AND fungus", 0),
        ("rust OR", 0),
        ("NEAR(rust fungus)", 0),
        ("ru*", 0),
        ("content:rust", 0),
        ("-rust", 2),
        ("(rust", 2),
        ("rust)", 2),
        ("^rust", 2),
        ("a\u{0}rust", 1),
    ] {
        let query = fts_query(input).unwrap();
        let (posts, total) = storage
            .search_posts(&query, 50, 0)
            .await
            .unwrap_or_else(|e| panic!("query {input:?} failed: {e}"));
        assert_eq!(posts.len(), expected, "query {input:?}");
        assert_eq!(total, expected as i64, "query {input:?}");
    }

    // A phrase without any token matches nothing and is not a syntax error.
    let (posts, total) = storage.search_posts("\"++\"", 50, 0).await.unwrap();
    assert!(posts.is_empty());
    assert_eq!(total, 0);

    // Input passed to MATCH without `fts_query` is a syntax error; the handlers never do this.
    assert!(storage.search_posts("#rust", 50, 0).await.is_err());

    let (page, total) = storage
        .search_posts(&fts_query("rust").unwrap(), 1, 1)
        .await
        .unwrap();
    assert_eq!(total, 2);
    assert_eq!(
        page.first().map(|p| p.content.as_str()),
        Some("learning c++ and #rust today")
    );
}

#[tokio::test]
async fn hashtag_posts_stream_filters_by_time_and_hash_sign() {
    let storage = storage().await;
    let user = TestUser::new(1);
    let pk = &user.master_pubkey;
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    storage
        .insert_posts(
            pk,
            &[
                user.post("#old tag", 100),
                user.post("#new tag", 300),
                user.post("no tag", 301),
            ],
            100,
        )
        .await
        .unwrap();

    let rows: Vec<_> = storage.hashtag_posts_since(200).collect().await;
    assert_eq!(rows.len(), 1);
    let (author, content, timestamp) = rows.into_iter().next().unwrap().unwrap();
    assert_eq!(
        (author.as_str(), content.as_str(), timestamp),
        (pk.as_str(), "#new tag", 300)
    );
}

#[tokio::test]
async fn trending_table_is_replaced() {
    let storage = storage().await;
    let tag = |name: &str, score: f64| TrendingHashtag {
        tag: name.to_string(),
        post_count: 1,
        unique_authors: 1,
        latest_post_at: 5,
        score,
        computed_at: 9,
    };
    storage
        .update_trending(&[tag("a", 1.0), tag("b", 3.0), tag("c", 2.0)])
        .await
        .unwrap();
    let names =
        |tags: Vec<TrendingHashtag>| -> Vec<String> { tags.into_iter().map(|t| t.tag).collect() };
    assert_eq!(
        names(storage.get_trending_hashtags(2).await.unwrap()),
        ["b", "c"]
    );

    storage.update_trending(&[tag("d", 1.0)]).await.unwrap();
    assert_eq!(
        names(storage.get_trending_hashtags(10).await.unwrap()),
        ["d"]
    );
}

// --- Maintenance, migration, file database ---

#[tokio::test]
async fn inactive_registrations_are_pruned_by_deactivation_time() {
    let storage = storage().await;
    let active = TestUser::new(1);
    let gone = TestUser::new(2);
    register(&storage, &active, 1_000, Visibility::Public)
        .await
        .unwrap();
    register(&storage, &gone, 1_000, Visibility::Public)
        .await
        .unwrap();
    unregister(&storage, &gone, 2_000).await.unwrap();
    let rows = "SELECT COUNT(*) FROM registrations WHERE pubkey = ?1";

    // Deactivated after the cutoff: kept.
    assert_eq!(storage.prune_inactive_registrations(0).await.unwrap(), 0);
    assert_eq!(count(&storage, rows, &gone.master_pubkey).await, 1);

    // Deactivated before the cutoff: deleted. Active rows are never deleted.
    let future = to_i64(now_millis()) + 60_000;
    assert_eq!(
        storage.prune_inactive_registrations(future).await.unwrap(),
        1
    );
    assert_eq!(count(&storage, rows, &gone.master_pubkey).await, 0);
    assert_eq!(count(&storage, rows, &active.master_pubkey).await, 1);

    // Without the row, the floors are gone and the old timestamp is accepted again.
    register(&storage, &gone, 1_500, Visibility::Public)
        .await
        .unwrap();
}

#[tokio::test]
async fn version_1_database_is_replaced_by_the_version_2_schema() {
    let options = SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    // A database written by the version 1 server: schema of 001 without the
    // schema_migrations table, with rows in every table.
    sqlx::raw_sql(include_str!("../../migrations/001_initial.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO registrations (pubkey, registered_at, last_seen) VALUES ('a', 1, 1);
         INSERT INTO posts (id, author, content, timestamp, indexed_at) VALUES ('p', 'a', 'old words', 1, 1);
         INSERT INTO interactions (id, author, kind, target_post_id, target_author, timestamp, indexed_at)
            VALUES ('i', 'a', 'Like', 'p', 'a', 1, 1);
         INSERT INTO sync_state (pubkey, last_synced_at) VALUES ('a', 1);
         INSERT INTO peer_delegations (master_pubkey, signing_pubkey, delegation_json, cached_at)
            VALUES ('a', 's', '{}', 1);
         INSERT INTO peer_device_announcements (master_pubkey, announcement_json, cached_at)
            VALUES ('a', '{}', 1);
         INSERT INTO trending_hashtags VALUES ('t', 1, 1, 1, 1.0, 1);",
    )
    .execute(&pool)
    .await
    .unwrap();

    Storage::run_migrations(&pool).await.unwrap();
    let storage = Storage { pool };

    for table in ["registrations", "posts", "interactions", "post_tombstones"] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    let removed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE name IN ('sync_state', 'peer_delegations', 'peer_device_announcements')",
    )
    .fetch_one(&storage.pool)
    .await
    .unwrap();
    assert_eq!(removed, 0);
    assert!(storage.get_trending_hashtags(10).await.unwrap().is_empty());
    let (found, _) = storage.search_posts("\"old\"", 10, 0).await.unwrap();
    assert!(found.is_empty());

    // The new schema is usable.
    let user = TestUser::new(1);
    register(&storage, &user, 1_000, Visibility::Public)
        .await
        .unwrap();
    let stored = storage
        .insert_posts(&user.master_pubkey, &[user.post("new words", 100)], 100)
        .await
        .unwrap();
    assert_eq!(stored.stored, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_database_accepts_concurrent_writers() {
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "proscenium-server-test-db-{:016x}",
        u64::from_le_bytes(suffix)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server.db");

    let storage = std::sync::Arc::new(Storage::open(&path).await.unwrap());
    let users: Vec<std::sync::Arc<TestUser>> = (1..=8)
        .map(|seed| std::sync::Arc::new(TestUser::new(seed)))
        .collect();
    for user in &users {
        register(&storage, user, 1_000, Visibility::Public)
            .await
            .unwrap();
    }

    // Eight writers on a pool of five connections, each with 20 write transactions,
    // while a reader runs feed queries.
    let mut tasks = tokio::task::JoinSet::new();
    for user in &users {
        let storage = storage.clone();
        let user = user.clone();
        tasks.spawn(async move {
            for n in 0..20u64 {
                let post = user.post(&format!("post {n}"), 100 + n);
                let stored = storage
                    .insert_posts(&user.master_pubkey, std::slice::from_ref(&post), 1_000)
                    .await
                    .unwrap();
                assert_eq!(stored.stored, 1);
                let profile = user.profile(&format!("name {n}"), Visibility::Public, n);
                assert!(
                    peer_profile(&storage, &user, &profile)
                        .await
                        .unwrap()
                        .profile_stored
                );
            }
        });
    }
    let reader = storage.clone();
    tasks.spawn(async move {
        for _ in 0..50 {
            reader.get_feed(50, None, None).await.unwrap();
            reader.list_users(50, 0).await.unwrap();
        }
    });
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    assert_eq!(storage.total_post_count().await.unwrap(), 160);

    // The data is in the file: a second handle on the same path reads it.
    drop(storage);
    let reopened = Storage::open(&path).await.unwrap();
    assert_eq!(reopened.total_post_count().await.unwrap(), 160);
    assert_eq!(reopened.registration_count().await.unwrap(), 8);
    drop(reopened);
    std::fs::remove_dir_all(&dir).unwrap();
}
