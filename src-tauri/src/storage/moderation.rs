use iroh_social_types::now_millis;
use rusqlite::params;

use super::Storage;

impl Storage {
    pub fn toggle_bookmark(&self, post_id: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id=?1",
                params![post_id],
                |row| row.get(0),
            )?;
            if exists {
                db.execute("DELETE FROM bookmarks WHERE post_id=?1", params![post_id])?;
                Ok(false)
            } else {
                let now = now_millis() as i64;
                db.execute(
                    "INSERT INTO bookmarks (post_id, created_at) VALUES (?1, ?2)",
                    params![post_id, now],
                )?;
                Ok(true)
            }
        })
    }

    pub fn is_bookmarked(&self, post_id: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM bookmarks WHERE post_id=?1",
                params![post_id],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }

    pub fn mute_user(&self, pubkey: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            let now = now_millis() as i64;
            db.execute(
                "INSERT OR IGNORE INTO mutes (pubkey, created_at) VALUES (?1, ?2)",
                params![pubkey, now],
            )?;
            Ok(())
        })
    }

    pub fn unmute_user(&self, pubkey: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute("DELETE FROM mutes WHERE pubkey=?1", params![pubkey])?;
            Ok(())
        })
    }

    pub fn is_muted(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM mutes WHERE pubkey=?1",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }

    pub fn get_muted_pubkeys(&self) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare("SELECT pubkey FROM mutes ORDER BY created_at DESC")?;
            let mut rows = stmt.query([])?;
            let mut keys = Vec::new();
            while let Some(row) = rows.next()? {
                keys.push(row.get(0)?);
            }
            Ok(keys)
        })
    }

    pub fn block_user(&self, pubkey: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            let now = now_millis() as i64;
            db.execute(
                "INSERT OR IGNORE INTO blocks (pubkey, created_at) VALUES (?1, ?2)",
                params![pubkey, now],
            )?;
            Ok(())
        })
    }

    pub fn unblock_user(&self, pubkey: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute("DELETE FROM blocks WHERE pubkey=?1", params![pubkey])?;
            Ok(())
        })
    }

    pub fn is_blocked(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM blocks WHERE pubkey=?1",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }

    pub fn get_blocked_pubkeys(&self) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare("SELECT pubkey FROM blocks ORDER BY created_at DESC")?;
            let mut rows = stmt.query([])?;
            let mut keys = Vec::new();
            while let Some(row) = rows.next()? {
                keys.push(row.get(0)?);
            }
            Ok(keys)
        })
    }

    pub fn is_hidden(&self, pubkey: &str) -> anyhow::Result<bool> {
        self.with_db(|db| {
            let exists: bool = db.query_row(
                "SELECT COUNT(*) > 0 FROM mutes WHERE pubkey=?1
                 UNION ALL
                 SELECT COUNT(*) > 0 FROM blocks WHERE pubkey=?1
                 LIMIT 1",
                params![pubkey],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
    }
}
