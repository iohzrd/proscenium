use super::layout;
use crate::api::AppState;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::routing::get;
use maud::html;
use serde::Deserialize;

use crate::storage::{PostStats, StoredPost, UserInfo};

// --- Helpers ---

fn render_post(post: &StoredPost, author_info: Option<&UserInfo>, stats: Option<&PostStats>) -> maud::Markup {
    let display_name = author_info
        .and_then(|u| u.display_name.as_deref())
        .filter(|n| !n.is_empty());
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
            @if let Some(ref reply_to) = post.reply_to {
                p.post-reply-context {
                    "Replying to "
                    a href=(format!("/post/{}/{}", post.reply_to_author.as_deref().unwrap_or(""), reply_to)) {
                        "a post"
                    }
                }
            }
            @if let Some(ref quote_of) = post.quote_of {
                p.post-reply-context {
                    @if post.content.is_empty() {
                        "Reposted "
                    } @else {
                        "Quoting "
                    }
                    a href=(format!("/post/{}/{}", post.quote_of_author.as_deref().unwrap_or(""), quote_of)) {
                        "a post"
                    }
                }
            }
            @if !post.content.is_empty() {
                p.post-content { (layout::render_content(&post.content)) }
            }
            @if let Some(s) = stats {
                @if s.likes > 0 || s.reposts > 0 || s.replies > 0 || s.quotes > 0 {
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
        .get_feed(50, params.before, None)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let author_map = build_author_map(&state, &posts).await;
    let stats_map = build_stats_map(&state, &posts).await;
    let server_name = &state.config.server.name;
    let oldest_ts = posts.last().map(|p| p.timestamp);

    let content = html! {
        h1.section-title { "Latest Posts" }
        @if posts.is_empty() {
            p.empty { "No posts yet. Be the first!" }
        } @else {
            @for post in &posts {
                (render_post(post, author_map.get(post.author.as_str()).and_then(|o| o.as_ref()), stats_map.get(&post.id)))
            }
            @if let Some(ts) = oldest_ts {
                a.load-more href=(format!("/?before={ts}")) { "Load older posts" }
            }
        }
    };

    Ok(Html(
        layout::page("Feed", server_name, content).into_string(),
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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let posts = if user.visibility == "public" {
        state
            .storage
            .get_user_posts(&pubkey, 50, params.before)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        vec![]
    };

    let stats_map = build_stats_map(&state, &posts).await;
    let oldest_ts = posts.last().map(|p| p.timestamp);
    let server_name = &state.config.server.name;
    let display = user
        .display_name
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or("Anonymous");

    let content = html! {
        div.profile-header {
            h1.profile-name { (display) }
            p.profile-pubkey { (pubkey) }
            @if let Some(ref bio) = user.bio {
                @if !bio.is_empty() {
                    p.profile-bio { (bio) }
                }
            }
            div.profile-stats {
                @if user.visibility == "public" {
                    span { strong { (user.post_count) } " posts" }
                }
                span { "Joined " (layout::format_time(user.registered_at)) }
            }
        }
        @if user.visibility != "public" {
            p.empty { "This user's posts are not publicly visible." }
        } @else if posts.is_empty() {
            p.empty { "No posts yet." }
        } @else {
            @for post in &posts {
                (render_post(post, Some(&user), stats_map.get(&post.id)))
            }
            @if let Some(ts) = oldest_ts {
                a.load-more href=(format!("/user/{}?before={ts}", pubkey)) { "Load older posts" }
            }
        }
    };

    Ok(Html(
        layout::page(&format!("{display}'s profile"), server_name, content).into_string(),
    ))
}

// --- Single post ---

async fn post_page(
    State(state): State<AppState>,
    Path((author, post_id)): Path<(String, String)>,
) -> Result<Html<String>, StatusCode> {
    let post = state
        .storage
        .get_post(&author, &post_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let user = state
        .storage
        .get_user_info(&author)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let server_name = &state.config.server.name;
    let display_name = user
        .as_ref()
        .and_then(|u| u.display_name.as_deref())
        .filter(|n| !n.is_empty())
        .unwrap_or("Anonymous");

    let post_stats = build_stats_map(&state, std::slice::from_ref(&post)).await;

    let content = html! {
        (render_post(&post, user.as_ref(), post_stats.get(&post.id)))
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

async fn trending_page(
    State(state): State<AppState>,
    Query(params): Query<TrendingParams>,
) -> Result<Html<String>, StatusCode> {
    let server_name = &state.config.server.name;

    if let Some(ref tag) = params.tag {
        let query = format!("#{tag}");
        let (posts, _total) = state
            .storage
            .search_posts(&query, 50, 0)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let author_map = build_author_map(&state, &posts).await;
        let stats_map = build_stats_map(&state, &posts).await;

        let content = html! {
            h1.section-title { "#" (tag) }
            @if posts.is_empty() {
                p.empty { "No posts found with this tag." }
            } @else {
                @for post in &posts {
                    (render_post(post, author_map.get(post.author.as_str()).and_then(|o| o.as_ref()), stats_map.get(&post.id)))
                }
            }
        };

        return Ok(Html(
            layout::page(&format!("#{tag}"), server_name, content).into_string(),
        ));
    }

    let tags = state
        .storage
        .get_trending_hashtags(30)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let content = html! {
        h1.section-title { "Trending" }
        @if tags.is_empty() {
            p.empty { "No trending topics yet." }
        } @else {
            ul.tag-list {
                @for tag in &tags {
                    li.tag-item {
                        a.tag-name href=(format!("/trending?tag={}", tag.tag)) {
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
        layout::page("Trending", server_name, content).into_string(),
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
    let offset = params.offset.unwrap_or(0);
    let limit = 50;
    let (users, total) = state
        .storage
        .list_users(limit, offset)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let server_name = &state.config.server.name;
    let next_offset = offset + limit;
    let has_more = next_offset < total;

    let content = html! {
        h1.section-title { "Users (" (total) ")" }
        @if users.is_empty() {
            p.empty { "No registered users yet." }
        } @else {
            @for user in &users {
                a.user-card href=(format!("/user/{}", user.pubkey)) {
                    div.user-name {
                        @if let Some(ref name) = user.display_name {
                            @if !name.is_empty() {
                                (name)
                            } @else {
                                (layout::short_pubkey(&user.pubkey))
                            }
                        } @else {
                            (layout::short_pubkey(&user.pubkey))
                        }
                    }
                    @if let Some(ref bio) = user.bio {
                        @if !bio.is_empty() {
                            p.user-bio { (bio) }
                        }
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

// --- Utilities ---

async fn build_author_map(
    state: &AppState,
    posts: &[StoredPost],
) -> std::collections::HashMap<String, Option<UserInfo>> {
    let mut map = std::collections::HashMap::new();
    for post in posts {
        if map.contains_key(&post.author) {
            continue;
        }
        let info = state
            .storage
            .get_user_info(&post.author)
            .await
            .ok()
            .flatten();
        map.insert(post.author.clone(), info);
    }
    map
}

async fn build_stats_map(
    state: &AppState,
    posts: &[StoredPost],
) -> std::collections::HashMap<String, PostStats> {
    let post_ids: Vec<(String, String)> = posts
        .iter()
        .map(|p| (p.author.clone(), p.id.clone()))
        .collect();
    state
        .storage
        .get_post_stats_batch(&post_ids)
        .await
        .unwrap_or_default()
}
