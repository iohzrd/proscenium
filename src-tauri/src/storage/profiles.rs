use crate::error::AppError;
use iroh_social_types::{Profile, Visibility};
use sqlx::Row;

use super::Storage;

impl Storage {
    pub async fn save_profile(&self, pubkey: &str, profile: &Profile) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO profiles (pubkey, display_name, bio, avatar_hash, avatar_ticket, visibility, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(pubkey) DO UPDATE SET display_name=?2, bio=?3, avatar_hash=?4, avatar_ticket=?5, visibility=?6, signature=?7",
        )
        .bind(pubkey)
        .bind(&profile.display_name)
        .bind(&profile.bio)
        .bind(&profile.avatar_hash)
        .bind(&profile.avatar_ticket)
        .bind(profile.visibility.to_string())
        .bind(&profile.signature)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_profile(&self, pubkey: &str) -> Result<Option<Profile>, AppError> {
        let row = sqlx::query(
            "SELECT display_name, bio, avatar_hash, avatar_ticket, visibility, signature FROM profiles WHERE pubkey=?1",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => {
                let vis_str: String = row.get(4);
                let visibility: Visibility = vis_str.parse().unwrap_or_default();
                Ok(Some(Profile {
                    display_name: row.get(0),
                    bio: row.get(1),
                    avatar_hash: row.get(2),
                    avatar_ticket: row.get(3),
                    visibility,
                    signature: row.get(5),
                }))
            }
            None => Ok(None),
        }
    }
}
