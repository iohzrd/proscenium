use super::layout;
use crate::api::{AppState, clamp_offset, internal_error};
use crate::content::{extract_hashtags, fts_query, normalize_hashtag};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use maud::html;
use serde::Deserialize;
use std::collections::HashMap;

use crate::storage::{PostStats, StoredPost};

/// Posts per page on the feed, profile and hashtag pages.
const PAGE_POSTS: i64 = 50;
/// Users per page on the user list.
const PAGE_USERS: i64 = 50;
/// Full-text matches read for one hashtag page before posts without the hashtag are
/// filtered out.
const HASHTAG_CANDIDATES: i64 = 200;

type StatsMap = HashMap<(String, String), PostStats>;

// --- Helpers ---

fn render_post(
    post: &StoredPost,
    display_name: Option<&str>,
    stats: Option<&PostStats>,
) -> maud::Markup {
    let short_key = layout::short_pubkey(&post.author);

    html! {
        article.post {
            div.post-header {
                a.post-author href=(format!("/user/{}", post.author)) {
                    @if let Some(name) = display_name {
                        (name)
                    } @else {
                        (short_key)
                    }
                }
                @if display_name.is_some() {
                    span.post-pubkey { (short_key) }
                }
                a.post-time href=(format!("/post/{}/{}", post.author, post.id)) {
                    (layout::format_time(post.timestamp))
                }
            }
            @if let (Some(reply_to), Some(reply_to_author)) = (&post.reply_to, &post.reply_to_author) {
                p.post-reply-context {
                    "Replying to "
                    a href=(format!("/post/{reply_to_author}/{reply_to}")) {
                        "a post"
                    }
                }
            }
            @if let (Some(quote_of), Some(quote_of_author)) = (&post.quote_of, &post.quote_of_author) {
                p.post-reply-context {
                    @if post.content.is_empty() {
                        "Reposted "
                    } @else {
                        "Quoting "
                    }
                    a href=(format!("/post/{quote_of_author}/{quote_of}")) {
                        "a post"
                    }
                }
            }
            @if !post.content.is_empty() {
                p.post-content { (layout::render_content(&post.content)) }
            }
            @if let Some(s) = stats {
                @if !s.is_empty() {
                    div.post-stats {
                        @if s.replies > 0 {
                            span.stat { (s.replies) " replies" }
                        }
                        @if s.quotes > 0 {
                            span.stat { (s.quotes) " quotes" }
                        }
                        @if s.reposts > 0 {
                            span.stat { (s.reposts) " reposts" }
                        }
                        @if s.likes > 0 {
                            span.stat { (s.likes) " likes" }
                        }
                    }
                }
            }
        }
    }
}

fn render_posts(
    posts: &[StoredPost],
    names: &HashMap<String, String>,
    stats: &StatsMap,
) -> maud::Markup {
    html! {
        @for post in posts {
            (render_post(
                post,
                names.get(&post.author).map(String::as_str),
                stats.get(&(post.author.clone(), post.id.clone())),
            ))
        }
    }
}

/// Display names of the authors of `posts`, in one query.
async fn load_display_names(
    state: &AppState,
    posts: &[StoredPost],
) -> Result<HashMap<String, String>, StatusCode> {
    let mut authors: Vec<&str> = posts.iter().map(|p| p.author.as_str()).collect();
    authors.sort_unstable();
    authors.dedup();
    state
        .storage
        .get_display_names(&authors)
        .await
        .map_err(internal_error)
}

/// Like, repost, reply and quote counts of `posts`, in one query.
async fn load_stats(state: &AppState, posts: &[StoredPost]) -> Result<StatsMap, StatusCode> {
    let keys: Vec<(&str, &str)> = posts
        .iter()
        .map(|p| (p.author.as_str(), p.id.as_str()))
        .collect();
    state
        .storage
        .get_post_stats_batch(&keys)
        .await
        .map_err(internal_error)
}

// --- Feed page ---

#[derive(Deserialize)]
struct FeedParams {
    before: Option<i64>,
}

