use futures_lite::Stream;
use proscenium_types::{
    Interaction, InteractionKind, LinkedDevicesAnnouncement, MAX_SYNC_TOMBSTONES, Post, Profile,
    ProfileUpdateRequest, RegistrationRequest, SigningKeyDelegation, Visibility, now_millis,
};
use serde::Serialize;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
};
use sqlx::{Sqlite, Transaction};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

#[cfg(test)]
mod tests;

pub struct Storage {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UserInfo {
    pub pubkey: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_hash: Option<String>,
    pub visibility: String,
    pub registered_at: i64,
    pub post_count: i64,
    pub latest_post_at: Option<i64>,
    pub transport_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StoredPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub media_json: Option<String>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StoredInteraction {
    pub id: String,
    pub author: String,
    pub kind: String,
    pub target_post_id: String,
    pub target_author: String,
    pub timestamp: i64,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostStats {
    /// Number of distinct authors with a `Like` interaction on the post.
    pub likes: i64,
    /// Posts that quote the post with empty content and no media.
    pub reposts: i64,
    pub replies: i64,
    /// Posts that quote the post with content or media.
    pub quotes: i64,
}

impl PostStats {
    pub fn is_empty(&self) -> bool {
        self.likes == 0 && self.reposts == 0 && self.replies == 0 && self.quotes == 0
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TrendingHashtag {
    pub tag: String,
    pub post_count: i64,
    pub unique_authors: i64,
    pub latest_post_at: i64,
    pub score: f64,
    pub computed_at: i64,
}

/// One change to a registration. The caller has verified every signature in it.
#[derive(Debug, Clone, Copy)]
pub enum RegistrationChange<'a> {
    /// `POST /api/v1/register`.
    Register(&'a RegistrationRequest),
    /// `DELETE /api/v1/register`.
    Unregister(&'a RegistrationRequest),
    /// `PUT /api/v1/register`.
    UpdateProfile(&'a ProfileUpdateRequest),
    /// A signed profile received over gossip or sync.
    PeerProfile {
        master_pubkey: &'a str,
        profile: &'a Profile,
    },
}

impl<'a> RegistrationChange<'a> {
    pub fn master_pubkey(&self) -> &'a str {
        match *self {
            Self::Register(req) | Self::Unregister(req) => &req.master_pubkey,
            Self::UpdateProfile(req) => &req.master_pubkey,
            Self::PeerProfile { master_pubkey, .. } => master_pubkey,
        }
    }

    /// `timestamp` and delegation of the signed HTTP request, if the change is one.
    fn signed_request(&self) -> Option<(u64, &'a SigningKeyDelegation)> {
        match *self {
            Self::Register(req) | Self::Unregister(req) => Some((req.timestamp, &req.delegation)),
            Self::UpdateProfile(req) => Some((req.timestamp, &req.delegation)),
            Self::PeerProfile { .. } => None,
        }
    }
}

/// Limits applied when a change creates or reactivates a registration.
#[derive(Debug, Clone, Copy)]
pub struct RegistrationPolicy {
    pub registration_open: bool,
    pub max_registered_users: u32,
}

/// State of a registration after `Storage::apply_registration_change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeOutcome {
    /// Whether the registration was active before the change.
    pub was_active: bool,
    pub is_active: bool,
    /// `Public` or `Listed`. Posts are stored only for `Public`.
    pub visibility: Visibility,
    pub registered_at: i64,
    /// False if the change carried a profile older than the stored one.
    pub profile_stored: bool,
}

/// Why a change was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// `timestamp` is not greater than that of the last accepted request.
    Replayed,
    /// The delegation has a lower `key_index` or `dm_key_index` than an accepted one.
    StaleDelegation,
    /// No active registration exists for the master pubkey.
    NotRegistered,
    RegistrationClosed,
    UserLimitReached,
    /// Private visibility was requested.
    PrivateVisibility,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Replayed => "timestamp is not newer than the last accepted request",
            Self::StaleDelegation => "delegation is older than the stored one",
            Self::NotRegistered => "user not registered",
            Self::RegistrationClosed => "registration is closed",
            Self::UserLimitReached => "registration limit reached",
            Self::PrivateVisibility => "private users cannot register with servers",
        })
    }
}

#[derive(Debug)]
pub enum ChangeError {
    Rejected(Rejection),
    Storage(anyhow::Error),
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(rejection) => rejection.fmt(f),
            Self::Storage(e) => write!(f, "storage error: {e}"),
        }
    }
}

impl std::error::Error for ChangeError {}

impl From<Rejection> for ChangeError {
    fn from(rejection: Rejection) -> Self {
        Self::Rejected(rejection)
    }
}

impl From<sqlx::Error> for ChangeError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.into())
    }
}

impl From<serde_json::Error> for ChangeError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.into())
    }
}

/// Result of offering a device announcement to `Storage::cache_announcement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnouncementOutcome {
    /// The device NodeId list was replaced.
    Stored,
    /// The stored announcement has an equal or higher version; the list was kept.
    NotNewer,
}

