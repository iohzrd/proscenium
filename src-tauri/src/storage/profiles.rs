use iroh_social_types::Profile;
use rusqlite::params;

use super::Storage;

impl Storage {
    pub fn save_profile(&self, pubkey: &str, profile: &Profile) -> anyhow::Result<()> {
        self.with_db(|db| {
            db.execute(
                "INSERT INTO profiles (pubkey, display_name, bio, avatar_hash, avatar_ticket, is_private)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(pubkey) DO UPDATE SET display_name=?2, bio=?3, avatar_hash=?4, avatar_ticket=?5, is_private=?6",
                params![pubkey, profile.display_name, profile.bio, profile.avatar_hash, profile.avatar_ticket, profile.is_private as i32],
            )?;
            Ok(())
        })
    }

    pub fn get_profile(&self, pubkey: &str) -> anyhow::Result<Option<Profile>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT display_name, bio, avatar_hash, avatar_ticket, is_private FROM profiles WHERE pubkey=?1",
            )?;
            let mut rows = stmt.query(params![pubkey])?;
            match rows.next()? {
                Some(row) => Ok(Some(Profile {
                    display_name: row.get(0)?,
                    bio: row.get(1)?,
                    avatar_hash: row.get(2)?,
                    avatar_ticket: row.get(3)?,
                    is_private: row.get::<_, i32>(4)? != 0,
                })),
                None => Ok(None),
            }
        })
    }
}