async fn feed_page(
    State(state): State<AppState>,
    Query(params): Query<FeedParams>,
) -> Result<Html<String>, StatusCode> {
    let posts = state
        .storage
        .get_feed(PAGE_POSTS, params.before, None)
        .await
        .map_err(internal_error)?;

    let names = load_display_names(&state, &posts).await?;
    let stats = load_stats(&state, &posts).await?;
    let server_name = &state.config.server.name;
    let oldest_ts = posts.last().map(|p| p.timestamp);

    let content = html! {
        h1.section-title { "Latest Posts" }
        @if posts.is_empty() {
            p.empty { "No posts yet. Be the first!" }
        } @else {
            (render_posts(&posts, &names, &stats))
            @if let Some(ts) = oldest_ts {
                a.load-more href=(format!("/?before={ts}")) { "Load older posts" }
            }
        }
    };

    Ok(Html(
        layout::page("Arc", server_name, content).into_string(),
    ))
}

// --- User profile ---

async fn user_page(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
    Query(params): Query<FeedParams>,
) -> Result<Html<String>, StatusCode> {
    let user = state
        .storage
        .get_user_info(&pubkey)
        .await
        .map_err(internal_error)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Empty unless the registration is public.
    let posts = state
        .storage
        .get_user_posts(&user.pubkey, PAGE_POSTS, params.before)
        .await
        .map_err(internal_error)?;

    let stats = load_stats(&state, &posts).await?;
    let oldest_ts = posts.last().map(|p| p.timestamp);
    let server_name = &state.config.server.name;
    let display_name = user.display_name.as_deref().filter(|n| !n.is_empty());
    let display = display_name.unwrap_or("Anonymous");
    let is_public = user.visibility == "public";

    let content = html! {
        div.profile-header {
            h1.profile-name { (display) }
            p.profile-pubkey { (user.pubkey) }
            @if let Some(bio) = user.bio.as_deref().filter(|b| !b.is_empty()) {
                p.profile-bio { (bio) }
            }
            div.profile-stats {
                @if is_public {
                    span { strong { (user.post_count) } " posts" }
                }
                span { "Joined " (layout::format_time(user.registered_at)) }
            }
        }
        @if !is_public {
            p.empty { "This user's posts are not publicly visible." }
        } @else if posts.is_empty() {
            p.empty { "No posts yet." }
        } @else {
            @for post in &posts {
                (render_post(
                    post,
                    display_name,
                    stats.get(&(post.author.clone(), post.id.clone())),
                ))
            }
            @if let Some(ts) = oldest_ts {
                a.load-more href=(format!("/user/{}?before={ts}", user.pubkey)) { "Load older posts" }
            }
        }
    };

    Ok(Html(
        layout::page(&format!("{display}'s profile"), server_name, content).into_string(),
    ))
}

// --- Single post ---

/// One post. Responds 404 unless the post is stored and its author has an active
/// public registration.
async fn post_page(
    State(state): State<AppState>,
    Path((author, post_id)): Path<(String, String)>,
) -> Result<Html<String>, StatusCode> {
    let post = state
        .storage
        .get_post(&author, &post_id)
        .await
        .map_err(internal_error)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let posts = std::slice::from_ref(&post);
    let names = load_display_names(&state, posts).await?;
    let stats = load_stats(&state, posts).await?;
    let server_name = &state.config.server.name;
    let display_name = names.get(&post.author).map_or("Anonymous", String::as_str);

    let content = html! {
        (render_posts(posts, &names, &stats))
        div.post-meta {
            span { "Posted " (layout::format_time(post.timestamp)) }
        }
    };

    Ok(Html(
        layout::page(&format!("Post by {display_name}"), server_name, content).into_string(),
    ))
}

// --- Trending ---

#[derive(Deserialize)]
struct TrendingParams {
    tag: Option<String>,
}

