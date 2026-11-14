use crate::error::AppError;
use crate::storage::Storage;
use proscenium_types::ServerEntry;
use sqlx::Row;

const SERVER_COLUMNS: &str = "url, name, description, node_id, registered_at, visibility, added_at";

fn row_to_server(row: &sqlx::sqlite::SqliteRow) -> ServerEntry {
    ServerEntry {
        url: row.get(0),
        name: row.get(1),
        description: row.get(2),
        node_id: row.get(3),
        registered_at: row.get(4),
        visibility: row.get(5),
        added_at: row.get(6),
    }
}

/// Error for an update that matched no row of `servers`.
fn unknown_server(url: &str) -> AppError {
    AppError::Other(format!("server {url} is not in the server list"))
}

impl Storage {
    /// Insert a server row, or replace the name, description and NodeId of an existing row
    /// with the same URL. The registration state of an existing row is kept.
    pub async fn add_server(
        &self,
        url: &str,
        name: &str,
        description: &str,
        node_id: &str,
    ) -> Result<(), AppError> {
        let now = proscenium_types::now_millis() as i64;
        sqlx::query(
            "INSERT INTO servers (url, name, description, node_id, added_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(url) DO UPDATE SET name = ?2, description = ?3, node_id = ?4",
        )
        .bind(url)
        .bind(name)
        .bind(description)
        .bind(node_id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete the server row. Returns `false` if no row has this URL.
    pub async fn remove_server(&self, url: &str) -> Result<bool, AppError> {
        let result = sqlx::query("DELETE FROM servers WHERE url = ?1")
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_servers(&self) -> Result<Vec<ServerEntry>, AppError> {
        let sql = format!("SELECT {SERVER_COLUMNS} FROM servers ORDER BY added_at DESC, url");
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_to_server).collect())
    }

    pub async fn get_server(&self, url: &str) -> Result<Option<ServerEntry>, AppError> {
        let sql = format!("SELECT {SERVER_COLUMNS} FROM servers WHERE url = ?1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(url)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(row_to_server))
    }

    /// Replace the name, description and NodeId of a server row.
    /// Returns an error if no row has this URL.
    pub async fn update_server_info(
        &self,
        url: &str,
        name: &str,
        description: &str,
        node_id: &str,
    ) -> Result<(), AppError> {
        let result = sqlx::query(
            "UPDATE servers SET name = ?2, description = ?3, node_id = ?4 WHERE url = ?1",
        )
        .bind(url)
        .bind(name)
        .bind(description)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(unknown_server(url));
        }
        Ok(())
    }

    /// Record that the local user is registered with the server under `visibility`.
    /// Returns an error if no row has this URL.
    pub async fn mark_server_registered(
        &self,
        url: &str,
        visibility: &str,
    ) -> Result<(), AppError> {
        let now = proscenium_types::now_millis() as i64;
        let result =
            sqlx::query("UPDATE servers SET registered_at = ?2, visibility = ?3 WHERE url = ?1")
                .bind(url)
                .bind(now)
                .bind(visibility)
                .execute(&self.pool)
                .await?;
        if result.rows_affected() == 0 {
            return Err(unknown_server(url));
        }
        Ok(())
    }

    /// Record that the server `url` accepted the own profile with `updated_at` at `now`.
    pub async fn mark_server_profile_synced(
        &self,
        url: &str,
        updated_at: u64,
        now: u64,
    ) -> Result<(), AppError> {
        sqlx::query(
            "UPDATE servers SET profile_synced_at = MAX(profile_synced_at, ?2),
                 profile_sent_at = ?3
             WHERE url = ?1",
        )
        .bind(url)
        .bind(i64::try_from(updated_at).unwrap_or(i64::MAX))
        .bind(i64::try_from(now).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// URLs of the servers the local user is registered with that have not accepted the
    /// own profile with `updated_at`, or have accepted none since `sent_before`.
    pub async fn get_servers_to_send_profile(
        &self,
        updated_at: u64,
        sent_before: u64,
    ) -> Result<Vec<String>, AppError> {
        let urls = sqlx::query_scalar(
            "SELECT url FROM servers
             WHERE registered_at IS NOT NULL AND (profile_synced_at < ?1 OR profile_sent_at < ?2)
             ORDER BY url",
        )
        .bind(i64::try_from(updated_at).unwrap_or(i64::MAX))
        .bind(i64::try_from(sent_before).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        Ok(urls)
    }

    /// Record that the local user is not registered with the server.
    /// Returns an error if no row has this URL.
    pub async fn mark_server_unregistered(&self, url: &str) -> Result<(), AppError> {
        let result = sqlx::query("UPDATE servers SET registered_at = NULL WHERE url = ?1")
            .bind(url)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(unknown_server(url));
        }
        Ok(())
    }
}
