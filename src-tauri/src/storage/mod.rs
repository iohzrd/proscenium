use crate::error::AppError;
mod device_sync;
mod follow_requests;
mod interactions;
mod linked_devices;
mod messaging;
mod moderation;
mod notifications;
mod peer_identities;
mod posts;
mod preferences;
mod profiles;
mod servers;
mod social;
mod sync;
#[cfg(test)]
mod tests;

pub use follow_requests::{DirectApproval, FollowRequestOutcome};
pub use peer_identities::AnnouncementOutcome;
pub use social::RemoteListKind;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::time::Duration;
pub use sync::TombstoneOutcome;

/// Largest number of rows one paged query returns.
pub const MAX_PAGE_SIZE: usize = 200;

/// Keyset pagination position: `(timestamp, id)` of the last row of the previous page.
pub type Cursor<'a> = (u64, &'a str);

/// `limit` capped at `MAX_PAGE_SIZE`, as a value for a SQL `LIMIT` parameter.
/// SQLite treats a negative `LIMIT` as no limit, so the cap also keeps the value non-negative.
fn page_limit(limit: usize) -> i64 {
    limit.min(MAX_PAGE_SIZE) as i64
}

/// `offset` as a value for a SQL `OFFSET` parameter.
fn page_offset(offset: usize) -> i64 {
    i64::try_from(offset).unwrap_or(i64::MAX)
}

/// A `u64` timestamp as a SQL integer parameter. Values above `i64::MAX` become `i64::MAX`.
fn sql_timestamp(timestamp: u64) -> i64 {
    i64::try_from(timestamp).unwrap_or(i64::MAX)
}

pub struct Storage {
    pool: SqlitePool,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage").finish()
    }
}

impl Storage {
    const MIGRATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "001_profiles",
            include_str!("../../migrations/001_profiles.sql"),
        ),
        (
            "002_peer_delegations",
            include_str!("../../migrations/002_peer_delegations.sql"),
        ),
        (
            "003_linked_devices",
            include_str!("../../migrations/003_linked_devices.sql"),
        ),
        (
            "004_social_graph",
            include_str!("../../migrations/004_social_graph.sql"),
        ),
        (
            "005_content",
            include_str!("../../migrations/005_content.sql"),
        ),
        (
            "006_notifications",
            include_str!("../../migrations/006_notifications.sql"),
        ),
        (
            "007_direct_messages",
            include_str!("../../migrations/007_direct_messages.sql"),
        ),
        (
            "008_bookmarks",
            include_str!("../../migrations/008_bookmarks.sql"),
        ),
        (
            "009_servers",
            include_str!("../../migrations/009_servers.sql"),
        ),
        (
            "010_push_outbox",
            include_str!("../../migrations/010_push_outbox.sql"),
        ),
        (
            "011_preferences",
            include_str!("../../migrations/011_preferences.sql"),
        ),
        (
            "012_profile_version",
            include_str!("../../migrations/012_profile_version.sql"),
        ),
        (
            "013_messaging",
            include_str!("../../migrations/013_messaging.sql"),
        ),
        (
            "014_social",
            include_str!("../../migrations/014_social.sql"),
        ),
        (
            "015_content",
            include_str!("../../migrations/015_content.sql"),
        ),
        (
            "016_devices",
            include_str!("../../migrations/016_devices.sql"),
        ),
        (
            "017_master_key_signing",
            include_str!("../../migrations/017_master_key_signing.sql"),
        ),
        (
            "018_random_transport_keys",
            include_str!("../../migrations/018_random_transport_keys.sql"),
        ),
        (
            "019_server_profile_sync",
            include_str!("../../migrations/019_server_profile_sync.sql"),
        ),
        (
            "020_follow_request_decisions",
            include_str!("../../migrations/020_follow_request_decisions.sql"),
        ),
        (
            "021_dm_messages_time_index",
            include_str!("../../migrations/021_dm_messages_time_index.sql"),
        ),
    ];

    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AppError> {
        // `secure_delete` overwrites the content of deleted rows with zeros.
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .pragma("secure_delete", "ON");

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;

        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Create an in-memory storage instance for testing.
    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self, AppError> {
        use std::str::FromStr;
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .pragma("secure_delete", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Delete every row of every table except `schema_migrations`, in one transaction, then
    /// rebuild the database file (`VACUUM`) and copy the write-ahead log into it and
    /// truncate the log (`wal_checkpoint(TRUNCATE)`), so neither file keeps pages of the
    /// deleted rows.
    pub async fn wipe_all_data(&self) -> Result<(), AppError> {
        // Read before the transaction starts: its first statement must be a write, because
        // a transaction that starts with a read fails with SQLITE_BUSY when it needs the
        // write lock after another connection has committed.
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_schema
             WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name != 'schema_migrations'",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut tx = self.pool.begin().await?;
        // Foreign keys are checked at commit, when every table is empty, so the order of
        // the deletes does not matter.
        sqlx::query("PRAGMA defer_foreign_keys = ON")
            .execute(&mut *tx)
            .await?;
        for table in tables {
            // Table names come from `sqlite_schema`, which only migrations write.
            let sql = format!("DELETE FROM \"{}\"", table.replace('"', "\"\""));
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

        sqlx::query("VACUUM").execute(&self.pool).await?;
        // Result row: (busy, WAL frames, frames checkpointed). `busy` is 1 if another
        // connection prevented the checkpoint from completing.
        let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&self.pool)
            .await?;
        if busy != 0 {
            log::warn!("[storage] the write-ahead log was not truncated after the wipe");
        }
        Ok(())
    }

    async fn run_migrations(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                name TEXT PRIMARY KEY,
                -- Unix ms
                applied_at INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        for (name, sql) in Self::MIGRATIONS {
            let already_applied: bool =
                sqlx::query_scalar("SELECT COUNT(*) > 0 FROM schema_migrations WHERE name=?1")
                    .bind(name)
                    .fetch_one(pool)
                    .await?;
            if !already_applied {
                log::info!("[storage] applying migration: {name}");
                // One transaction per migration: the statements and the bookkeeping row
                // are applied together or not at all.
                let mut tx = pool.begin().await?;
                sqlx::raw_sql(sqlx::AssertSqlSafe(*sql))
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO schema_migrations (name, applied_at) VALUES (?1, ?2)")
                    .bind(name)
                    .bind(proscenium_types::now_millis() as i64)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
            }
        }
        Ok(())
    }
}
