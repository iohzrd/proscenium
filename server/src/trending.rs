use crate::content::extract_hashtags;
use crate::storage::{Storage, TrendingHashtag};
use futures_lite::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Number of hashtags kept in the trending table.
const MAX_TRENDING_TAGS: usize = 50;
/// Distinct hashtags tracked during one computation. Further new hashtags are not counted.
const MAX_TRACKED_TAGS: usize = 100_000;

const MS_PER_HOUR: f64 = 3_600_000.0;

#[derive(Default)]
struct TagStats {
    /// Posts that contain the tag.
    post_count: i64,
    latest_post_at: i64,
    /// Timestamp of the newest post with the tag, per author.
    newest_by_author: HashMap<String, i64>,
}

/// Accumulates hashtag use over the posts of one window.
#[derive(Default)]
pub struct TrendingStats {
    tags: HashMap<String, TagStats>,
}

impl TrendingStats {
    /// Count one post. Each distinct hashtag of the post is counted once.
    pub fn add_post(&mut self, author: &str, content: &str, timestamp: i64) {
        for tag in extract_hashtags(content) {
            if !self.tags.contains_key(&tag) && self.tags.len() >= MAX_TRACKED_TAGS {
                continue;
            }
            let stats = self.tags.entry(tag).or_default();
            stats.post_count += 1;
            stats.latest_post_at = stats.latest_post_at.max(timestamp);
            let newest = stats
                .newest_by_author
                .entry(author.to_string())
                .or_insert(timestamp);
            *newest = (*newest).max(timestamp);
        }
    }

    /// The `MAX_TRENDING_TAGS` hashtags with the highest score at time `now` (Unix ms).
    ///
    /// The score of a tag is the sum over its distinct authors of
    /// `1 / (1 + hours since that author's newest post with the tag)`. Each author
    /// adds at most 1, so repeated use by one author does not raise the score above
    /// that of a single recent post.
    pub fn into_ranked(self, now: i64) -> Vec<TrendingHashtag> {
        let mut ranked: Vec<TrendingHashtag> = self
            .tags
            .into_iter()
            .map(|(tag, stats)| {
                let score = stats
                    .newest_by_author
                    .values()
                    .map(|&newest| {
                        let hours = now.saturating_sub(newest).max(0) as f64 / MS_PER_HOUR;
                        1.0 / (1.0 + hours)
                    })
                    .sum();
                TrendingHashtag {
                    tag,
                    post_count: stats.post_count,
                    unique_authors: stats.newest_by_author.len() as i64,
                    latest_post_at: stats.latest_post_at,
                    score,
                    computed_at: now,
                }
            })
            .collect();
        ranked.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.tag.cmp(&b.tag)));
        ranked.truncate(MAX_TRENDING_TAGS);
        ranked
    }
}

/// Recompute the trending table from the posts of the last `window_hours` hours.
pub async fn compute_trending(storage: &Storage, window_hours: u64) -> anyhow::Result<()> {
    let now = i64::try_from(proscenium_types::now_millis()).unwrap_or(i64::MAX);
    let window_ms = i64::try_from(window_hours.saturating_mul(3_600_000)).unwrap_or(i64::MAX);
    let cutoff = now.saturating_sub(window_ms);

    let mut stats = TrendingStats::default();
    let mut post_count = 0u64;
    {
        // Rows are read one at a time; only the per-tag counters are kept in memory.
        let mut rows = std::pin::pin!(storage.hashtag_posts_since(cutoff));
        while let Some(row) = rows.next().await {
            let (author, content, timestamp) = row?;
            stats.add_post(&author, &content, timestamp);
            post_count += 1;
        }
    }

    let trending = stats.into_ranked(now);
    storage.update_trending(&trending).await?;
    tracing::info!(
        "[trending] computed {} trending hashtags from {} posts containing '#'",
        trending.len(),
        post_count,
    );

    Ok(())
}

pub fn start_trending_task(storage: Arc<Storage>, interval_minutes: u64, window_hours: u64) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(interval_minutes.saturating_mul(60));
        loop {
            if let Err(e) = compute_trending(&storage, window_hours).await {
                tracing::error!("[trending] computation error: {e}");
            }
            tokio::time::sleep(interval).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn rank(posts: &[(&str, &str, i64)]) -> Vec<TrendingHashtag> {
        let mut stats = TrendingStats::default();
        for (author, content, timestamp) in posts {
            stats.add_post(author, content, *timestamp);
        }
        stats.into_ranked(NOW)
    }

    fn find<'a>(ranked: &'a [TrendingHashtag], tag: &str) -> &'a TrendingHashtag {
        ranked.iter().find(|t| t.tag == tag).unwrap()
    }

    #[test]
    fn repeated_tag_in_one_post_counts_once() {
        let content = "#x ".repeat(3000);
        let ranked = rank(&[("a", &content, NOW)]);
        let x = find(&ranked, "x");
        assert_eq!(x.post_count, 1);
        assert_eq!(x.unique_authors, 1);
        assert!((x.score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn many_posts_by_one_author_rank_below_two_authors() {
        let mut posts: Vec<(String, String, i64)> = (0..500)
            .map(|i| ("spammer".to_string(), "#spam".to_string(), NOW - i))
            .collect();
        posts.push(("a".to_string(), "#real".to_string(), NOW));
        posts.push(("b".to_string(), "#real".to_string(), NOW));
        let borrowed: Vec<(&str, &str, i64)> = posts
            .iter()
            .map(|(a, c, t)| (a.as_str(), c.as_str(), *t))
            .collect();
        let ranked = rank(&borrowed);

        assert_eq!(ranked.first().map(|t| t.tag.as_str()), Some("real"));
        let spam = find(&ranked, "spam");
        assert_eq!(spam.post_count, 500);
        assert_eq!(spam.unique_authors, 1);
        assert!(spam.score <= 1.0);
        let real = find(&ranked, "real");
        assert_eq!(real.unique_authors, 2);
        assert!((real.score - 2.0).abs() < 1e-9);
    }

    #[test]
    fn older_use_scores_lower() {
        let hour = 3_600_000;
        let ranked = rank(&[("a", "#old", NOW - 3 * hour), ("b", "#new", NOW)]);
        assert!((find(&ranked, "old").score - 0.25).abs() < 1e-9);
        assert!((find(&ranked, "new").score - 1.0).abs() < 1e-9);
        assert_eq!(find(&ranked, "old").latest_post_at, NOW - 3 * hour);
    }

    #[test]
    fn tags_are_case_folded_and_capped() {
        let ranked = rank(&[("a", "#Rust", NOW), ("b", "#rust", NOW)]);
        assert_eq!(ranked.len(), 1);
        assert_eq!(find(&ranked, "rust").unique_authors, 2);

        let many: String = (0..200).map(|i| format!("#t{i} ")).collect();
        let mut stats = TrendingStats::default();
        stats.add_post("a", &many, NOW);
        // At most MAX_HASHTAGS_PER_POST tags of one post are counted.
        assert_eq!(stats.tags.len(), crate::content::MAX_HASHTAGS_PER_POST);

        let mut stats = TrendingStats::default();
        for i in 0..(MAX_TRENDING_TAGS + 20) {
            stats.add_post("a", &format!("#tag{i}"), NOW);
        }
        assert_eq!(stats.into_ranked(NOW).len(), MAX_TRENDING_TAGS);
    }
}
