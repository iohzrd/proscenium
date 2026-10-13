use super::{AppState, clamp_limit, clamp_offset, internal_error};
use crate::content::fts_query;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// Maximum number of interactions returned for one post.
const MAX_INTERACTIONS: i64 = 500;

#[derive(Deserialize)]
struct UserPostsParams {
    limit: Option<i64>,
    before: Option<i64>,
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct PostsResponse {
    posts: Vec<crate::storage::StoredPost>,
}

#[derive(Serialize)]
struct PostSearchResponse {
    posts: Vec<crate::storage::StoredPost>,
    total: i64,
    query: String,
}

#[derive(Serialize)]
struct InteractionsResponse {
    interactions: Vec<crate::storage::StoredInteraction>,
    like_count: i64,
}

/// Posts of one user. Empty unless the user has an active public registration.
async fn get_user_posts(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
    Query(params): Query<UserPostsParams>,
) -> Result<Json<PostsResponse>, StatusCode> {
    let limit = clamp_limit(params.limit, 50, 200);
    let posts = state
        .storage
        .get_user_posts(&pubkey, limit, params.before)
        .await
        .map_err(internal_error)?;

    Ok(Json(PostsResponse { posts }))
}

/// Full-text search. Responds 400 if `q` has no searchable term or exceeds the limits
/// of `content::fts_query`.
async fn search_posts(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<PostSearchResponse>, StatusCode> {
    let limit = clamp_limit(params.limit, 20, 100);
    let offset = clamp_offset(params.offset);
    let query = fts_query(&params.q).ok_or(StatusCode::BAD_REQUEST)?;

    let (posts, total) = state
        .storage
        .search_posts(&query, limit, offset)
        .await
        .map_err(internal_error)?;

    Ok(Json(PostSearchResponse {
        posts,
        total,
        query: params.q,
    }))
}

/// Interactions on one post. Responds 404 unless the post is stored and its author has
/// an active public registration.
async fn get_post_interactions(
    State(state): State<AppState>,
    Path((author, post_id)): Path<(String, String)>,
) -> Result<Json<InteractionsResponse>, StatusCode> {
    let (interactions, like_count) = state
        .storage
        .get_post_interactions(&author, &post_id, MAX_INTERACTIONS)
        .await
        .map_err(internal_error)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(InteractionsResponse {
        interactions,
        like_count,
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/users/{pubkey}/posts", get(get_user_posts))
        .route("/api/v1/posts/search", get(search_posts))
        .route(
            "/api/v1/posts/{author}/{post_id}/interactions",
            get(get_post_interactions),
        )
}
