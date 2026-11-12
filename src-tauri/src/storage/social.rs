use crate::constants::MAX_REMOTE_LIST_ENTRIES;
use crate::error::AppError;
use proscenium_types::{SocialGraphEntry, Visibility, is_valid_pubkey, now_millis};

use super::Storage;

/// Age after which a cached remote list is deleted when another list is stored.
const REMOTE_LIST_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Which list of a remote user a cached row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteListKind {
    Follows,
    Followers,
}

impl RemoteListKind {
    fn as_str(self) -> &'static str {
        match self {
            RemoteListKind::Follows => "follows",
            RemoteListKind::Followers => "followers",
        }
    }
}

/// Cached remote social graph result.
pub struct CachedRemoteSocial {
    pub entries: Vec<SocialGraphEntry>,
    pub hidden: bool,
    pub fetched_at: Option<u64>,
}

impl Storage {
    /// Visibility stored in the profile row of `pubkey`.
    ///
    /// A missing row means `Public`. A stored value that does not parse is an error,
    /// so a caller cannot treat a damaged row as `Public`.
    pub async fn get_visibility(&self, pubkey: &str) -> Result<Visibility, AppError> {
        let result: Option<String> =
            sqlx::query_scalar("SELECT visibility FROM profiles WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_optional(&self.pool)
                .await?;
        match result {
            None => Ok(Visibility::Public),
            Some(value) => value
                .parse()
                .map_err(|e: String| AppError::Other(format!("stored visibility is invalid: {e}"))),
        }
    }

    // ---- Follows (rows with follower = the local user) ----

    pub async fn follow(&self, me: &str, entry: &SocialGraphEntry) -> Result<(), AppError> {
        let now = now_millis() as i64;
        sqlx::query(
            "INSERT INTO social_graph (follower, followee, followed_at, state, last_changed_at)
             VALUES (?1, ?2, ?3, 'active', ?4)
             ON CONFLICT(follower, followee) DO UPDATE SET followed_at=?3, state='active', last_changed_at=?4",
        )
        .bind(me)
        .bind(&entry.pubkey)
        .bind(entry.followed_at as i64)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark the follow of `pubkey` as removed. Returns `true` if an active follow existed.
    pub async fn unfollow(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        let now = now_millis() as i64;
        let result = sqlx::query(
            "UPDATE social_graph SET state='removed', last_changed_at=?3
             WHERE follower=?1 AND followee=?2 AND state='active' AND followed_at IS NOT NULL",
        )
        .bind(me)
        .bind(pubkey)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_follows(&self, me: &str) -> Result<Vec<SocialGraphEntry>, AppError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT followee, followed_at FROM social_graph
             WHERE follower=?1 AND state='active' AND followed_at IS NOT NULL
             ORDER BY followed_at DESC, followee ASC",
        )
        .bind(me)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(pubkey, followed_at)| SocialGraphEntry {
                pubkey,
                followed_at: followed_at as u64,
                first_seen: 0,
                last_seen: 0,
                is_online: false,
            })
            .collect())
    }

    pub async fn is_following(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM social_graph
             WHERE follower=?1 AND followee=?2 AND state='active' AND followed_at IS NOT NULL",
        )
        .bind(me)
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    // ---- Followers (rows with followee = the local user) ----

    /// True if `pubkey` is recorded as a follower of `me`.
    ///
    /// This is presence information for display and for push delivery. It does not grant
    /// access: Listed and Private checks use `is_approved_follower` and `is_mutual`.
    pub async fn is_follower(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM social_graph
             WHERE follower=?1 AND followee=?2 AND first_seen IS NOT NULL",
        )
        .bind(pubkey)
        .bind(me)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Record `pubkey` as an online follower of `me`. The caller has verified the
    /// follower's identity. Returns `true` if no follower row existed before.
    pub async fn upsert_follower(
        &self,
        me: &str,
        pubkey: &str,
        now: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let is_new = Self::upsert_follower_tx(&mut tx, me, pubkey, now).await?;
        tx.commit().await?;
        Ok(is_new)
    }

    /// Delete the row that records `pubkey` as a follower of `me`.
    pub(super) async fn delete_follower_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        me: &str,
        pubkey: &str,
    ) -> Result<(), AppError> {
        sqlx::query("DELETE FROM social_graph WHERE follower=?1 AND followee=?2")
            .bind(pubkey)
            .bind(me)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// `upsert_follower` inside an open write transaction.
    pub(super) async fn upsert_follower_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        me: &str,
        pubkey: &str,
        now: u64,
    ) -> Result<bool, AppError> {
        let existing: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM social_graph
             WHERE follower=?1 AND followee=?2 AND first_seen IS NOT NULL",
        )
        .bind(pubkey)
        .bind(me)
        .fetch_one(&mut **tx)
        .await?;
        sqlx::query(
            "INSERT INTO social_graph (follower, followee, first_seen, last_seen, is_online)
             VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT(follower, followee) DO UPDATE SET
                first_seen = COALESCE(first_seen, ?3), last_seen = ?3, is_online = 1",
        )
        .bind(pubkey)
        .bind(me)
        .bind(now as i64)
        .execute(&mut **tx)
        .await?;
        Ok(!existing)
    }

    pub async fn set_follower_offline(&self, me: &str, pubkey: &str) -> Result<(), AppError> {
        sqlx::query(
            "UPDATE social_graph SET is_online=0
             WHERE follower=?1 AND followee=?2",
        )
        .bind(pubkey)
        .bind(me)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_followers(&self, me: &str) -> Result<Vec<SocialGraphEntry>, AppError> {
        let rows: Vec<(String, i64, i64, i32)> = sqlx::query_as(
            "SELECT follower, first_seen, last_seen, is_online FROM social_graph
             WHERE followee=?1 AND first_seen IS NOT NULL
             ORDER BY last_seen DESC, follower ASC",
        )
        .bind(me)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(pubkey, first_seen, last_seen, is_online)| SocialGraphEntry {
                    pubkey,
                    followed_at: 0,
                    first_seen: first_seen as u64,
                    last_seen: last_seen as u64,
                    is_online: is_online != 0,
                },
            )
            .collect())
    }

    /// True if the local user `me` follows `pubkey` and `pubkey` holds an approved
    /// follow request.
    pub async fn is_mutual(&self, me: &str, pubkey: &str) -> Result<bool, AppError> {
        Ok(self.is_following(me, pubkey).await? && self.is_approved_follower(pubkey).await?)
    }

    /// Master pubkeys that receive the local user's content by direct push.
    ///
    /// Listed: every approved follower. Private: approved followers that `me` follows.
    /// Public: none (content is sent on the gossip topic). Blocked users are excluded.
    pub async fn get_push_recipients(
        &self,
        me: &str,
        visibility: Visibility,
    ) -> Result<Vec<String>, AppError> {
        let sql = match visibility {
            Visibility::Public => return Ok(Vec::new()),
            Visibility::Listed => {
                "SELECT fr.pubkey FROM follow_requests fr
                 WHERE fr.status='approved' AND fr.pubkey != ?1
                   AND fr.pubkey NOT IN
                       (SELECT pubkey FROM moderation WHERE kind='block' AND state='active')
                 ORDER BY fr.pubkey"
            }
            Visibility::Private => {
                "SELECT fr.pubkey FROM follow_requests fr
                 WHERE fr.status='approved' AND fr.pubkey != ?1
                   AND fr.pubkey NOT IN
                       (SELECT pubkey FROM moderation WHERE kind='block' AND state='active')
                   AND fr.pubkey IN
                       (SELECT followee FROM social_graph
                        WHERE follower=?1 AND state='active' AND followed_at IS NOT NULL)
                 ORDER BY fr.pubkey"
            }
        };
        let recipients: Vec<String> = sqlx::query_scalar(sql)
            .bind(me)
            .fetch_all(&self.pool)
            .await?;
        Ok(recipients)
    }

    // ---- Lists fetched from other users (table remote_social_graph) ----

    /// Replace the cached list `kind` of `remote_pubkey` with `entries`.
    ///
    /// Entries whose pubkey is not a canonical key are skipped and at most
    /// `MAX_REMOTE_LIST_ENTRIES` are stored. Nothing is written to `social_graph`.
    pub async fn cache_remote_list(
        &self,
        remote_pubkey: &str,
        kind: RemoteListKind,
        entries: &[SocialGraphEntry],
        hidden: bool,
    ) -> Result<(), AppError> {
        let now = now_millis();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        sqlx::query("DELETE FROM remote_social_graph WHERE owner = ?1 AND kind = ?2")
            .bind(remote_pubkey)
            .bind(kind.as_str())
            .execute(&mut *tx)
            .await?;

        for entry in entries
            .iter()
            .filter(|e| is_valid_pubkey(&e.pubkey))
            .take(MAX_REMOTE_LIST_ENTRIES)
        {
            sqlx::query(
                "INSERT OR IGNORE INTO remote_social_graph
                    (owner, kind, pubkey, followed_at, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(remote_pubkey)
            .bind(kind.as_str())
            .bind(&entry.pubkey)
            .bind(entry.followed_at.min(i64::MAX as u64) as i64)
            .bind(entry.first_seen.min(i64::MAX as u64) as i64)
            .bind(entry.last_seen.min(i64::MAX as u64) as i64)
            .execute(&mut *tx)
            .await?;
        }

        let meta_sql = match kind {
            RemoteListKind::Follows => {
                "INSERT INTO remote_social_meta (pubkey, follows_hidden, follows_fetched_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(pubkey) DO UPDATE SET follows_hidden = ?2, follows_fetched_at = ?3"
            }
            RemoteListKind::Followers => {
                "INSERT INTO remote_social_meta (pubkey, followers_hidden, followers_fetched_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(pubkey) DO UPDATE SET followers_hidden = ?2, followers_fetched_at = ?3"
            }
        };
        sqlx::query(meta_sql)
            .bind(remote_pubkey)
            .bind(hidden)
            .bind(now as i64)
            .execute(&mut *tx)
            .await?;

        // Delete lists of users whose two lists were both fetched before the cutoff.
        let cutoff = now.saturating_sub(REMOTE_LIST_RETENTION_MS) as i64;
        sqlx::query(
            "DELETE FROM remote_social_graph WHERE owner IN
                (SELECT pubkey FROM remote_social_meta
                 WHERE COALESCE(follows_fetched_at, 0) < ?1
                   AND COALESCE(followers_fetched_at, 0) < ?1)",
        )
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM remote_social_meta
             WHERE COALESCE(follows_fetched_at, 0) < ?1
               AND COALESCE(followers_fetched_at, 0) < ?1",
        )
        .bind(cutoff)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// The cached list `kind` of `remote_pubkey`, or `None` if it was never fetched.
    pub async fn get_cached_remote_list(
        &self,
        remote_pubkey: &str,
        kind: RemoteListKind,
    ) -> Result<Option<CachedRemoteSocial>, AppError> {
        let meta_sql = match kind {
            RemoteListKind::Follows => {
                "SELECT follows_hidden, follows_fetched_at FROM remote_social_meta WHERE pubkey = ?1"
            }
            RemoteListKind::Followers => {
                "SELECT followers_hidden, followers_fetched_at FROM remote_social_meta WHERE pubkey = ?1"
            }
        };
        let meta: Option<(bool, Option<i64>)> = sqlx::query_as(meta_sql)
            .bind(remote_pubkey)
            .fetch_optional(&self.pool)
            .await?;

        let Some((hidden, Some(fetched_at))) = meta else {
            return Ok(None);
        };

        let order = match kind {
            RemoteListKind::Follows => {
                "SELECT pubkey, followed_at, first_seen, last_seen FROM remote_social_graph
                 WHERE owner = ?1 AND kind = ?2 ORDER BY followed_at DESC, pubkey ASC"
            }
            RemoteListKind::Followers => {
                "SELECT pubkey, followed_at, first_seen, last_seen FROM remote_social_graph
                 WHERE owner = ?1 AND kind = ?2 ORDER BY last_seen DESC, pubkey ASC"
            }
        };
        let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(order)
            .bind(remote_pubkey)
            .bind(kind.as_str())
            .fetch_all(&self.pool)
            .await?;

        Ok(Some(CachedRemoteSocial {
            entries: rows
                .into_iter()
                .map(
                    |(pubkey, followed_at, first_seen, last_seen)| SocialGraphEntry {
                        pubkey,
                        followed_at: followed_at as u64,
                        first_seen: first_seen as u64,
                        last_seen: last_seen as u64,
                        is_online: false,
                    },
                )
                .collect(),
            hidden,
            fetched_at: Some(fetched_at as u64),
        }))
    }
}
