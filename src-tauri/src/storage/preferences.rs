use super::Storage;
use crate::error::AppError;

impl Storage {
    pub async fn get_preference(&self, key: &str) -> Result<Option<String>, AppError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM preferences WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|(v,)| v))
    }

    pub async fn set_preference(&self, key: &str, value: &str) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO preferences (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_bool_preference(&self, key: &str) -> Result<bool, AppError> {
        Ok(self.get_preference(key).await?.is_some_and(|v| v == "1"))
    }

    pub async fn set_bool_preference(&self, key: &str, value: bool) -> Result<(), AppError> {
        self.set_preference(key, if value { "1" } else { "0" })
            .await
    }
}
