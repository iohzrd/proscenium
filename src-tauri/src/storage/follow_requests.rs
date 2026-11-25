use crate::constants::MAX_PENDING_FOLLOW_REQUESTS;
use crate::error::AppError;
use proscenium_types::FollowRequestEntry;
use sqlx::Row;

use super::Storage;

/// State of a requester's row after `record_follow_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowRequestOutcome {
    /// A pending row was inserted by this call.
    NewPending,
    /// A pending row already existed; it was left unchanged.
    StillPending,
    /// The requester is an approved follower.
    Approved,
    /// The local user denied this requester and the denial has not expired.
    Denied,
}

/// Result of `approve_follower_directly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectApproval {
    /// The requester became an approved follower with this call.
    New,
    /// The requester was an approved follower already.
    Repeated,
    /// The local user denied this requester and the denial has not expired. Nothing was
    /// written.
    Denied,
}

/// Delete the pending rows other than the newest `MAX_PENDING_FOLLOW_REQUESTS`, by
/// `created_at` and then `pubkey`: the rows `record_follow_request` evicts first. The
/// cached identities of the evicted requesters are deleted unless something else refers
/// to them.
async fn evict_excess_pending_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), AppError> {
    let evicted: Vec<String> = sqlx::query_scalar(
        "DELETE FROM follow_requests WHERE status='pending' AND pubkey IN (
             SELECT pubkey FROM follow_requests WHERE status='pending'
             ORDER BY created_at DESC, pubkey DESC LIMIT -1 OFFSET ?1
         ) RETURNING pubkey",
    )
    .bind(i64::try_from(MAX_PENDING_FOLLOW_REQUESTS).unwrap_or(i64::MAX))
    .fetch_all(&mut **tx)
    .await?;
    super::peer_identities::delete_unreferenced_identities_tx(tx, &evicted).await
}

impl Storage {
    /// Record a verified follow request from `pubkey` and return the resulting row state.
    ///
    /// Runs in one write transaction. Pending and denied rows whose `expires_at` is not
    /// after `now` are deleted first. An approved or denied row is never changed, so a
    /// denied requester stays denied until the denial expires. With `MAX_PENDING_FOLLOW_REQUESTS` pending rows, the oldest
    /// pending row is deleted before the new one is inserted, so requests from generated
    /// identities cannot block new requests. The cached identities of deleted rows are
    /// deleted unless something else refers to them.
    pub async fn record_follow_request(
        &self,
        pubkey: &str,
        timestamp: u64,
        now: u64,
        expires_at: u64,
    ) -> Result<FollowRequestOutcome, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        let expired: Vec<String> = sqlx::query_scalar(
            "DELETE FROM follow_requests WHERE status != 'approved' AND expires_at <= ?1
             RETURNING pubkey",
        )
        .bind(now as i64)
        .fetch_all(&mut *tx)
        .await?;
        super::peer_identities::delete_unreferenced_identities_tx(&mut tx, &expired).await?;

