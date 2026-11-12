use crate::error::AppError;
use proscenium_types::{MAX_SYNC_TOMBSTONES, MediaAttachment, Post, PostTombstone};
use sqlx::Row;

use super::Storage;

/// Number of rows `apply_post_tombstones` changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TombstoneOutcome {
    /// Post rows deleted.
    pub posts_removed: u64,
    /// Tombstone rows inserted (a tombstone that was already stored is not counted).
    pub tombstones_added: u64,
}

impl Storage {
    // ---- Post tombstones ----

    /// Store signed deletions of posts by `author` and delete the posts they identify.
    ///
    /// The caller has verified each signature with `verify_delete_post_signature` and each
    /// id with `is_valid_id`; entries whose `author` differs from the argument are skipped.
    /// Tombstone insert, post delete and the delete of the post's notifications run in one
    /// write transaction. At most `MAX_SYNC_TOMBSTONES` tombstones are kept per author;
    /// the ones with the lowest `deleted_at` are removed beyond that.
    pub async fn apply_post_tombstones(
        &self,
        author: &str,
        tombstones: &[PostTombstone],
        now: u64,
    ) -> Result<TombstoneOutcome, AppError> {
        let mut outcome = TombstoneOutcome::default();
        if tombstones.is_empty() {
            return Ok(outcome);
        }
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for tombstone in tombstones.iter().filter(|t| t.author == author) {
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO post_tombstones (author, id, signature, deleted_at)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(author)
            .bind(&tombstone.id)
            .bind(&tombstone.signature)
            .bind(now as i64)
            .execute(&mut *tx)
            .await?;
            outcome.tombstones_added += inserted.rows_affected();

            let removed = sqlx::query("DELETE FROM posts WHERE author=?1 AND id=?2")
                .bind(author)
                .bind(&tombstone.id)
                .execute(&mut *tx)
                .await?;
            if removed.rows_affected() > 0 {
                outcome.posts_removed += removed.rows_affected();
                Self::delete_post_dependents(&mut tx, &tombstone.id).await?;
            }
        }
        if outcome.tombstones_added > 0 {
            Self::trim_post_tombstones(&mut tx, author).await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }

    /// Store the signed deletion of the own post `(author, id)` in `tx`, keeping at most
    /// `MAX_SYNC_TOMBSTONES` deletions of `author`.
    pub(super) async fn insert_post_tombstone(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        author: &str,
        id: &str,
        signature: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT OR IGNORE INTO post_tombstones (author, id, signature, deleted_at)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(author)
        .bind(id)
        .bind(signature)
        .bind(proscenium_types::now_millis() as i64)
        .execute(&mut **tx)
        .await?;
        Self::trim_post_tombstones(tx, author).await
    }

    /// Keep the newest `MAX_SYNC_TOMBSTONES` deletions of `author`.
    async fn trim_post_tombstones(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        author: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "DELETE FROM post_tombstones WHERE author=?1 AND id NOT IN
                (SELECT id FROM post_tombstones WHERE author=?1
                 ORDER BY deleted_at DESC, id ASC LIMIT ?2)",
        )
        .bind(author)
        .bind(MAX_SYNC_TOMBSTONES as i64)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn is_post_tombstoned(&self, author: &str, id: &str) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM post_tombstones WHERE author=?1 AND id=?2",
        )
        .bind(author)
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    pub async fn count_post_tombstones(&self, author: &str) -> Result<u64, AppError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_tombstones WHERE author=?1")
            .bind(author)
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }

    /// The tombstones stored for `author`, newest `deleted_at` first, at most `limit`.
    pub async fn get_post_tombstones(
        &self,
        author: &str,
        limit: usize,
    ) -> Result<Vec<PostTombstone>, AppError> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, signature FROM post_tombstones WHERE author=?1
             ORDER BY deleted_at DESC, id ASC LIMIT ?2",
        )
        .bind(author)
        .bind(limit.min(MAX_SYNC_TOMBSTONES) as i64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, signature)| PostTombstone {
                id,
                author: author.to_string(),
                signature,
            })
            .collect())
    }

    /// Delete the post `(author, id)` and its notifications if a tombstone exists for it.
    /// Returns `true` if a post row was deleted.
    ///
    /// Called after an insert to cover a tombstone that was stored between the tombstone
    /// check and the insert.
    pub async fn remove_post_if_tombstoned(
        &self,
        author: &str,
        id: &str,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let removed = sqlx::query(
            "DELETE FROM posts WHERE author=?1 AND id=?2
             AND EXISTS (SELECT 1 FROM post_tombstones WHERE author=?1 AND id=?2)",
        )
        .bind(author)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if removed {
            sqlx::query("DELETE FROM notifications WHERE post_id=?1 OR target_post_id=?1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(removed)
    }

    // ---- Interaction tombstones ----

    /// Store a verified deletion of the interaction `(author, id)`. The caller deletes the
    /// interaction row afterwards; once the tombstone exists the row is not stored again.
    /// At most `MAX_SYNC_TOMBSTONES` tombstones are kept per author.
    pub async fn record_interaction_tombstone(
        &self,
        author: &str,
        id: &str,
        now: u64,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO interaction_tombstones (author, id, deleted_at)
             VALUES (?1, ?2, ?3)",
        )
        .bind(author)
        .bind(id)
        .bind(now as i64)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if inserted {
            sqlx::query(
                "DELETE FROM interaction_tombstones WHERE author=?1 AND id NOT IN
                    (SELECT id FROM interaction_tombstones WHERE author=?1
                     ORDER BY deleted_at DESC, id ASC LIMIT ?2)",
            )
            .bind(author)
            .bind(MAX_SYNC_TOMBSTONES as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn is_interaction_tombstoned(
        &self,
        author: &str,
        id: &str,
    ) -> Result<bool, AppError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM interaction_tombstones WHERE author=?1 AND id=?2",
        )
        .bind(author)
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(exists)
    }

    /// Ids of the interaction tombstones stored for `author`, newest `deleted_at` first,
    /// at most `limit`.
    pub async fn get_interaction_tombstone_ids(
        &self,
        author: &str,
        limit: usize,
    ) -> Result<Vec<String>, AppError> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM interaction_tombstones WHERE author=?1
             ORDER BY deleted_at DESC, id ASC LIMIT ?2",
        )
        .bind(author)
        .bind(limit.min(MAX_SYNC_TOMBSTONES) as i64)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Delete the interaction `(author, id)` if a tombstone exists for it.
    /// Returns `true` if a row was deleted.
    pub async fn remove_interaction_if_tombstoned(
        &self,
        author: &str,
        id: &str,
    ) -> Result<bool, AppError> {
        let result = sqlx::query(
            "DELETE FROM interactions WHERE author=?1 AND id=?2
             AND EXISTS (SELECT 1 FROM interaction_tombstones WHERE author=?1 AND id=?2)",
        )
        .bind(author)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    // ---- Queries of the sync protocol ----

    /// True if a post row `(author, id)` exists.
    pub async fn has_post(&self, author: &str, id: &str) -> Result<bool, AppError> {
        let exists: bool =
            sqlx::query_scalar("SELECT COUNT(*) > 0 FROM posts WHERE author=?1 AND id=?2")
                .bind(author)
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    /// `(id, timestamp)` of every post by `author`, ordered by `(timestamp, id)` ascending.
    pub async fn get_post_ids_with_timestamps(
        &self,
        author: &str,
    ) -> Result<Vec<(String, u64)>, AppError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT id, timestamp FROM posts WHERE author=?1 ORDER BY timestamp ASC, id ASC",
        )
        .bind(author)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, timestamp)| (id, timestamp as u64))
            .collect())
    }

    /// Ids of the posts by `author` that are not in `known_ids`, ordered by
    /// `(timestamp, id)` ascending.
    ///
    /// `known_ids` is loaded into a temporary table with one statement on one connection.
    /// The surrounding transaction is rolled back at the end (and when the future is
    /// dropped), which removes the temporary table from the pooled connection.
    pub async fn get_missing_post_ids(
        &self,
        author: &str,
        known_ids: &[String],
    ) -> Result<Vec<String>, AppError> {
        let known_json = serde_json::to_string(known_ids)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "CREATE TEMP TABLE IF NOT EXISTS sync_known_ids (id TEXT PRIMARY KEY) WITHOUT ROWID",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM sync_known_ids")
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO sync_known_ids (id) SELECT value FROM json_each(?1)")
            .bind(&known_json)
            .execute(&mut *tx)
            .await?;
        let missing: Vec<String> = sqlx::query_scalar(
            "SELECT p.id FROM posts p
             WHERE p.author=?1 AND p.id NOT IN (SELECT id FROM sync_known_ids)
             ORDER BY p.timestamp ASC, p.id ASC",
        )
        .bind(author)
        .fetch_all(&mut *tx)
        .await?;
        tx.rollback().await?;
        Ok(missing)
    }

    /// The posts `(author, id)` for each id in `ids`, ordered by `(timestamp, id)` ascending.
    /// Ids without a row are omitted.
    pub async fn get_posts_by_ids(
        &self,
        author: &str,
        ids: &[String],
    ) -> Result<Vec<Post>, AppError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_json = serde_json::to_string(ids)?;
        let rows = sqlx::query(
            "SELECT id, author, content, timestamp, media_json, reply_to, reply_to_author, quote_of, quote_of_author, signature
             FROM posts
             WHERE author=?1 AND id IN (SELECT value FROM json_each(?2))
             ORDER BY timestamp ASC, id ASC",
        )
        .bind(author)
        .bind(&ids_json)
        .fetch_all(&self.pool)
        .await?;
        let mut posts = Vec::with_capacity(rows.len());
        for row in &rows {
            let media_json: String = row.get(4);
            let media: Vec<MediaAttachment> = serde_json::from_str(&media_json)?;
            posts.push(Post {
                id: row.get(0),
                author: row.get(1),
                content: row.get(2),
                timestamp: row.get::<i64, _>(3) as u64,
                media,
                reply_to: row.get(5),
                reply_to_author: row.get(6),
                quote_of: row.get(7),
                quote_of_author: row.get(8),
                signature: row.get(9),
            });
        }
        Ok(posts)
    }

    /// Delete the interactions by `author` whose id is not in `keep_ids`.
    /// Returns the number of deleted rows.
    pub async fn delete_interactions_not_in(
        &self,
        author: &str,
        keep_ids: &[String],
    ) -> Result<u64, AppError> {
        let keep_json = serde_json::to_string(keep_ids)?;
        let result = sqlx::query(
            "DELETE FROM interactions
             WHERE author=?1 AND id NOT IN (SELECT value FROM json_each(?2))",
        )
        .bind(author)
        .bind(&keep_json)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proscenium_types::{Interaction, InteractionKind};

    async fn storage() -> Storage {
        Storage::open_in_memory().await.expect("open in-memory db")
    }

    fn post(id: &str, author: &str, timestamp: u64) -> Post {
        Post {
            id: id.to_string(),
            author: author.to_string(),
            content: format!("content of {id}"),
            timestamp,
            media: vec![],
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: "sig".to_string(),
        }
    }

    fn tombstone(id: &str, author: &str) -> PostTombstone {
        PostTombstone {
            id: id.to_string(),
            author: author.to_string(),
            signature: format!("sig-{id}"),
        }
    }

    fn like(id: &str, author: &str, target: &str, timestamp: u64) -> Interaction {
        Interaction {
            id: id.to_string(),
            author: author.to_string(),
            kind: InteractionKind::Like,
            target_post_id: target.to_string(),
            target_author: "target_author".to_string(),
            timestamp,
            signature: "sig".to_string(),
        }
    }

    #[tokio::test]
    async fn post_tombstone_deletes_only_the_authors_post() {
        let s = storage().await;
        s.insert_post(&post("p1", "a", 1)).await.unwrap();
        s.insert_post(&post("p1", "b", 1)).await.unwrap();
        s.insert_notification("mention", "a", None, Some("p1"), 1)
            .await
            .unwrap();

        let outcome = s
            .apply_post_tombstones("a", &[tombstone("p1", "a")], 100)
            .await
            .unwrap();
        assert_eq!(outcome.posts_removed, 1);
        assert_eq!(outcome.tombstones_added, 1);
        assert!(!s.has_post("a", "p1").await.unwrap());
        assert!(s.has_post("b", "p1").await.unwrap());
        assert!(s.is_post_tombstoned("a", "p1").await.unwrap());
        assert!(!s.is_post_tombstoned("b", "p1").await.unwrap());
        assert_eq!(s.get_unread_notification_count().await.unwrap(), 0);

        // Applying the same tombstone again changes nothing.
        let outcome = s
            .apply_post_tombstones("a", &[tombstone("p1", "a")], 200)
            .await
            .unwrap();
        assert_eq!(outcome, TombstoneOutcome::default());
        assert_eq!(s.count_post_tombstones("a").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn post_tombstone_of_another_author_is_skipped() {
        let s = storage().await;
        s.insert_post(&post("p1", "b", 1)).await.unwrap();
        let outcome = s
            .apply_post_tombstones("a", &[tombstone("p1", "b")], 100)
            .await
            .unwrap();
        assert_eq!(outcome, TombstoneOutcome::default());
        assert!(s.has_post("b", "p1").await.unwrap());
        assert_eq!(s.count_post_tombstones("a").await.unwrap(), 0);
        assert_eq!(s.count_post_tombstones("b").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn post_tombstones_are_listed_newest_first_with_signature() {
        let s = storage().await;
        s.apply_post_tombstones("a", &[tombstone("p1", "a")], 100)
            .await
            .unwrap();
        s.apply_post_tombstones("a", &[tombstone("p2", "a")], 300)
            .await
            .unwrap();
        s.apply_post_tombstones("a", &[tombstone("p3", "a")], 200)
            .await
            .unwrap();

        let listed = s.get_post_tombstones("a", 10).await.unwrap();
        let ids: Vec<&str> = listed.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["p2", "p3", "p1"]);
        assert_eq!(listed[0].signature, "sig-p2");
        assert_eq!(listed[0].author, "a");
        assert_eq!(s.get_post_tombstones("a", 2).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn post_tombstones_per_author_are_capped() {
        let s = storage().await;
        let first: Vec<PostTombstone> = (0..MAX_SYNC_TOMBSTONES)
            .map(|i| tombstone(&format!("old{i}"), "a"))
            .collect();
        s.apply_post_tombstones("a", &first, 100).await.unwrap();
        assert_eq!(
            s.count_post_tombstones("a").await.unwrap(),
            MAX_SYNC_TOMBSTONES as u64
        );

        s.apply_post_tombstones("a", &[tombstone("new", "a")], 200)
            .await
            .unwrap();
        assert_eq!(
            s.count_post_tombstones("a").await.unwrap(),
            MAX_SYNC_TOMBSTONES as u64
        );
        // The newest tombstone is kept.
        assert!(s.is_post_tombstoned("a", "new").await.unwrap());
    }

    #[tokio::test]
    async fn remove_post_if_tombstoned_requires_a_tombstone() {
        let s = storage().await;
        s.insert_post(&post("p1", "a", 1)).await.unwrap();
        assert!(!s.remove_post_if_tombstoned("a", "p1").await.unwrap());
        assert!(s.has_post("a", "p1").await.unwrap());

        s.apply_post_tombstones("a", &[tombstone("p1", "a")], 100)
            .await
            .unwrap();
        // The original post arrives again and is inserted by a writer that read the
        // tombstone table before the tombstone was stored.
        s.insert_post(&post("p1", "a", 1)).await.unwrap();
        assert!(s.remove_post_if_tombstoned("a", "p1").await.unwrap());
        assert!(!s.has_post("a", "p1").await.unwrap());
    }

    #[tokio::test]
    async fn interaction_tombstone_blocks_and_removes_the_interaction() {
        let s = storage().await;
        s.save_interaction(&like("i1", "a", "p1", 1)).await.unwrap();
        assert!(!s.is_interaction_tombstoned("a", "i1").await.unwrap());
        assert!(!s.remove_interaction_if_tombstoned("a", "i1").await.unwrap());

        s.record_interaction_tombstone("a", "i1", 100)
            .await
            .unwrap();
        assert!(s.is_interaction_tombstoned("a", "i1").await.unwrap());
        assert!(!s.is_interaction_tombstoned("b", "i1").await.unwrap());
        assert!(s.remove_interaction_if_tombstoned("a", "i1").await.unwrap());
        assert_eq!(s.count_interactions_by_author("a").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn missing_post_ids_are_computed_against_the_known_list() {
        let s = storage().await;
        s.insert_post(&post("p1", "a", 300)).await.unwrap();
        s.insert_post(&post("p2", "a", 100)).await.unwrap();
        s.insert_post(&post("p3", "a", 200)).await.unwrap();
        s.insert_post(&post("p4", "b", 50)).await.unwrap();

        let known = vec!["p3".to_string(), "unknown".to_string()];
        let missing = s.get_missing_post_ids("a", &known).await.unwrap();
        assert_eq!(missing, vec!["p2", "p1"]); // (timestamp, id) ascending

        // The temporary table of the first call does not affect the second call on the
        // same pooled connection.
        let missing = s.get_missing_post_ids("a", &[]).await.unwrap();
        assert_eq!(missing, vec!["p2", "p3", "p1"]);
        let missing = s
            .get_missing_post_ids("a", &["p1".into(), "p2".into(), "p3".into()])
            .await
            .unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn posts_by_ids_returns_only_the_authors_rows_in_order() {
        let s = storage().await;
        s.insert_post(&post("p1", "a", 300)).await.unwrap();
        s.insert_post(&post("p2", "a", 100)).await.unwrap();
        s.insert_post(&post("p2", "b", 100)).await.unwrap();

        let ids = vec!["p1".to_string(), "p2".to_string(), "absent".to_string()];
        let posts = s.get_posts_by_ids("a", &ids).await.unwrap();
        let got: Vec<(&str, &str)> = posts
            .iter()
            .map(|p| (p.author.as_str(), p.id.as_str()))
            .collect();
        assert_eq!(got, vec![("a", "p2"), ("a", "p1")]);
        assert!(s.get_posts_by_ids("a", &[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_ids_with_timestamps_are_ordered() {
        let s = storage().await;
        s.insert_post(&post("pb", "a", 100)).await.unwrap();
        s.insert_post(&post("pa", "a", 100)).await.unwrap();
        s.insert_post(&post("pc", "a", 50)).await.unwrap();
        assert_eq!(
            s.get_post_ids_with_timestamps("a").await.unwrap(),
            vec![
                ("pc".to_string(), 50),
                ("pa".to_string(), 100),
                ("pb".to_string(), 100)
            ]
        );
    }

    #[tokio::test]
    async fn interactions_not_in_the_kept_set_are_deleted_for_one_author() {
        let s = storage().await;
        s.save_interaction(&like("i1", "a", "p1", 1)).await.unwrap();
        s.save_interaction(&like("i2", "a", "p2", 2)).await.unwrap();
        s.save_interaction(&like("i3", "b", "p1", 3)).await.unwrap();

        let removed = s
            .delete_interactions_not_in("a", &["i2".to_string()])
            .await
            .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(s.count_interactions_by_author("a").await.unwrap(), 1);
        assert_eq!(s.count_interactions_by_author("b").await.unwrap(), 1);
    }
}
