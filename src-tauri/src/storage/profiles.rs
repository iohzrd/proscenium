use crate::error::AppError;
use proscenium_types::{Profile, Visibility};
use sqlx::Row;

use super::Storage;

impl Storage {
    /// Insert or replace the profile stored under `pubkey`.
    /// An existing row is replaced only if `profile.updated_at` is greater than or equal to
    /// the stored value. Returns `true` if a row was written.
    pub async fn save_profile(&self, pubkey: &str, profile: &Profile) -> Result<bool, AppError> {
        let result = sqlx::query(
            "INSERT INTO profiles (pubkey, display_name, bio, avatar_hash, avatar_ticket, visibility, signature, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(pubkey) DO UPDATE SET display_name=?2, bio=?3, avatar_hash=?4, avatar_ticket=?5, visibility=?6, signature=?7, updated_at=?8
             WHERE excluded.updated_at >= profiles.updated_at",
        )
        .bind(pubkey)
        .bind(&profile.display_name)
        .bind(&profile.bio)
        .bind(&profile.avatar_hash)
        .bind(&profile.avatar_ticket)
        .bind(profile.visibility.to_string())
        .bind(&profile.signature)
        .bind(profile.updated_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_profile(&self, pubkey: &str) -> Result<Option<Profile>, AppError> {
        let row = sqlx::query(
            "SELECT display_name, bio, avatar_hash, avatar_ticket, visibility, signature, updated_at FROM profiles WHERE pubkey=?1",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => {
                // An unknown stored value is an error; it is not read as `Public`.
                let vis_str: String = row.get(4);
                let visibility: Visibility = vis_str.parse().map_err(AppError::Other)?;
                Ok(Some(Profile {
                    display_name: row.get(0),
                    bio: row.get(1),
                    avatar_hash: row.get(2),
                    avatar_ticket: row.get(3),
                    visibility,
                    updated_at: row.get::<i64, _>(6) as u64,
                    signature: row.get(5),
                }))
            }
            None => Ok(None),
        }
    }

    /// Number of follower rows `get_followers(me)` returns, without loading them.
    pub async fn count_followers(&self, me: &str) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM social_graph WHERE followee=?1 AND first_seen IS NOT NULL",
        )
        .bind(me)
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
    }
}
