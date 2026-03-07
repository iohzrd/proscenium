use rusqlite::params;
use sha2::{Digest, Sha256};

use super::{Notification, Storage};

impl Storage {
    pub fn insert_notification(
        &self,
        kind: &str,
        actor: &str,
        target_post_id: Option<&str>,
        post_id: Option<&str>,
        timestamp: u64,
    ) -> anyhow::Result<()> {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update(actor.as_bytes());
        hasher.update(target_post_id.unwrap_or("").as_bytes());
        hasher.update(post_id.unwrap_or("").as_bytes());
        let id = format!("{:x}", hasher.finalize());
        self.with_db(|db| {
            db.execute(
                "INSERT OR IGNORE INTO notifications (id, kind, actor, target_post_id, post_id, timestamp, read)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                params![id, kind, actor, target_post_id, post_id, timestamp as i64],
            )?;
            Ok(())
        })
    }

    pub fn get_notifications(
        &self,
        limit: usize,
        before: Option<u64>,
    ) -> anyhow::Result<Vec<Notification>> {
        self.with_db(|db| {
            let hidden =
                "AND n.actor NOT IN (SELECT pubkey FROM mutes UNION SELECT pubkey FROM blocks)";
            let mut notifications = Vec::new();
            match before {
                Some(b) => {
                    let sql = format!(
                        "SELECT id, kind, actor, target_post_id, post_id, timestamp, read
                         FROM notifications n
                         WHERE n.timestamp < ?1 {hidden}
                         ORDER BY n.timestamp DESC LIMIT ?2"
                    );
                    let mut stmt = db.prepare(&sql)?;
                    let mut rows = stmt.query(params![b as i64, limit as i64])?;
                    while let Some(row) = rows.next()? {
                        notifications.push(Self::row_to_notification(row)?);
                    }
                }
                None => {
                    let sql = format!(
                        "SELECT id, kind, actor, target_post_id, post_id, timestamp, read
                         FROM notifications n
                         WHERE 1=1 {hidden}
                         ORDER BY n.timestamp DESC LIMIT ?1"
                    );
                    let mut stmt = db.prepare(&sql)?;
                    let mut rows = stmt.query(params![limit as i64])?;
                    while let Some(row) = rows.next()? {
                        notifications.push(Self::row_to_notification(row)?);
                    }
                }
            }
            Ok(notifications)
        })
    }

    fn row_to_notification(row: &rusqlite::Row) -> rusqlite::Result<Notification> {
        let read_int: i32 = row.get(6)?;
        Ok(Notification {
            id: row.get(0)?,
            kind: row.get(1)?,
            actor: row.get(2)?,
            target_post_id: row.get(3)?,
            post_id: row.get(4)?,
            timestamp: row.get::<_, i64>(5)? as u64,
            read: read_int != 0,
        })
    }

    pub fn get_unread_notification_count(&self) -> anyhow::Result<u32> {
        self.with_db(|db| {
            let count: i64 = db.query_row(
                "SELECT COUNT(*) FROM notifications WHERE read=0",
                [],
                |row| row.get(0),
            )?;
            Ok(count as u32)
        })
    }

    pub fn mark_notifications_read(&self) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute("UPDATE notifications SET read=1 WHERE read=0", [])?;
            Ok(())
        })
    }
}
