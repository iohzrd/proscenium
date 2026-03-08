use crate::storage::{Storage, TrendingHashtag};
use std::collections::HashMap;
use std::sync::Arc;

/// Extract hashtags from content, normalized to lowercase.
fn extract_hashtags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '#' {
            let mut tag = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '_' {
                    tag.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if !tag.is_empty() {
                tags.push(tag.to_lowercase());
            }
        }
    }
    tags
}

pub async fn compute_trending(storage: &Storage, window_hours: u64) -> anyhow::Result<()> {
    let now = iroh_social_types::now_millis() as i64;
    let window_ms = (window_hours * 60 * 60 * 1000) as i64;
    let cutoff = now - window_ms;

    // Fetch all posts in the window from Public users
    let posts = storage
        .get_feed(10000, None, None)
        .await?
        .into_iter()
        .filter(|p| p.timestamp >= cutoff)
        .collect::<Vec<_>>();

    let post_count_total = posts.len();

    // Offload CPU-bound aggregation and scoring to a blocking thread
    // to avoid starving the async runtime
    let trending = tokio::task::spawn_blocking(move || {
        let mut tag_stats: HashMap<String, TagStats> = HashMap::new();

        for post in &posts {
            let tags = extract_hashtags(&post.content);
            let post_ts = post.timestamp;
            for tag in tags {
                let stats = tag_stats.entry(tag).or_insert_with(|| TagStats {
                    posts: Vec::new(),
                    authors: std::collections::HashSet::new(),
                });
                stats.posts.push(post_ts);
                stats.authors.insert(post.author.clone());
            }
        }

        let mut trending: Vec<TrendingHashtag> = Vec::new();

        for (tag, stats) in &tag_stats {
            let post_count = stats.posts.len() as i64;
            let unique_authors = stats.authors.len() as i64;
            let author_weight = (unique_authors as f64).sqrt();

            let latest_post_at = *stats.posts.iter().max().unwrap_or(&now);
            let oldest_post_at = *stats.posts.iter().min().unwrap_or(&now);

            let recency_factor: f64 = stats
                .posts
                .iter()
                .map(|&ts| {
                    let hours_since = (now - ts) as f64 / 3_600_000.0;
                    1.0 / (1.0 + hours_since)
                })
                .sum();

            let hours_since_oldest = (now - oldest_post_at) as f64 / 3_600_000.0;
            let age_decay = 1.0 + (hours_since_oldest / 24.0);

            let score = (post_count as f64 * author_weight * recency_factor) / age_decay;

            trending.push(TrendingHashtag {
                tag: tag.clone(),
                post_count,
                unique_authors,
                latest_post_at,
                score,
                computed_at: now,
            });
        }

        trending.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        trending.truncate(50);
        trending
    })
    .await?;

    storage.update_trending(&trending).await?;
    tracing::info!(
        "[trending] computed {} trending hashtags from {} posts",
        trending.len(),
        post_count_total,
    );

    Ok(())
}

pub fn start_trending_task(storage: Arc<Storage>, interval_minutes: u64, window_hours: u64) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(interval_minutes * 60);
        loop {
            if let Err(e) = compute_trending(&storage, window_hours).await {
                tracing::error!("[trending] computation error: {e}");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

struct TagStats {
    posts: Vec<i64>,
    authors: std::collections::HashSet<String>,
}