        let existing: Option<String> =
            sqlx::query_scalar("SELECT status FROM follow_requests WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_optional(&mut *tx)
                .await?;
        let outcome = match existing.as_deref() {
            Some("approved") => FollowRequestOutcome::Approved,
            Some("denied") => FollowRequestOutcome::Denied,
            Some(_) => FollowRequestOutcome::StillPending,
            None => {
                let pending: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM follow_requests WHERE status='pending'",
                )
                .fetch_one(&mut *tx)
                .await?;
                if pending as u64 >= MAX_PENDING_FOLLOW_REQUESTS {
                    let evicted: Vec<String> = sqlx::query_scalar(
                        "DELETE FROM follow_requests WHERE pubkey = (
                             SELECT pubkey FROM follow_requests WHERE status='pending'
                             ORDER BY created_at ASC, pubkey ASC LIMIT 1
                         ) RETURNING pubkey",
                    )
                    .fetch_all(&mut *tx)
                    .await?;
                    super::peer_identities::delete_unreferenced_identities_tx(&mut tx, &evicted)
                        .await?;
                }
                sqlx::query(
                    "INSERT INTO follow_requests (pubkey, timestamp, status, created_at, expires_at)
                     VALUES (?1, ?2, 'pending', ?3, ?4)",
                )
                .bind(pubkey)
                .bind(timestamp as i64)
                .bind(now as i64)
                .bind(expires_at as i64)
                .execute(&mut *tx)
                .await?;
                FollowRequestOutcome::NewPending
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    /// Approve the pending, unexpired request of `pubkey` and record `pubkey` as a follower
    /// of `me`, in one transaction. Returns `false` if no such pending request exists.
    pub async fn approve_follow_request(
        &self,
        me: &str,
        pubkey: &str,
        now: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let result = sqlx::query(
            "UPDATE follow_requests SET status='approved', decided_at=?2
             WHERE pubkey=?1 AND status='pending' AND expires_at > ?2",
        )
        .bind(pubkey)
        .bind(now as i64)
        .execute(&mut *tx)
        .await?;
        let approved = result.rows_affected() > 0;
        if approved {
            Self::upsert_follower_tx(&mut tx, me, pubkey, now).await?;
        }
        tx.commit().await?;
        Ok(approved)
    }

    /// Approve every pending, unexpired request of a user that `me` follows, and record
    /// each such user as a follower of `me`, in one transaction. Returns their pubkeys.
    ///
    /// For a Private user, following a requester approves its request (`follow_user`).
    /// Device sync merges follows and requests without that rule, so a request stored on
    /// one device and a follow made on another are approved by this call afterwards.
    pub async fn approve_pending_requests_of_followed(
        &self,
        me: &str,
        now: u64,
    ) -> Result<Vec<String>, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let approved: Vec<String> = sqlx::query_scalar(
            "UPDATE follow_requests SET status='approved', decided_at=?2
             WHERE status='pending' AND expires_at > ?2
               AND pubkey IN (SELECT followee FROM social_graph
                   WHERE follower=?1 AND state='active' AND followed_at IS NOT NULL)
             RETURNING pubkey",
        )
        .bind(me)
        .bind(now as i64)
        .fetch_all(&mut *tx)
        .await?;
        for pubkey in &approved {
            Self::upsert_follower_tx(&mut tx, me, pubkey, now).await?;
        }
        tx.commit().await?;
        Ok(approved)
    }