/// What the server stores for one author, as sent in a `SyncRequest`.
#[derive(Debug, Clone, Default)]
pub struct SyncCursor {
    /// Ids of all stored posts, newest first.
    pub post_ids: Vec<String>,
    pub newest_post_timestamp: u64,
    pub interaction_count: u64,
    pub newest_interaction_timestamp: u64,
    pub tombstone_count: u64,
}

/// Result of `Storage::insert_posts`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InsertCount {
    /// Rows inserted.
    pub stored: usize,
    /// Items not inserted because the per-user limit was reached.
    pub over_limit: usize,
}

#[derive(sqlx::FromRow)]
struct RegistrationRow {
    registered_at: i64,
    is_active: bool,
    requested_visibility: String,
    profile_visibility: Option<String>,
    visibility: String,
    last_request_timestamp: i64,
    key_index: i64,
    dm_key_index: i64,
    profile_updated_at: i64,
    announcement_version: i64,
}

/// Join that restricts `posts p` to authors with an active public registration.
const PUBLIC_AUTHOR_JOIN: &str =
    "JOIN registrations r ON r.pubkey = p.author AND r.is_active = 1 AND r.visibility = 'public'";

/// Per post `(author, id)` of the JSON array `?1` (`[[author, id], ...]`): number of
/// distinct authors with a `Like`, reposts, replies and quotes. Each count is an index
/// search on `idx_interactions_target`, `idx_posts_quote_of` or `idx_posts_reply_to`.
const POST_STATS_SQL: &str = "SELECT t.author, t.id,
    (SELECT COUNT(DISTINCT i.author) FROM interactions i
      WHERE i.target_author = t.author AND i.target_post_id = t.id AND i.kind = 'Like'),
    (SELECT COUNT(*) FROM posts q
      WHERE q.quote_of_author = t.author AND q.quote_of = t.id
        AND q.content = '' AND q.media_json IS NULL),
    (SELECT COUNT(*) FROM posts c
      WHERE c.reply_to_author = t.author AND c.reply_to = t.id),
    (SELECT COUNT(*) FROM posts q
      WHERE q.quote_of_author = t.author AND q.quote_of = t.id
        AND (q.content != '' OR q.media_json IS NOT NULL))
 FROM (SELECT json_extract(value, '$[0]') AS author, json_extract(value, '$[1]') AS id
       FROM json_each(?1)) t";

fn effective_visibility(requested: Visibility, profile: Option<Visibility>) -> Visibility {
    if requested == Visibility::Public && profile != Some(Visibility::Listed) {
        Visibility::Public
    } else {
        Visibility::Listed
    }
}

