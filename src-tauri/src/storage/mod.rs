mod crypto;
pub(crate) mod follow_requests;
mod interactions;
mod messaging;
mod moderation;
mod notifications;
mod posts;
mod profiles;
mod push;
pub(crate) mod servers;
mod social;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

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
    db: Mutex<Connection>,
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
            "002_follows",
            include_str!("../../migrations/002_follows.sql"),
        ),
        ("003_posts", include_str!("../../migrations/003_posts.sql")),
        (
            "004_interactions",
            include_str!("../../migrations/004_interactions.sql"),
        ),
        (
            "005_direct_messages",
            include_str!("../../migrations/005_direct_messages.sql"),
        ),
        (
            "006_bookmarks",
            include_str!("../../migrations/006_bookmarks.sql"),
        ),
        (
            "007_moderation",
            include_str!("../../migrations/007_moderation.sql"),
        ),
        (
            "008_mentions",
            include_str!("../../migrations/008_mentions.sql"),
        ),
        (
            "009_notifications",
            include_str!("../../migrations/009_notifications.sql"),
        ),
        (
            "010_visibility",
            include_str!("../../migrations/010_visibility.sql"),
        ),
        (
            "011_push_outbox",
            include_str!("../../migrations/011_push_outbox.sql"),
        ),
        (
            "012_follow_requests",
            include_str!("../../migrations/012_follow_requests.sql"),
        ),
        (
            "013_servers",
            include_str!("../../migrations/013_servers.sql"),
        ),
    ];

    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        Self::run_migrations(&conn)?;
        Ok(Self {
            db: Mutex::new(conn),
        })
    }

    pub(crate) fn with_db<T>(
        &self,
        f: impl FnOnce(&Connection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let db = self.db.lock().unwrap();
        f(&db)
    }

    fn run_migrations(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                name TEXT PRIMARY KEY,
                applied_at INTEGER NOT NULL
            )",
        )?;

        for (name, sql) in Self::MIGRATIONS {
            let already_applied: bool = conn.query_row(
                "SELECT COUNT(*) > 0 FROM schema_migrations WHERE name=?1",
                params![name],
                |row| row.get(0),
            )?;
            if !already_applied {
                println!("[storage] applying migration: {name}");
                conn.execute_batch(sql)?;
                conn.execute(
                    "INSERT INTO schema_migrations (name, applied_at) VALUES (?1, strftime('%s', 'now'))",
                    params![name],
                )?;
            }
        }
        Ok(())
    }
}
