use iroh_social_types::{Interaction, Post, Profile, Visibility};
use serde::Serialize;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

pub struct Storage {
    pub pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Registration {
    pub pubkey: String,
    pub registered_at: i64,
    pub last_seen: i64,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_hash: Option<String>,
    pub visibility: String,
    pub is_active: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserInfo {
    pub pubkey: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_hash: Option<String>,
    pub visibility: String,
    pub registered_at: i64,
    pub post_count: i64,
    pub latest_post_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StoredPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub media_json: Option<String>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StoredInteraction {
    pub id: String,
    pub author: String,
    pub kind: String,
    pub target_post_id: String,
    pub target_author: String,
    pub timestamp: i64,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TrendingHashtag {
    pub tag: String,
    pub post_count: i64,
    pub unique_authors: i64,
    pub latest_post_at: i64,
    pub score: f64,
    pub computed_at: i64,
}

impl Storage {
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;

        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA foreign_keys=ON").execute(&pool).await?;
        sqlx::query("PRAGMA busy_timeout=5000")
            .execute(&pool)
            .await?;

        let migration_sql = include_str!("../migrations/001_initial.sql");
        sqlx::raw_sql(migration_sql).execute(&pool).await?;

        Ok(Self { pool })
    }

    // --- Registrations ---

    pub async fn register_user(
        &self,
        pubkey: &str,
        visibility: &str,
        profile: Option<&Profile>,
    ) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis() as i64;
        let (display_name, bio, avatar_hash) = match profile {
            Some(p) => (
                Some(p.display_name.as_str()),
                Some(p.bio.as_str()),
                p.avatar_hash.as_deref(),
            ),
            None => (None, None, None),
        };

        sqlx::query(
            "INSERT INTO registrations (pubkey, registered_at, last_seen, display_name, bio, avatar_hash, visibility, is_active)
             VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT(pubkey) DO UPDATE SET
                last_seen = ?2,
                display_name = COALESCE(?3, display_name),
                bio = COALESCE(?4, bio),
                avatar_hash = COALESCE(?5, avatar_hash),
                visibility = ?6,
                is_active = 1",
        )
        .bind(pubkey)
        .bind(now)
        .bind(display_name)
        .bind(bio)
        .bind(avatar_hash)
        .bind(visibility)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn unregister_user(&self, pubkey: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM interactions WHERE author = ?1")
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM posts WHERE author = ?1")
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM sync_state WHERE pubkey = ?1")
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        sqlx::query("UPDATE registrations SET is_active = 0 WHERE pubkey = ?1")
            .bind(pubkey)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_registration(&self, pubkey: &str) -> anyhow::Result<Option<Registration>> {
        let reg = sqlx::query_as::<_, Registration>(
            "SELECT * FROM registrations WHERE pubkey = ?1 AND is_active = 1",
        )
        .bind(pubkey)
        .fetch_optional(&self.pool)
        .await?;
        Ok(reg)
    }

    pub async fn get_active_public_pubkeys(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT pubkey FROM registrations WHERE is_active = 1 AND visibility = 'public'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn update_profile(&self, pubkey: &str, profile: &Profile) -> anyhow::Result<bool> {
        let now = iroh_social_types::now_millis() as i64;
        let vis = profile.visibility.to_string();
        let result = sqlx::query(
            "UPDATE registrations SET display_name = ?2, bio = ?3, avatar_hash = ?4, visibility = ?5, last_seen = ?6
             WHERE pubkey = ?1 AND is_active = 1",
        )
        .bind(pubkey)
        .bind(&profile.display_name)
        .bind(&profile.bio)
        .bind(&profile.avatar_hash)
        .bind(&vis)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn handle_visibility_change(
        &self,
        pubkey: &str,
        new_visibility: Visibility,
    ) -> anyhow::Result<()> {
        match new_visibility {
            Visibility::Private => {
                self.unregister_user(pubkey).await?;
            }
            Visibility::Listed => {
                sqlx::query("DELETE FROM interactions WHERE author = ?1")
                    .bind(pubkey)
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DELETE FROM posts WHERE author = ?1")
                    .bind(pubkey)
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DELETE FROM sync_state WHERE pubkey = ?1")
                    .bind(pubkey)
                    .execute(&self.pool)
                    .await?;
                sqlx::query(
                    "UPDATE registrations SET visibility = 'listed' WHERE pubkey = ?1 AND is_active = 1",
                )
                .bind(pubkey)
                .execute(&self.pool)
                .await?;
            }
            Visibility::Public => {
                sqlx::query(
                    "UPDATE registrations SET visibility = 'public' WHERE pubkey = ?1 AND is_active = 1",
                )
                .bind(pubkey)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    pub async fn registration_count(&self) -> anyhow::Result<i64> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM registrations WHERE is_active = 1")
                .fetch_one(&self.pool)
                .await?;
        Ok(count.0)
    }

    // --- Posts ---

    pub async fn insert_post(&self, post: &Post) -> anyhow::Result<bool> {
        let now = iroh_social_types::now_millis() as i64;
        let media_json = if post.media.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&post.media)?)
        };

        let result = sqlx::query(
            "INSERT OR IGNORE INTO posts (id, author, content, timestamp, media_json, reply_to, reply_to_author, quote_of, quote_of_author, signature, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(&post.id)
        .bind(&post.author)
        .bind(&post.content)
        .bind(post.timestamp as i64)
        .bind(&media_json)
        .bind(&post.reply_to)
        .bind(&post.reply_to_author)
        .bind(&post.quote_of)
        .bind(&post.quote_of_author)
        .bind(&post.signature)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_post(&self, id: &str, author: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM posts WHERE id = ?1 AND author = ?2")
            .bind(id)
            .bind(author)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_post(&self, author: &str, id: &str) -> anyhow::Result<Option<StoredPost>> {
        let post =
            sqlx::query_as::<_, StoredPost>("SELECT * FROM posts WHERE author = ?1 AND id = ?2")
                .bind(author)
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(post)
    }

    pub async fn get_user_posts(
        &self,
        pubkey: &str,
        limit: i64,
        before: Option<i64>,
    ) -> anyhow::Result<Vec<StoredPost>> {
        let posts = if let Some(before_ts) = before {
            sqlx::query_as::<_, StoredPost>(
                "SELECT * FROM posts WHERE author = ?1 AND timestamp < ?2 ORDER BY timestamp DESC LIMIT ?3",
            )
            .bind(pubkey)
            .bind(before_ts)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, StoredPost>(
                "SELECT * FROM posts WHERE author = ?1 ORDER BY timestamp DESC LIMIT ?2",
            )
            .bind(pubkey)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };
        Ok(posts)
    }

    pub async fn get_feed(
        &self,
        limit: i64,
        before: Option<i64>,
        authors: Option<&[String]>,
    ) -> anyhow::Result<Vec<StoredPost>> {
        if let Some(author_list) = authors {
            if author_list.is_empty() {
                return Ok(vec![]);
            }
            let placeholders: Vec<String> = (0..author_list.len())
                .map(|i| format!("?{}", i + 3))
                .collect();
            let in_clause = placeholders.join(",");

            let sql = if before.is_some() {
                format!(
                    "SELECT p.* FROM posts p JOIN registrations r ON p.author = r.pubkey
                     WHERE r.visibility = 'public' AND r.is_active = 1 AND p.author IN ({in_clause}) AND p.timestamp < ?1
                     ORDER BY p.timestamp DESC LIMIT ?2"
                )
            } else {
                format!(
                    "SELECT p.* FROM posts p JOIN registrations r ON p.author = r.pubkey
                     WHERE r.visibility = 'public' AND r.is_active = 1 AND p.author IN ({in_clause})
                     ORDER BY p.timestamp DESC LIMIT ?2"
                )
            };

            let mut query = sqlx::query_as::<_, StoredPost>(&sql);
            query = query.bind(before.unwrap_or(i64::MAX));
            query = query.bind(limit);
            for author in author_list {
                query = query.bind(author);
            }
            Ok(query.fetch_all(&self.pool).await?)
        } else {
            let posts = if let Some(before_ts) = before {
                sqlx::query_as::<_, StoredPost>(
                    "SELECT p.* FROM posts p JOIN registrations r ON p.author = r.pubkey
                     WHERE r.visibility = 'public' AND r.is_active = 1 AND p.timestamp < ?1
                     ORDER BY p.timestamp DESC LIMIT ?2",
                )
                .bind(before_ts)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query_as::<_, StoredPost>(
                    "SELECT p.* FROM posts p JOIN registrations r ON p.author = r.pubkey
                     WHERE r.visibility = 'public' AND r.is_active = 1
                     ORDER BY p.timestamp DESC LIMIT ?1",
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            };
            Ok(posts)
        }
    }

    pub async fn search_posts(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<(Vec<StoredPost>, i64)> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM posts_fts f JOIN posts p ON f.rowid = p.rowid
             JOIN registrations r ON p.author = r.pubkey
             WHERE f.content MATCH ?1 AND r.visibility = 'public' AND r.is_active = 1",
        )
        .bind(query)
        .fetch_one(&self.pool)
        .await?;

        let posts = sqlx::query_as::<_, StoredPost>(
            "SELECT p.* FROM posts_fts f JOIN posts p ON f.rowid = p.rowid
             JOIN registrations r ON p.author = r.pubkey
             WHERE f.content MATCH ?1 AND r.visibility = 'public' AND r.is_active = 1
             ORDER BY p.timestamp DESC LIMIT ?2 OFFSET ?3",
        )
        .bind(query)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        Ok((posts, count.0))
    }

    pub async fn total_post_count(&self) -> anyhow::Result<i64> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts")
            .fetch_one(&self.pool)
            .await?;
        Ok(count.0)
    }

    // --- Interactions ---

    pub async fn insert_interaction(&self, interaction: &Interaction) -> anyhow::Result<bool> {
        let now = iroh_social_types::now_millis() as i64;
        let kind = format!("{:?}", interaction.kind);

        let result = sqlx::query(
            "INSERT OR IGNORE INTO interactions (id, author, kind, target_post_id, target_author, timestamp, signature, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&interaction.id)
        .bind(&interaction.author)
        .bind(&kind)
        .bind(&interaction.target_post_id)
        .bind(&interaction.target_author)
        .bind(interaction.timestamp as i64)
        .bind(&interaction.signature)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_interaction(&self, id: &str, author: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM interactions WHERE id = ?1 AND author = ?2")
            .bind(id)
            .bind(author)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn get_post_interactions(
        &self,
        target_author: &str,
        target_post_id: &str,
    ) -> anyhow::Result<Vec<StoredInteraction>> {
        let interactions = sqlx::query_as::<_, StoredInteraction>(
            "SELECT * FROM interactions WHERE target_author = ?1 AND target_post_id = ?2 ORDER BY timestamp DESC",
        )
        .bind(target_author)
        .bind(target_post_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(interactions)
    }

    pub async fn get_post_like_count(
        &self,
        target_author: &str,
        target_post_id: &str,
    ) -> anyhow::Result<i64> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM interactions WHERE target_author = ?1 AND target_post_id = ?2 AND kind = 'Like'",
        )
        .bind(target_author)
        .bind(target_post_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count.0)
    }

    // --- Users ---

    pub async fn list_users(
        &self,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<(Vec<UserInfo>, i64)> {
        let total: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM registrations WHERE is_active = 1")
                .fetch_one(&self.pool)
                .await?;

        let rows = sqlx::query_as::<_, Registration>(
            "SELECT * FROM registrations WHERE is_active = 1 ORDER BY last_seen DESC LIMIT ?1 OFFSET ?2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let mut users = Vec::with_capacity(rows.len());
        for reg in rows {
            let (post_count, latest_post_at) = if reg.visibility == "public" {
                let pc: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts WHERE author = ?1")
                    .bind(&reg.pubkey)
                    .fetch_one(&self.pool)
                    .await?;
                let lp: Option<(i64,)> =
                    sqlx::query_as("SELECT MAX(timestamp) FROM posts WHERE author = ?1")
                        .bind(&reg.pubkey)
                        .fetch_optional(&self.pool)
                        .await?;
                (pc.0, lp.map(|r| r.0))
            } else {
                (0, None)
            };

            users.push(UserInfo {
                pubkey: reg.pubkey,
                display_name: reg.display_name,
                bio: reg.bio,
                avatar_hash: reg.avatar_hash,
                visibility: reg.visibility,
                registered_at: reg.registered_at,
                post_count,
                latest_post_at,
            });
        }

        Ok((users, total.0))
    }

    pub async fn search_users(&self, query: &str, limit: i64) -> anyhow::Result<Vec<UserInfo>> {
        let pattern = format!("%{query}%");
        let rows = sqlx::query_as::<_, Registration>(
            "SELECT * FROM registrations WHERE is_active = 1 AND display_name LIKE ?1 ORDER BY last_seen DESC LIMIT ?2",
        )
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let users = rows
            .into_iter()
            .map(|reg| UserInfo {
                pubkey: reg.pubkey,
                display_name: reg.display_name,
                bio: reg.bio,
                avatar_hash: reg.avatar_hash,
                visibility: reg.visibility,
                registered_at: reg.registered_at,
                post_count: 0,
                latest_post_at: None,
            })
            .collect();
        Ok(users)
    }

    pub async fn get_user_info(&self, pubkey: &str) -> anyhow::Result<Option<UserInfo>> {
        let reg = self.get_registration(pubkey).await?;
        let Some(reg) = reg else {
            return Ok(None);
        };

        let (post_count, latest_post_at) = if reg.visibility == "public" {
            let pc: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM posts WHERE author = ?1")
                .bind(pubkey)
                .fetch_one(&self.pool)
                .await?;
            let lp: Option<(i64,)> =
                sqlx::query_as("SELECT MAX(timestamp) FROM posts WHERE author = ?1")
                    .bind(pubkey)
                    .fetch_optional(&self.pool)
                    .await?;
            (pc.0, lp.map(|r| r.0))
        } else {
            (0, None)
        };

        Ok(Some(UserInfo {
            pubkey: reg.pubkey,
            display_name: reg.display_name,
            bio: reg.bio,
            avatar_hash: reg.avatar_hash,
            visibility: reg.visibility,
            registered_at: reg.registered_at,
            post_count,
            latest_post_at,
        }))
    }

    // --- Trending ---

    pub async fn get_trending_hashtags(&self, limit: i64) -> anyhow::Result<Vec<TrendingHashtag>> {
        let tags = sqlx::query_as::<_, TrendingHashtag>(
            "SELECT * FROM trending_hashtags ORDER BY score DESC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(tags)
    }

    pub async fn update_trending(&self, tags: &[TrendingHashtag]) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM trending_hashtags")
            .execute(&self.pool)
            .await?;

        for tag in tags {
            sqlx::query(
                "INSERT INTO trending_hashtags (tag, post_count, unique_authors, latest_post_at, score, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(&tag.tag)
            .bind(tag.post_count)
            .bind(tag.unique_authors)
            .bind(tag.latest_post_at)
            .bind(tag.score)
            .bind(tag.computed_at)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    // --- Sync state ---

    pub async fn update_sync_state(
        &self,
        pubkey: &str,
        last_post_ts: Option<i64>,
        last_interaction_ts: Option<i64>,
    ) -> anyhow::Result<()> {
        let now = iroh_social_types::now_millis() as i64;
        sqlx::query(
            "INSERT INTO sync_state (pubkey, last_synced_at, last_post_timestamp, last_interaction_timestamp)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(pubkey) DO UPDATE SET
                last_synced_at = ?2,
                last_post_timestamp = COALESCE(?3, last_post_timestamp),
                last_interaction_timestamp = COALESCE(?4, last_interaction_timestamp)",
        )
        .bind(pubkey)
        .bind(now)
        .bind(last_post_ts)
        .bind(last_interaction_ts)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_last_post_timestamp(&self, pubkey: &str) -> anyhow::Result<Option<u64>> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT last_post_timestamp FROM sync_state WHERE pubkey = ?1")
                .bind(pubkey)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0).map(|ts| ts as u64))
    }

    pub async fn get_last_interaction_timestamp(
        &self,
        pubkey: &str,
    ) -> anyhow::Result<Option<u64>> {
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT last_interaction_timestamp FROM sync_state WHERE pubkey = ?1")
                .bind(pubkey)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.and_then(|r| r.0).map(|ts| ts as u64))
    }
}
