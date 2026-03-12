mod crypto;
mod device_sync;
pub(crate) mod follow_requests;
mod interactions;
mod linked_devices;
mod messaging;
mod moderation;
mod notifications;
mod peer_delegations;
mod posts;
mod profiles;
mod push;
pub(crate) mod servers;
mod social;

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostCounts {
    pub likes: u32,
    pub replies: u32,
    pub reposts: u32,
    pub liked_by_me: bool,
    pub reposted_by_me: bool,
}

pub struct FeedQuery {
    pub limit: usize,
    pub before: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub actor: String,
    pub target_post_id: Option<String>,
    pub post_id: Option<String>,
    pub timestamp: u64,
    pub read: bool,
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
    ];

    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let url = format!("sqlite:{}?mode=rwc", path.as_ref().display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;

        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;
        sqlx::query("PRAGMA busy_timeout=5000")
            .execute(&pool)
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
                sqlx::query_scalar("SELECT COUNT(*) > 0 FROM schema_migrations WHERE name=?1")
                    .bind(name)
                    .fetch_one(pool)
                    .await?;
            if !already_applied {
                println!("[storage] applying migration: {name}");
                sqlx::raw_sql(sql).execute(pool).await?;
                sqlx::query(
                    "INSERT INTO schema_migrations (name, applied_at) VALUES (?1, strftime('%s', 'now'))",
                )
                .bind(name)
                .execute(pool)
                .await?;
            }
        }
        Ok(())
    }
}
