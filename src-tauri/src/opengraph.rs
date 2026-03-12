use iroh_social_types::LinkPreview;
use regex::Regex;
use scraper::{Html, Selector};
use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>"')\]]+"#).unwrap());

const MAX_PREVIEWS: usize = 3;
const MAX_CACHE_ENTRIES: usize = 512;
const MAX_BODY_BYTES: usize = 512 * 1024;
const CACHE_TTL_SECS: u64 = 3600; // 1 hour

struct CacheEntry {
    value: Option<LinkPreview>,
    inserted_at: u64,
}

pub struct OgCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

impl OgCache {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get_link_preview(
        &self,
        client: &reqwest::Client,
        url: &str,
    ) -> Option<LinkPreview> {
        let now = iroh_social_types::now_millis() / 1000;

        // Check cache
        {
            let cache = self.entries.lock().await;
            if let Some(entry) = cache.get(url)
                && now - entry.inserted_at < CACHE_TTL_SECS
            {
                return entry.value.clone();
            }
        }

        let result = fetch_og(client, url).await;

        // Cache the result (including None to avoid re-fetching failures)
        let mut cache = self.entries.lock().await;

        // Evict expired entries first, then oldest half if still over capacity
        cache.retain(|_, entry| now - entry.inserted_at < CACHE_TTL_SECS);
        if cache.len() >= MAX_CACHE_ENTRIES {
            let evict: Vec<String> = cache.keys().take(MAX_CACHE_ENTRIES / 2).cloned().collect();
            for k in evict {
                cache.remove(&k);
            }
        }

        cache.insert(
            url.to_string(),
            CacheEntry {
                value: result.clone(),
                inserted_at: now,
            },
        );

        result
    }
}

pub fn extract_urls(content: &str) -> Vec<String> {
    URL_RE
        .find_iter(content)
        .map(|m| m.as_str().to_string())
        .take(MAX_PREVIEWS)
        .collect()
}

async fn fetch_og(client: &reqwest::Client, url: &str) -> Option<LinkPreview> {
    let resp = client.get(url).send().await.ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.contains("text/html") {
        return None;
    }

    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > MAX_BODY_BYTES {
        return None;
    }

    let html = String::from_utf8_lossy(&bytes);
    let doc = Html::parse_document(&html);

    let og_title = select_meta_property(&doc, "og:title");
    let og_desc = select_meta_property(&doc, "og:description");
    let og_image = select_meta_property(&doc, "og:image");
    let og_site = select_meta_property(&doc, "og:site_name");

    let title = og_title.or_else(|| select_title(&doc));
    let description = og_desc.or_else(|| select_meta_name(&doc, "description"));

    if title.is_none() && description.is_none() && og_image.is_none() {
        return None;
    }

    Some(LinkPreview {
        url: url.to_string(),
        title,
        description,
        image: og_image,
        site_name: og_site,
    })
}

fn select_meta_property(doc: &Html, property: &str) -> Option<String> {
    let selector = Selector::parse(&format!("meta[property=\"{property}\"]")).ok()?;
    doc.select(&selector)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| s.to_string())
}

fn select_meta_name(doc: &Html, name: &str) -> Option<String> {
    let selector = Selector::parse(&format!("meta[name=\"{name}\"]")).ok()?;
    doc.select(&selector)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| s.to_string())
}

fn select_title(doc: &Html) -> Option<String> {
    let selector = Selector::parse("title").ok()?;
    doc.select(&selector)
        .next()
        .map(|el| el.text().collect::<String>())
        .filter(|s| !s.is_empty())
}
