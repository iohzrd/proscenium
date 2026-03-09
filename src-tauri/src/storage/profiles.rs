use iroh_social_types::{Profile, Visibility};
use rusqlite::params;

use super::Storage;

impl Storage {
    pub fn save_profile(&self, pubkey: &str, profile: &Profile) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "INSERT INTO profiles (pubkey, display_name, bio, avatar_hash, avatar_ticket, visibility, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(pubkey) DO UPDATE SET display_name=?2, bio=?3, avatar_hash=?4, avatar_ticket=?5, visibility=?6, signature=?7",
                params![pubkey, profile.display_name, profile.bio, profile.avatar_hash, profile.avatar_ticket, profile.visibility.to_string(), profile.signature],
            )?;
            Ok(())
        })
    }

    pub fn get_profile(&self, pubkey: &str) -> anyhow::Result<Option<Profile>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT display_name, bio, avatar_hash, avatar_ticket, visibility, signature FROM profiles WHERE pubkey=?1",
            )?;
            let mut rows = stmt.query(params![pubkey])?;
            match rows.next()? {
                Some(row) => {
                    let vis_str: String = row.get(4)?;
                    let visibility: Visibility = vis_str.parse().unwrap_or_default();
                    Ok(Some(Profile {
                        display_name: row.get(0)?,
                        bio: row.get(1)?,
                        avatar_hash: row.get(2)?,
                        avatar_ticket: row.get(3)?,
                        visibility,
                        signature: row.get(5)?,
                    }))
                }
                None => Ok(None),
            }
        })
    }
}