/// Posts that contain the hashtag `tag`. The full-text index is queried for the tag
/// name as a phrase; matches that contain the words but not the hashtag are removed.
/// Responds 400 if `tag` is not a hashtag name.
async fn tag_page(state: &AppState, tag: &str) -> Result<Html<String>, StatusCode> {
    let tag = normalize_hashtag(tag).ok_or(StatusCode::BAD_REQUEST)?;
    let query = fts_query(&tag).ok_or(StatusCode::BAD_REQUEST)?;
    let (candidates, _total) = state
        .storage
        .search_posts(&query, HASHTAG_CANDIDATES, 0)
        .await
        .map_err(internal_error)?;
    let posts: Vec<StoredPost> = candidates
        .into_iter()
        .filter(|post| extract_hashtags(&post.content).contains(&tag))
        .take(PAGE_POSTS as usize)
        .collect();

    let names = load_display_names(state, &posts).await?;
    let stats = load_stats(state, &posts).await?;

    let content = html! {
        h1.section-title { "#" (tag) }
        @if posts.is_empty() {
            p.empty { "No posts found with this tag." }
        } @else {
            (render_posts(&posts, &names, &stats))
        }
    };

    Ok(Html(
        layout::page(&format!("#{tag}"), &state.config.server.name, content).into_string(),
    ))
}

async fn trending_page(
    State(state): State<AppState>,
    Query(params): Query<TrendingParams>,
) -> Result<Html<String>, StatusCode> {
    if let Some(tag) = &params.tag {
        return tag_page(&state, tag).await;
    }

    let tags = state
        .storage
        .get_trending_hashtags(30)
        .await
        .map_err(internal_error)?;

    let content = html! {
        h1.section-title { "Trending" }
        @if tags.is_empty() {
            p.empty { "No trending topics yet." }
        } @else {
            ul.tag-list {
                @for tag in &tags {
                    li.tag-item {
                        a.tag-name href=(layout::tag_href(&tag.tag)) {
                            "#" (tag.tag)
                        }
                        span.tag-stats {
                            (tag.post_count) " posts by " (tag.unique_authors) " users"
                        }
                    }
                }
            }
        }
    };

    Ok(Html(
        layout::page("Trending", &state.config.server.name, content).into_string(),
    ))
}

// --- Users list ---

#[derive(Deserialize)]
struct UsersParams {
    offset: Option<i64>,
}

async fn users_page(
    State(state): State<AppState>,
    Query(params): Query<UsersParams>,
) -> Result<Html<String>, StatusCode> {
    let offset = clamp_offset(params.offset);
    let (users, total) = state
        .storage
        .list_users(PAGE_USERS, offset)
        .await
        .map_err(internal_error)?;

    let server_name = &state.config.server.name;
    let next_offset = offset.saturating_add(PAGE_USERS);
    let has_more = next_offset < total;

    let content = html! {
        h1.section-title { "Users (" (total) ")" }
        @if users.is_empty() {
            p.empty { "No registered users yet." }
        } @else {
            @for user in &users {
                a.user-card href=(format!("/user/{}", user.pubkey)) {
                    div.user-name {
                        @if let Some(name) = user.display_name.as_deref().filter(|n| !n.is_empty()) {
                            (name)
                        } @else {
                            (layout::short_pubkey(&user.pubkey))
                        }
                    }
                    @if let Some(bio) = user.bio.as_deref().filter(|b| !b.is_empty()) {
                        p.user-bio { (bio) }
                    }
                    p.user-meta {
                        @if user.visibility == "public" {
                            (user.post_count) " posts"
                        } @else {
                            "Listed"
                        }
                    }
                }
            }
            @if has_more {
                a.load-more href=(format!("/users?offset={next_offset}")) { "Load more" }
            }
        }
    };

    Ok(Html(
        layout::page("Users", server_name, content).into_string(),
    ))
}

// --- Route builder ---

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(feed_page))
        .route("/user/{pubkey}", get(user_page))
        .route("/post/{author}/{id}", get(post_page))
        .route("/trending", get(trending_page))
        .route("/users", get(users_page))
}