fn interaction_kind_str(kind: InteractionKind) -> &'static str {
    match kind {
        InteractionKind::Like => "Like",
    }
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Escape `%`, `_` and `\` for a `LIKE ... ESCAPE '\'` pattern.
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Storage {
    const MIGRATIONS: &'static [(&'static str, &'static str)] = &[
        ("001_initial", include_str!("../migrations/001_initial.sql")),
        (
            "002_protocol_v2",
            include_str!("../migrations/002_protocol_v2.sql"),
        ),
    ];

    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Create an in-memory storage instance for testing.
    #[cfg(test)]
    pub async fn open_in_memory() -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                name TEXT PRIMARY KEY,
                applied_at INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        for (name, sql) in Self::MIGRATIONS {
            let already_applied: bool =
                sqlx::query_scalar("SELECT COUNT(*) > 0 FROM schema_migrations WHERE name = ?1")
                    .bind(name)
                    .fetch_one(pool)
                    .await?;
            if already_applied {
                continue;
            }
            tracing::info!("[storage] applying migration: {name}");
            // One transaction per migration: the statements and the bookkeeping row
            // are applied together or not at all.
            let mut tx = pool.begin().await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(*sql))
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO schema_migrations (name, applied_at) VALUES (?1, strftime('%s', 'now'))",
            )
            .bind(name)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Ok(())
    }

    /// Begin a transaction that holds the write lock from its first statement, so rows
    /// read inside it cannot be changed by another writer before it commits.
    async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
        self.pool.begin_with("BEGIN IMMEDIATE").await
    }

    // --- Registrations ---

    async fn load_registration_row(
        conn: &mut SqliteConnection,
        pubkey: &str,
    ) -> Result<Option<RegistrationRow>, sqlx::Error> {
        sqlx::query_as::<_, RegistrationRow>(
            "SELECT registered_at, is_active, requested_visibility, profile_visibility, visibility,
                    last_request_timestamp, key_index, dm_key_index, profile_updated_at,
                    announcement_version
             FROM registrations WHERE pubkey = ?1",
        )
        .bind(pubkey)
        .fetch_optional(conn)
        .await
    }

    async fn load_active_row(
        conn: &mut SqliteConnection,
        pubkey: &str,
    ) -> Result<RegistrationRow, ChangeError> {
        Self::load_registration_row(conn, pubkey)
            .await?
            .filter(|row| row.is_active)
            .ok_or(ChangeError::Rejected(Rejection::NotRegistered))
    }

    fn check_delegation_not_older(
        row: &RegistrationRow,
        delegation: &SigningKeyDelegation,
    ) -> Result<(), Rejection> {
        if i64::from(delegation.key_index) < row.key_index
            || i64::from(delegation.dm_key_index) < row.dm_key_index
        {
            return Err(Rejection::StaleDelegation);
        }
        Ok(())
    }

    async fn store_delegation(
        conn: &mut SqliteConnection,
        pubkey: &str,
        delegation: &SigningKeyDelegation,
    ) -> Result<(), ChangeError> {
        sqlx::query(
            "UPDATE registrations
             SET signing_pubkey = ?2, delegation_json = ?3, key_index = ?4, dm_key_index = ?5
             WHERE pubkey = ?1",
        )
        .bind(pubkey)
        .bind(&delegation.signing_pubkey)
        .bind(serde_json::to_string(delegation)?)
        .bind(i64::from(delegation.key_index))
        .bind(i64::from(delegation.dm_key_index))
        .execute(conn)
        .await?;
        Ok(())
    }

    /// Delete the posts, interactions and tombstones stored for `pubkey`.
    async fn delete_content(conn: &mut SqliteConnection, pubkey: &str) -> Result<(), sqlx::Error> {
        for sql in [
            "DELETE FROM interactions WHERE author = ?1",
            "DELETE FROM posts WHERE author = ?1",
            "DELETE FROM post_tombstones WHERE author = ?1",
        ] {
            sqlx::query(sql).bind(pubkey).execute(&mut *conn).await?;
        }
        Ok(())
    }

    /// Mark the registration inactive at time `now` and clear profile, delegation and
    /// device columns. The replay and downgrade floors are kept.
    async fn deactivate(
        conn: &mut SqliteConnection,
        pubkey: &str,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE registrations
             SET is_active = 0, last_seen = ?2, profile_visibility = NULL, display_name = NULL,
                 bio = NULL, avatar_hash = NULL, transport_node_id = NULL, signing_pubkey = NULL,
                 delegation_json = NULL, device_node_ids_json = NULL
             WHERE pubkey = ?1",
        )
        .bind(pubkey)
        .bind(now)
        .execute(conn)
        .await?;
        Ok(())
    }

    /// Delete inactive registration rows that were deactivated before `before_ms`.
    /// Their replay and downgrade floors are deleted with them. Returns the number of
    /// deleted rows.
    pub async fn prune_inactive_registrations(&self, before_ms: i64) -> anyhow::Result<u64> {
        let result =
            sqlx::query("DELETE FROM registrations WHERE is_active = 0 AND last_seen < ?1")
                .bind(before_ms)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected())
    }

    /// Apply one registration, unregistration or profile change in one transaction.
    ///
    /// For a signed HTTP request the transaction first rejects a `timestamp` that is not
    /// greater than the last accepted one and a delegation older than the stored one.
    /// A profile is stored only if its `updated_at` is not lower than the stored value;
    /// a profile with private visibility deactivates the registration. Posts,
    /// interactions and tombstones of the user are deleted when the registration stops
    /// being active with public visibility.
    pub async fn apply_registration_change(
        &self,
        change: &RegistrationChange<'_>,
        policy: &RegistrationPolicy,
    ) -> Result<ChangeOutcome, ChangeError> {
        let now = to_i64(now_millis());
        let pubkey = change.master_pubkey();
        let mut tx = self.begin_write().await?;
        let row = Self::load_registration_row(&mut tx, pubkey).await?;

        if let (Some((timestamp, delegation)), Some(row)) = (change.signed_request(), &row) {
            if to_i64(timestamp) <= row.last_request_timestamp {
                return Err(Rejection::Replayed.into());
            }
            Self::check_delegation_not_older(row, delegation)?;
        }

        let was_active = row.as_ref().is_some_and(|r| r.is_active);
        let was_public = was_active && row.as_ref().is_some_and(|r| r.visibility == "public");

        let outcome = match *change {
            RegistrationChange::Register(req) => {
                if req.visibility == Visibility::Private {
                    return Err(Rejection::PrivateVisibility.into());
                }
                if !was_active {
                    if !policy.registration_open {
                        return Err(Rejection::RegistrationClosed.into());
                    }
                    let active: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM registrations WHERE is_active = 1",
                    )
                    .fetch_one(&mut *tx)
                    .await?;
                    if active >= i64::from(policy.max_registered_users) {
                        return Err(Rejection::UserLimitReached.into());
                    }
                }
                let active_row = row.as_ref().filter(|r| r.is_active);
                let profile_visibility = active_row
                    .and_then(|r| r.profile_visibility.as_deref())
                    .and_then(|v| v.parse::<Visibility>().ok());
                let visibility = effective_visibility(req.visibility, profile_visibility);
                let registered_at = active_row.map_or(now, |r| r.registered_at);
                sqlx::query(
                    "INSERT INTO registrations
                        (pubkey, registered_at, last_seen, is_active, requested_visibility,
                         visibility, transport_node_id, last_request_timestamp)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7)
                     ON CONFLICT(pubkey) DO UPDATE SET
                        registered_at = excluded.registered_at,
                        last_seen = excluded.last_seen,
                        is_active = 1,
                        requested_visibility = excluded.requested_visibility,
                        visibility = excluded.visibility,
                        transport_node_id = excluded.transport_node_id,
                        last_request_timestamp = excluded.last_request_timestamp",
                )
                .bind(pubkey)
                .bind(registered_at)
                .bind(now)
                .bind(req.visibility.to_string())
                .bind(visibility.to_string())
                .bind(&req.transport_node_id)
                .bind(to_i64(req.timestamp))
                .execute(&mut *tx)
                .await?;
                Self::store_delegation(&mut tx, pubkey, &req.delegation).await?;
                ChangeOutcome {
                    was_active,
                    is_active: true,
                    visibility,
                    registered_at,
                    profile_stored: true,
                }
            }
            RegistrationChange::Unregister(req) => {
                let row = row
                    .filter(|r| r.is_active)
                    .ok_or(Rejection::NotRegistered)?;
                Self::deactivate(&mut tx, pubkey, now).await?;
                sqlx::query(
                    "UPDATE registrations
                     SET last_request_timestamp = ?2, key_index = ?3, dm_key_index = ?4
                     WHERE pubkey = ?1",
                )
                .bind(pubkey)
                .bind(to_i64(req.timestamp))
                .bind(i64::from(req.delegation.key_index))
                .bind(i64::from(req.delegation.dm_key_index))
                .execute(&mut *tx)
                .await?;
                ChangeOutcome {
                    was_active,
                    is_active: false,
                    visibility: Visibility::Listed,
                    registered_at: row.registered_at,
                    profile_stored: true,
                }
            }
            RegistrationChange::UpdateProfile(req) => {
                let row = row
                    .filter(|r| r.is_active)
                    .ok_or(Rejection::NotRegistered)?;
                sqlx::query(
                    "UPDATE registrations SET last_request_timestamp = ?2 WHERE pubkey = ?1",
                )
                .bind(pubkey)
                .bind(to_i64(req.timestamp))
                .execute(&mut *tx)
                .await?;
                Self::store_delegation(&mut tx, pubkey, &req.delegation).await?;
                Self::apply_profile(&mut tx, pubkey, &row, &req.profile, now).await?
            }
            RegistrationChange::PeerProfile { profile, .. } => {
                let row = row
                    .filter(|r| r.is_active)
                    .ok_or(Rejection::NotRegistered)?;
                Self::apply_profile(&mut tx, pubkey, &row, profile, now).await?
            }
        };

        let is_public = outcome.is_active && outcome.visibility == Visibility::Public;
        if was_public && !is_public {
            Self::delete_content(&mut tx, pubkey).await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Store `profile` for the active registration `row` unless it is older than the
    /// stored profile. A private profile deactivates the registration.
    async fn apply_profile(
        conn: &mut SqliteConnection,
        pubkey: &str,
        row: &RegistrationRow,
        profile: &Profile,
        now: i64,
    ) -> Result<ChangeOutcome, ChangeError> {
        let requested = row
            .requested_visibility
            .parse::<Visibility>()
            .unwrap_or(Visibility::Listed);
        let mut outcome = ChangeOutcome {
            was_active: true,
            is_active: true,
            visibility: if row.visibility == "public" {
                Visibility::Public
            } else {
                Visibility::Listed
            },
            registered_at: row.registered_at,
            profile_stored: false,
        };
        let updated_at = to_i64(profile.updated_at);
        if updated_at < row.profile_updated_at {
            return Ok(outcome);
        }
        outcome.profile_stored = true;

        if profile.visibility == Visibility::Private {
            Self::deactivate(conn, pubkey, now).await?;
            sqlx::query("UPDATE registrations SET profile_updated_at = ?2 WHERE pubkey = ?1")
                .bind(pubkey)
                .bind(updated_at)
                .execute(conn)
                .await?;
            outcome.is_active = false;
            outcome.visibility = Visibility::Listed;
            return Ok(outcome);
        }

        outcome.visibility = effective_visibility(requested, Some(profile.visibility));
        sqlx::query(
            "UPDATE registrations
             SET display_name = ?2, bio = ?3, avatar_hash = ?4, profile_visibility = ?5,
                 visibility = ?6, profile_updated_at = ?7, last_seen = ?8
             WHERE pubkey = ?1",
        )
        .bind(pubkey)
        .bind(&profile.display_name)
        .bind(&profile.bio)
        .bind(&profile.avatar_hash)
        .bind(profile.visibility.to_string())
        .bind(outcome.visibility.to_string())
        .bind(updated_at)
        .bind(now)
        .execute(conn)
        .await?;
        Ok(outcome)
    }

    /// Store a delegation whose master-key signature the caller has verified, for an
    /// active registration. Rejected if it is older than the stored one.
    pub async fn cache_delegation(
        &self,
        delegation: &SigningKeyDelegation,
    ) -> Result<(), ChangeError> {
        let mut tx = self.begin_write().await?;
        let row = Self::load_active_row(&mut tx, &delegation.master_pubkey).await?;
        Self::check_delegation_not_older(&row, delegation)?;
        Self::store_delegation(&mut tx, &delegation.master_pubkey, delegation).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Store a device announcement that passed `verify_linked_devices_announcement`,
    /// for an active registration.
    ///
    /// The embedded delegation is stored under the rule of `cache_delegation`. The
    /// device NodeId list is replaced only if `announcement.version` is greater than
    /// the stored version.
    pub async fn cache_announcement(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<AnnouncementOutcome, ChangeError> {
        let mut tx = self.begin_write().await?;
        let row = Self::load_active_row(&mut tx, &announcement.master_pubkey).await?;
        Self::check_delegation_not_older(&row, &announcement.delegation)?;
        Self::store_delegation(
            &mut tx,
            &announcement.master_pubkey,
            &announcement.delegation,
        )
        .await?;

        let version = to_i64(announcement.version);
        if version <= row.announcement_version {
            tx.commit().await?;
            return Ok(AnnouncementOutcome::NotNewer);
        }
        let node_ids: Vec<&str> = announcement
            .devices
            .iter()
            .map(|d| d.node_id.as_str())
            .collect();
        sqlx::query(
            "UPDATE registrations SET device_node_ids_json = ?2, announcement_version = ?3
             WHERE pubkey = ?1",
        )
        .bind(&announcement.master_pubkey)
        .bind(serde_json::to_string(&node_ids)?)
        .bind(version)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(AnnouncementOutcome::Stored)
    }

    /// The signing key of an active registration, if one is stored.
    pub async fn get_signing_pubkey(&self, pubkey: &str) -> anyhow::Result<Option<String>> {
        let key: Option<Option<String>> = sqlx::query_scalar(
            "SELECT signing_pubkey FROM registrations WHERE pubkey = ?1 AND is_active = 1",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(key.flatten())
    }

    /// The NodeIds known for an active registration: the registering device first, then
    /// the devices of the newest announcement. `None` if no active registration exists.
    pub async fn get_device_node_ids(&self, pubkey: &str) -> anyhow::Result<Option<Vec<String>>> {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT transport_node_id, device_node_ids_json FROM registrations
             WHERE pubkey = ?1 AND is_active = 1",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?;
        let Some((transport_node_id, device_node_ids_json)) = row else {
            return Ok(None);
        };
        let mut node_ids: Vec<String> = transport_node_id.into_iter().collect();
        if let Some(json) = device_node_ids_json {
            for node_id in serde_json::from_str::<Vec<String>>(&json)? {
                if !node_ids.contains(&node_id) {
                    node_ids.push(node_id);
                }
            }
        }
        Ok(Some(node_ids))
    }

    /// `Some(transport_node_id)` if `pubkey` has an active registration with public
    /// visibility (the condition for a gossip subscription), else `None`.
    pub async fn get_subscription_target(
        &self,
        pubkey: &str,
    ) -> anyhow::Result<Option<Option<String>>> {
        let row: Option<Option<String>> = sqlx::query_scalar(
            "SELECT transport_node_id FROM registrations
             WHERE pubkey = ?1 AND is_active = 1 AND visibility = 'public'",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn get_active_public_pubkeys(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT pubkey FROM registrations WHERE is_active = 1 AND visibility = 'public'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn registration_count(&self) -> anyhow::Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM registrations WHERE is_active = 1")
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    // --- Posts ---

    /// True if `author` has an active registration with public visibility.
    async fn is_active_public(
        conn: &mut SqliteConnection,
        author: &str,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM registrations
             WHERE pubkey = ?1 AND is_active = 1 AND visibility = 'public'",
        )
        .bind(author)
        .fetch_one(conn)
        .await
    }

    /// Insert verified posts of `author` in one transaction.
    ///
    /// Nothing is inserted unless `author` has an active public registration. A post is
    /// skipped if its author differs from `author`, a tombstone exists for its id, it is
    /// already stored, or `author` already has `max_posts` posts.
    pub async fn insert_posts(
        &self,
        author: &str,
        posts: &[Post],
        max_posts: u32,
    ) -> anyhow::Result<InsertCount> {
        let mut count = InsertCount::default();
        if posts.is_empty() {
            return Ok(count);
        }
        let now = to_i64(now_millis());
        let mut tx = self.begin_write().await?;
        if !Self::is_active_public(&mut tx, author).await? {
            return Ok(count);
        }
        let mut stored_total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE author = ?1")
                .bind(author)
                .fetch_one(&mut *tx)
                .await?;

        for post in posts.iter().filter(|p| p.author == author) {
            if stored_total >= i64::from(max_posts) {
                count.over_limit += 1;
                continue;
            }
            let media_json = if post.media.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&post.media)?)
            };
            let result = sqlx::query(
                "INSERT OR IGNORE INTO posts
                    (id, author, content, timestamp, media_json, reply_to, reply_to_author,
                     quote_of, quote_of_author, signature, indexed_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
                 WHERE NOT EXISTS (SELECT 1 FROM post_tombstones WHERE author = ?2 AND id = ?1)",
            )
            .bind(&post.id)
            .bind(&post.author)
            .bind(&post.content)
            .bind(to_i64(post.timestamp))
            .bind(&media_json)
            .bind(&post.reply_to)
            .bind(&post.reply_to_author)
            .bind(&post.quote_of)
            .bind(&post.quote_of_author)
            .bind(&post.signature)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() > 0 {
                stored_total += 1;
                count.stored += 1;
            }
        }
        tx.commit().await?;
        Ok(count)
    }

    /// Record that `author` deleted the posts `ids`, and delete those posts, in one
    /// transaction. Returns the number of posts deleted.
    ///
    /// Nothing is changed unless `author` has an active public registration. At most
    /// `MAX_SYNC_TOMBSTONES` tombstones are kept per author; beyond that the post is
    /// still deleted but no tombstone is added.
    pub async fn apply_tombstones(&self, author: &str, ids: &[&str]) -> anyhow::Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let now = to_i64(now_millis());
        let mut tx = self.begin_write().await?;
        if !Self::is_active_public(&mut tx, author).await? {
            return Ok(0);
        }
        let mut tombstones: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM post_tombstones WHERE author = ?1")
                .bind(author)
                .fetch_one(&mut *tx)
                .await?;
        let mut deleted = 0;
        for id in ids {
            if tombstones < MAX_SYNC_TOMBSTONES as i64 {
                let result = sqlx::query(
                    "INSERT OR IGNORE INTO post_tombstones (author, id, deleted_at)
                     VALUES (?1, ?2, ?3)",
                )
                .bind(author)
                .bind(id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                tombstones += to_i64(result.rows_affected());
            }
            deleted += sqlx::query("DELETE FROM posts WHERE author = ?1 AND id = ?2")
                .bind(author)
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        tx.commit().await?;
        Ok(deleted)
    }

    /// Delete posts and interactions with a timestamp below `before_ms`.
    /// Returns the numbers of deleted posts and interactions.
    pub async fn prune_older_than(&self, before_ms: i64) -> anyhow::Result<(u64, u64)> {
        let mut tx = self.begin_write().await?;
        let posts = sqlx::query("DELETE FROM posts WHERE timestamp < ?1")
            .bind(before_ms)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let interactions = sqlx::query("DELETE FROM interactions WHERE timestamp < ?1")
            .bind(before_ms)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok((posts, interactions))
    }

    /// The post `(author, id)`, if its author has an active public registration.
    pub async fn get_post(&self, author: &str, id: &str) -> anyhow::Result<Option<StoredPost>> {
        let sql = format!(
            "SELECT p.* FROM posts p {PUBLIC_AUTHOR_JOIN} WHERE p.author = ?1 AND p.id = ?2"
        );
        let post = sqlx::query_as::<_, StoredPost>(sqlx::AssertSqlSafe(sql))
            .bind(author)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(post)
    }

    /// Posts of `pubkey`, newest first. Empty unless `pubkey` has an active public
    /// registration.
    pub async fn get_user_posts(
        &self,
        pubkey: &str,
        limit: i64,
        before: Option<i64>,
    ) -> anyhow::Result<Vec<StoredPost>> {
        let sql = format!(
            "SELECT p.* FROM posts p {PUBLIC_AUTHOR_JOIN}
             WHERE p.author = ?1 AND p.timestamp < ?2
             ORDER BY p.timestamp DESC, p.id DESC LIMIT ?3"
        );
        let posts = sqlx::query_as::<_, StoredPost>(sqlx::AssertSqlSafe(sql))
            .bind(pubkey)
            .bind(before.unwrap_or(i64::MAX))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(posts)
    }

    /// Posts of authors with an active public registration, newest first.
    /// `authors` restricts the result to those authors.
    pub async fn get_feed(
        &self,
        limit: i64,
        before: Option<i64>,
        authors: Option<&[String]>,
    ) -> anyhow::Result<Vec<StoredPost>> {
        let before = before.unwrap_or(i64::MAX);
        let posts = match authors {
            Some([]) => vec![],
            Some(authors) => {
                let sql = format!(
                    "SELECT p.* FROM posts p {PUBLIC_AUTHOR_JOIN}
                     WHERE p.timestamp < ?1 AND p.author IN (SELECT value FROM json_each(?3))
                     ORDER BY p.timestamp DESC, p.id DESC LIMIT ?2"
                );
                sqlx::query_as::<_, StoredPost>(sqlx::AssertSqlSafe(sql))
                    .bind(before)
                    .bind(limit)
                    .bind(serde_json::to_string(authors)?)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                let sql = format!(
                    "SELECT p.* FROM posts p {PUBLIC_AUTHOR_JOIN}
                     WHERE p.timestamp < ?1
                     ORDER BY p.timestamp DESC, p.id DESC LIMIT ?2"
                );
                sqlx::query_as::<_, StoredPost>(sqlx::AssertSqlSafe(sql))
                    .bind(before)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(posts)
    }

    /// Full-text search. `fts_query` must come from `content::fts_query`.
    pub async fn search_posts(
        &self,
        fts_query: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<(Vec<StoredPost>, i64)> {
        let count_sql = format!(
            "SELECT COUNT(*) FROM posts_fts f JOIN posts p ON f.rowid = p.seq {PUBLIC_AUTHOR_JOIN}
             WHERE f.content MATCH ?1"
        );
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count_sql))
            .bind(fts_query)
            .fetch_one(&self.pool)
            .await?;

        let sql = format!(
            "SELECT p.* FROM posts_fts f JOIN posts p ON f.rowid = p.seq {PUBLIC_AUTHOR_JOIN}
             WHERE f.content MATCH ?1
             ORDER BY p.timestamp DESC, p.id DESC LIMIT ?2 OFFSET ?3"
        );
        let posts = sqlx::query_as::<_, StoredPost>(sqlx::AssertSqlSafe(sql))
            .bind(fts_query)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok((posts, count))
    }

    pub async fn total_post_count(&self) -> anyhow::Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM posts p {PUBLIC_AUTHOR_JOIN}");
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }

    /// Author, content and timestamp of each post with `timestamp >= since_ms` whose
    /// content contains `#` and whose author has an active public registration.
    pub fn hashtag_posts_since(
        &self,
        since_ms: i64,
    ) -> impl Stream<Item = Result<(String, String, i64), sqlx::Error>> + Send + '_ {
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT p.author, p.content, p.timestamp FROM posts p
             JOIN registrations r ON r.pubkey = p.author AND r.is_active = 1 AND r.visibility = 'public'
             WHERE p.timestamp >= ?1 AND instr(p.content, '#') > 0",
        )
        .bind(since_ms)
        .fetch(&self.pool)
    }

    // --- Interactions ---

    /// Insert verified interactions of `author` in one transaction.
    ///
    /// Nothing is inserted unless `author` has an active public registration. An
    /// interaction is skipped if its author differs from `author`, it is already stored,
    /// or `author` already has `max_interactions` interactions.
    pub async fn insert_interactions(
        &self,
        author: &str,
        interactions: &[Interaction],
        max_interactions: u32,
    ) -> anyhow::Result<InsertCount> {
        let mut count = InsertCount::default();
        if interactions.is_empty() {
            return Ok(count);
        }
        let now = to_i64(now_millis());
        let mut tx = self.begin_write().await?;
        if !Self::is_active_public(&mut tx, author).await? {
            return Ok(count);
        }
        let mut stored_total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM interactions WHERE author = ?1")
                .bind(author)
                .fetch_one(&mut *tx)
                .await?;

        for interaction in interactions.iter().filter(|i| i.author == author) {
            if stored_total >= i64::from(max_interactions) {
                count.over_limit += 1;
                continue;
            }
            let result = sqlx::query(
                "INSERT OR IGNORE INTO interactions
                    (id, author, kind, target_post_id, target_author, timestamp, signature, indexed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(&interaction.id)
            .bind(&interaction.author)
            .bind(interaction_kind_str(interaction.kind))
            .bind(&interaction.target_post_id)
            .bind(&interaction.target_author)
            .bind(to_i64(interaction.timestamp))
            .bind(&interaction.signature)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() > 0 {
                stored_total += 1;
                count.stored += 1;
            }
        }
        tx.commit().await?;
        Ok(count)
    }

    pub async fn delete_interaction(&self, id: &str, author: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM interactions WHERE id = ?1 AND author = ?2")
            .bind(id)
            .bind(author)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Interactions on the post `(target_author, target_post_id)`, newest first, and the
    /// number of distinct authors with a `Like` among all of them. `None` if the post is
    /// not stored or its author has no active public registration.
    pub async fn get_post_interactions(
        &self,
        target_author: &str,
        target_post_id: &str,
        limit: i64,
    ) -> anyhow::Result<Option<(Vec<StoredInteraction>, i64)>> {
        if self
            .get_post(target_author, target_post_id)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        let interactions = sqlx::query_as::<_, StoredInteraction>(
            "SELECT i.* FROM interactions i
             JOIN registrations r ON r.pubkey = i.author AND r.is_active = 1 AND r.visibility = 'public'
             WHERE i.target_author = ?1 AND i.target_post_id = ?2
             ORDER BY i.timestamp DESC, i.id DESC LIMIT ?3",
        )
        .bind(target_author)
        .bind(target_post_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let like_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT i.author) FROM interactions i
             JOIN registrations r ON r.pubkey = i.author AND r.is_active = 1 AND r.visibility = 'public'
             WHERE i.target_author = ?1 AND i.target_post_id = ?2 AND i.kind = 'Like'",
        )
        .bind(target_author)
        .bind(target_post_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(Some((interactions, like_count)))
    }

    /// Like, repost, reply and quote counts for a batch of posts, in one query.
    /// Keys are `(author, post_id)`; posts whose counts are all zero are omitted.
    pub async fn get_post_stats_batch(
        &self,
        posts: &[(&str, &str)],
    ) -> anyhow::Result<HashMap<(String, String), PostStats>> {
        let mut stats = HashMap::new();
        if posts.is_empty() {
            return Ok(stats);
        }
        let rows: Vec<(String, String, i64, i64, i64, i64)> = sqlx::query_as(POST_STATS_SQL)
            .bind(serde_json::to_string(posts)?)
            .fetch_all(&self.pool)
            .await?;

        for (author, id, likes, reposts, replies, quotes) in rows {
            let post_stats = PostStats {
                likes,
                reposts,
                replies,
                quotes,
            };
            if !post_stats.is_empty() {
                stats.insert((author, id), post_stats);
            }
        }
        Ok(stats)
    }

    // --- Users ---

    const USER_INFO_SELECT: &'static str =
        "SELECT r.pubkey, r.display_name, r.bio, r.avatar_hash, r.visibility, r.registered_at,
                (SELECT COUNT(*) FROM posts p WHERE p.author = r.pubkey) AS post_count,
                (SELECT MAX(p.timestamp) FROM posts p WHERE p.author = r.pubkey) AS latest_post_at,
                r.transport_node_id
         FROM registrations r WHERE r.is_active = 1";

    /// Active registrations ordered by last activity, and their total number.
    pub async fn list_users(
        &self,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<(Vec<UserInfo>, i64)> {
        let total = self.registration_count().await?;
        let sql = format!(
            "{} ORDER BY r.last_seen DESC, r.pubkey LIMIT ?1 OFFSET ?2",
            Self::USER_INFO_SELECT
        );
        let users = sqlx::query_as::<_, UserInfo>(sqlx::AssertSqlSafe(sql))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;
        Ok((users, total))
    }

    /// Active registrations whose display name contains `query` as a substring.
    pub async fn search_users(&self, query: &str, limit: i64) -> anyhow::Result<Vec<UserInfo>> {
        let pattern = format!("%{}%", escape_like(query));
        let sql = format!(
            "{} AND r.display_name LIKE ?1 ESCAPE '\\'
             ORDER BY r.last_seen DESC, r.pubkey LIMIT ?2",
            Self::USER_INFO_SELECT
        );
        let users = sqlx::query_as::<_, UserInfo>(sqlx::AssertSqlSafe(sql))
            .bind(&pattern)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(users)
    }

    pub async fn get_user_info(&self, pubkey: &str) -> anyhow::Result<Option<UserInfo>> {
        let sql = format!("{} AND r.pubkey = ?1", Self::USER_INFO_SELECT);
        let user = sqlx::query_as::<_, UserInfo>(sqlx::AssertSqlSafe(sql))
            .bind(pubkey)
            .fetch_optional(&self.pool)
            .await?;
        Ok(user)
    }

    /// Display names of the active registrations among `pubkeys`, in one query.
    /// Users without a display name are omitted.
    pub async fn get_display_names(
        &self,
        pubkeys: &[&str],
    ) -> anyhow::Result<HashMap<String, String>> {
        if pubkeys.is_empty() {
            return Ok(HashMap::new());
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT pubkey, display_name FROM registrations
             WHERE is_active = 1 AND display_name IS NOT NULL AND display_name != ''
               AND pubkey IN (SELECT value FROM json_each(?1))",
        )
        .bind(serde_json::to_string(pubkeys)?)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }

    // --- Trending ---

    pub async fn get_trending_hashtags(&self, limit: i64) -> anyhow::Result<Vec<TrendingHashtag>> {
        let tags = sqlx::query_as::<_, TrendingHashtag>(
            "SELECT * FROM trending_hashtags ORDER BY score DESC, tag LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(tags)
    }

    /// Replace the trending table with `tags` in one transaction.
    pub async fn update_trending(&self, tags: &[TrendingHashtag]) -> anyhow::Result<()> {
        let mut tx = self.begin_write().await?;
        sqlx::query("DELETE FROM trending_hashtags")
            .execute(&mut *tx)
            .await?;
        for tag in tags {
            sqlx::query(
                "INSERT INTO trending_hashtags (tag, post_count, unique_authors, latest_post_at, score, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(&tag.tag)
            .bind(tag.post_count)
            .bind(tag.unique_authors)
            .bind(tag.latest_post_at)
            .bind(tag.score)
            .bind(tag.computed_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    // --- Sync ---

    /// The values a `SyncRequest` for `author` carries, read in one transaction.
    pub async fn get_sync_cursor(&self, author: &str) -> anyhow::Result<SyncCursor> {
        let mut tx = self.pool.begin().await?;
        let posts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT id, timestamp FROM posts WHERE author = ?1 ORDER BY timestamp DESC, id DESC",
        )
        .bind(author)
        .fetch_all(&mut *tx)
        .await?;
        let (interaction_count, newest_interaction): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(timestamp), 0) FROM interactions WHERE author = ?1",
        )
        .bind(author)
        .fetch_one(&mut *tx)
        .await?;
        let tombstone_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM post_tombstones WHERE author = ?1")
                .bind(author)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;

        Ok(SyncCursor {
            newest_post_timestamp: posts.first().map_or(0, |(_, ts)| to_u64(*ts)),
            post_ids: posts.into_iter().map(|(id, _)| id).collect(),
            interaction_count: to_u64(interaction_count),
            newest_interaction_timestamp: to_u64(newest_interaction),
            tombstone_count: to_u64(tombstone_count),
        })
    }
}
