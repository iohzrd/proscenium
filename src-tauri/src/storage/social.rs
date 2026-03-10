use iroh_social_types::{FollowEntry, FollowerEntry, Visibility, now_millis};
use rusqlite::params;

use super::Storage;

impl Storage {
    pub fn get_visibility(&self, pubkey: &str) -> anyhow::Result<Visibility> {
        self.with_db(|db| {
            let result: Option<String> = db
                .query_row(
                    "SELECT visibility FROM profiles WHERE pubkey=?1",
                    params![pubkey],
                    |row| row.get(0),
                )
                .ok();
            Ok(result
                .and_then(|s| s.parse().ok())
                .unwrap_or(Visibility::Public))
        })
    }

    pub fn is_follower(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }

    pub fn follow(&self, entry: &FollowEntry) -> anyhow::Result<()> {
        let now = now_millis() as i64;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO follows (pubkey, alias, followed_at, state, last_changed_at) VALUES (?1, ?2, ?3, 'active', ?4)
                 ON CONFLICT(pubkey) DO UPDATE SET alias=?2, followed_at=?3, state='active', last_changed_at=?4",
                params![entry.pubkey, entry.alias, entry.followed_at as i64, now],
            )?;
            Ok(())
        })
    }

    pub fn update_follow_alias(&self, pubkey: &str, alias: Option<&str>) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "UPDATE follows SET alias=?2 WHERE pubkey=?1",
                params![pubkey, alias],
            )?;
            Ok(())
        })
    }

    pub fn unfollow(&self, pubkey: &str) -> anyhow::Result<()> {
        let now = now_millis() as i64;
        self.with_db(|db| {
            db.execute(
                "UPDATE follows SET state='removed', last_changed_at=?2 WHERE pubkey=?1",
                params![pubkey, now],
            )?;
            Ok(())
        })
    }

    pub fn get_follows(&self) -> anyhow::Result<Vec<FollowEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT pubkey, alias, followed_at FROM follows WHERE state='active' ORDER BY followed_at DESC",
            )?;
            let mut rows = stmt.query([])?;
            let mut follows = Vec::new();
            while let Some(row) = rows.next()? {
                follows.push(FollowEntry {
                    pubkey: row.get(0)?,
                    alias: row.get(1)?,
                    followed_at: row.get::<_, i64>(2)? as u64,
                });
            }
            Ok(follows)
        })
    }

    pub fn upsert_follower(&self, pubkey: &str, now: u64) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let existing: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1",
                params![pubkey],
                |row| row.get(0),
            )?;
            db.execute(
                "INSERT INTO followers (pubkey, first_seen, last_seen, is_online)
                 VALUES (?1, ?2, ?2, 1)
                 ON CONFLICT(pubkey) DO UPDATE SET last_seen=?2, is_online=1",
                params![pubkey, now as i64],
            )?;
            Ok(!existing)
        })
    }

    pub fn set_follower_offline(&self, pubkey: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "UPDATE followers SET is_online=0 WHERE pubkey=?1",
                params![pubkey],
            )?;
            Ok(())
        })
    }

    pub fn get_followers(&self) -> anyhow::Result<Vec<FollowerEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT pubkey, first_seen, last_seen, is_online FROM followers ORDER BY last_seen DESC",
            )?;
            let mut rows = stmt.query([])?;
            let mut followers = Vec::new();
            while let Some(row) = rows.next()? {
                followers.push(FollowerEntry {
                    pubkey: row.get(0)?,
                    first_seen: row.get::<_, i64>(1)? as u64,
                    last_seen: row.get::<_, i64>(2)? as u64,
                    is_online: row.get::<_, i32>(3)? != 0,
                });
            }
            Ok(followers)
        })
    }

    pub fn is_following(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM follows WHERE pubkey=?1 AND state='active'",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }

    pub fn is_mutual(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let is_follower: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1",
                params![pubkey],
                |row| row.get(0),
            )?;
            let is_following: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM follows WHERE pubkey=?1 AND state='active'",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(is_follower && is_following)
        })
    }
}
