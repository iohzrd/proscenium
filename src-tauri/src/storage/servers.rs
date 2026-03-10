use crate::storage::Storage;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub url: String,
    pub name: String,
    pub description: String,
    pub node_id: String,
    pub registered_at: Option<i64>,
    pub visibility: String,
    pub added_at: i64,
    pub last_synced_at: Option<i64>,
}

impl Storage {
    pub fn add_server(&self, url: &str) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis() as i64;
        self.with_db(|db| {
            db.execute(
                "INSERT OR IGNORE INTO servers (url, added_at) VALUES (?1, ?2)",
                params![url, now],
            )?;
            Ok(())
        })
    }

    pub fn remove_server(&self, url: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute("DELETE FROM servers WHERE url = ?1", params![url])?;
            Ok(())
        })
    }

    pub fn get_servers(&self) -> anyhow::Result<Vec<ServerEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT url, name, description, node_id, registered_at, visibility, added_at, last_synced_at FROM servers ORDER BY added_at DESC",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ServerEntry {
                        url: row.get(0)?,
                        name: row.get(1)?,
                        description: row.get(2)?,
                        node_id: row.get(3)?,
                        registered_at: row.get(4)?,
                        visibility: row.get(5)?,
                        added_at: row.get(6)?,
                        last_synced_at: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn get_server(&self, url: &str) -> anyhow::Result<Option<ServerEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT url, name, description, node_id, registered_at, visibility, added_at, last_synced_at FROM servers WHERE url = ?1",
            )?;
            let entry = stmt
                .query_row(params![url], |row| {
                    Ok(ServerEntry {
                        url: row.get(0)?,
                        name: row.get(1)?,
                        description: row.get(2)?,
                        node_id: row.get(3)?,
                        registered_at: row.get(4)?,
                        visibility: row.get(5)?,
                        added_at: row.get(6)?,
                        last_synced_at: row.get(7)?,
                    })
                })
                .optional()?;
            Ok(entry)
        })
    }

    pub fn update_server_info(
        &self,
        url: &str,
        name: &str,
        description: &str,
        node_id: &str,
    ) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "UPDATE servers SET name = ?2, description = ?3, node_id = ?4 WHERE url = ?1",
                params![url, name, description, node_id],
            )?;
            Ok(())
        })
    }

    pub fn mark_server_registered(&self, url: &str, visibility: &str) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis() as i64;
        self.with_db(|db| {
            db.execute(
                "UPDATE servers SET registered_at = ?2, visibility = ?3 WHERE url = ?1",
                params![url, now, visibility],
            )?;
            Ok(())
        })
    }

    pub fn get_registered_servers(&self) -> anyhow::Result<Vec<ServerEntry>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT url, name, description, node_id, registered_at, visibility, added_at, last_synced_at FROM servers WHERE registered_at IS NOT NULL ORDER BY added_at DESC",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(ServerEntry {
                        url: row.get(0)?,
                        name: row.get(1)?,
                        description: row.get(2)?,
                        node_id: row.get(3)?,
                        registered_at: row.get(4)?,
                        visibility: row.get(5)?,
                        added_at: row.get(6)?,
                        last_synced_at: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    pub fn mark_server_unregistered(&self, url: &str) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "UPDATE servers SET registered_at = NULL WHERE url = ?1",
                params![url],
            )?;
            Ok(())
        })
    }
}