    /// Store `pubkey` as an approved follower of `me` without a pending step, in one
    /// transaction. Used when a Private user receives a request from a user they follow.
    ///
    /// Writes nothing and returns `Denied` if an unexpired denied row exists for `pubkey`.
    pub async fn approve_follower_directly(
        &self,
        me: &str,
        pubkey: &str,
        timestamp: u64,
        now: u64,
        expires_at: u64,
    ) -> Result<DirectApproval, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(String, i64)> =
            sqlx::query_as("SELECT status, expires_at FROM follow_requests WHERE pubkey=?1")
                .bind(pubkey)
                .fetch_optional(&mut *tx)
                .await?;
        let approval = match &row {
            Some((status, expires)) if status == "denied" && *expires > now as i64 => {
                tx.commit().await?;
                return Ok(DirectApproval::Denied);
            }
            Some((status, _)) if status == "approved" => DirectApproval::Repeated,
            _ => DirectApproval::New,
        };
        sqlx::query(
            "INSERT INTO follow_requests
                (pubkey, timestamp, status, created_at, expires_at, decided_at)
             VALUES (?1, ?2, 'approved', ?3, ?4, ?3)
             ON CONFLICT(pubkey) DO UPDATE SET status='approved', timestamp=?2,
                decided_at=MAX(decided_at, ?3)",
        )
        .bind(pubkey)
        .bind(timestamp as i64)
        .bind(now as i64)
        .bind(expires_at as i64)
        .execute(&mut *tx)
        .await?;
        Self::upsert_follower_tx(&mut tx, me, pubkey, now).await?;
        tx.commit().await?;
        Ok(approval)
    }

    /// Deny the pending, unexpired request of `pubkey`.
    pub async fn deny_follow_request(&self, pubkey: &str, now: u64) -> Result<bool, AppError> {
        let result = sqlx::query(
            "UPDATE follow_requests SET status='denied', decided_at=?2
             WHERE pubkey=?1 AND status='pending' AND expires_at > ?2",
        )
        .bind(pubkey)
        .bind(now as i64)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Withdraw the approval or pending request of `pubkey`, the follower of `me`: the row
    /// becomes denied at `now` until `expires_at`, and `pubkey` is no longer stored as a
    /// follower. Returns `false` if `pubkey` had no approved or pending row. Device sync
    /// copies the denial to the other devices.
    pub async fn revoke_follower(
        &self,
        me: &str,
        pubkey: &str,
        now: u64,
        expires_at: u64,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let revoked = sqlx::query(
            "UPDATE follow_requests SET status='denied', decided_at=?2, expires_at=?3
             WHERE pubkey=?1 AND status IN ('approved', 'pending')",
        )
        .bind(pubkey)
        .bind(now as i64)
        .bind(i64::try_from(expires_at).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        Self::delete_follower_tx(&mut tx, me, pubkey).await?;
        tx.commit().await?;
        Ok(revoked)
    }

    /// All approved requests plus the pending and denied requests that have not expired.
    pub async fn get_follow_requests(&self, now: u64) -> Result<Vec<FollowRequestEntry>, AppError> {
        let rows = sqlx::query(
            "SELECT pubkey, timestamp, status, created_at, expires_at, decided_at
             FROM follow_requests
             WHERE status='approved' OR expires_at > ?1
             ORDER BY created_at DESC, pubkey ASC",
        )
        .bind(now as i64)
        .fetch_all(&self.pool)
        .await?;
        let mut entries = Vec::new();
        for row in &rows {
            entries.push(FollowRequestEntry {
                pubkey: row.get(0),
                timestamp: row.get::<i64, _>(1) as u64,
                status: row.get(2),
                created_at: row.get::<i64, _>(3) as u64,
                expires_at: row.get::<i64, _>(4) as u64,
                decided_at: row.get::<i64, _>(5) as u64,
            });
        }
        Ok(entries)
    }

    /// Number of pending requests that have not expired.
    pub async fn get_pending_follow_request_count(&self, now: u64) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM follow_requests WHERE status='pending' AND expires_at > ?1",
        )
        .bind(now as i64)
        .fetch_one(&self.pool)
        .await?;
        Ok(count as u64)
    }

    /// Merge follow requests received from another device of the local user `me`.
    ///
    /// A received decision (approved or denied) replaces the local row only if its
    /// `decided_at` is greater; a pending row is inserted only if no row exists. A merged
    /// approval also records `pubkey` as a follower of `me`, and a merged denial removes
    /// that record. Afterwards only the newest `MAX_PENDING_FOLLOW_REQUESTS` pending rows
    /// are kept, so rows that one device evicted do not return. Entries with an invalid
    /// pubkey or status, and expired pending or denied entries, are skipped. Returns the
    /// number of rows written.
    pub async fn merge_follow_requests(
        &self,
        me: &str,
        entries: &[FollowRequestEntry],
    ) -> Result<u32, AppError> {
        let now = proscenium_types::now_millis();
        let clamp = |t: u64| i64::try_from(t.min(now)).unwrap_or(i64::MAX);
        let mut merged = 0u32;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for entry in entries {
            let decided = matches!(entry.status.as_str(), "approved" | "denied");
            if entry.pubkey == me
                || !proscenium_types::is_valid_pubkey(&entry.pubkey)
                || !(decided || entry.status == "pending")
                || (entry.status != "approved" && entry.expires_at <= now)
            {
                continue;
            }
            let decided_at = if decided {
                clamp(entry.decided_at).max(1)
            } else {
                0
            };
            let result = sqlx::query(
                "INSERT INTO follow_requests
                    (pubkey, timestamp, status, created_at, expires_at, decided_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(pubkey) DO UPDATE SET
                    timestamp = excluded.timestamp,
                    status = excluded.status,
                    expires_at = excluded.expires_at,
                    decided_at = excluded.decided_at
                 WHERE excluded.decided_at > follow_requests.decided_at",
            )
            .bind(&entry.pubkey)
            .bind(clamp(entry.timestamp))
            .bind(&entry.status)
            .bind(clamp(entry.created_at))
            .bind(i64::try_from(entry.expires_at).unwrap_or(i64::MAX))
            .bind(decided_at)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() == 0 {
                continue;
            }
            merged += 1;
            match entry.status.as_str() {
                "approved" => {
                    Self::upsert_follower_tx(&mut tx, me, &entry.pubkey, now).await?;
                }
                "denied" => Self::delete_follower_tx(&mut tx, me, &entry.pubkey).await?,
                _ => {}
            }
        }
        evict_excess_pending_tx(&mut tx).await?;
        tx.commit().await?;
        Ok(merged)
    }

    pub async fn is_approved_follower(&self, pubkey: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM follow_requests WHERE pubkey=?1 AND status='approved'",
        )
        .bind(pubkey)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }
}
