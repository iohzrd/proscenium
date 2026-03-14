use crate::error::AppError;
use crate::storage::Storage;
use iroh_social_types::ServerEntry;
use sqlx::Row;

fn row_to_server(row: &sqlx::sqlite::SqliteRow) -> ServerEntry {
    ServerEntry {
        url: row.get(0),
        name: row.get(1),
        description: row.get(2),
        node_id: row.get(3),
        registered_at: row.get(4),
        visibility: row.get(5),
        added_at: row.get(6),
        last_synced_at: row.get(7),
    }
}

impl Storage {
    pub async fn add_server(&self, url: &str) -> Result<(), AppError> {
        let now = iroh_social_types::now_millis() as i64;
        sqlx::query("INSERT OR IGNORE INTO servers (url, added_at) VALUES (?1, ?2)")
            .bind(url)
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn remove_server(&self, url: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM servers WHERE url = ?1")
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_servers(&self) -> Result<Vec<ServerEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT url, name, description, node_id, registered_at, visibility, added_at, last_synced_at FROM servers ORDER BY added_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_server).collect())
    }

    pub async fn get_server(&self, url: &str) -> Result<Option<ServerEntry>, AppError> {
        let row = sqlx::query(
            "SELECT url, name, description, node_id, registered_at, visibility, added_at, last_synced_at FROM servers WHERE url = ?1",
        )
        .bind(url)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_server))
    }

    pub async fn update_server_info(
        &self,
        url: &str,
        name: &str,
        description: &str,
        node_id: &str,
    ) -> Result<(), AppError> {
        sqlx::query("UPDATE servers SET name = ?2, description = ?3, node_id = ?4 WHERE url = ?1")
            .bind(url)
            .bind(name)
            .bind(description)
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn mark_server_registered(
        &self,
        url: &str,
        visibility: &str,
    ) -> Result<(), AppError> {
        let now = iroh_social_types::now_millis() as i64;
        sqlx::query("UPDATE servers SET registered_at = ?2, visibility = ?3 WHERE url = ?1")
            .bind(url)
            .bind(now)
            .bind(visibility)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_registered_servers(&self) -> Result<Vec<ServerEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT url, name, description, node_id, registered_at, visibility, added_at, last_synced_at FROM servers WHERE registered_at IS NOT NULL ORDER BY added_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_server).collect())
    }

    pub async fn mark_server_unregistered(&self, url: &str) -> Result<(), AppError> {
        sqlx::query("UPDATE servers SET registered_at = NULL WHERE url = ?1")
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
