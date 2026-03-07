use super::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

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

async fn get_user_posts(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
    Query(params): Query<UserPostsParams>,
) -> Result<Json<PostsResponse>, StatusCode> {
    // Check if user is Listed (no posts stored)
    if let Ok(Some(reg)) = state.storage.get_registration(&pubkey).await
        && reg.visibility == "listed"
    {
        return Ok(Json(PostsResponse { posts: vec![] }));
    }

    let limit = params.limit.unwrap_or(50).min(200);
    let posts = state
        .storage
        .get_user_posts(&pubkey, limit, params.before)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(PostsResponse { posts }))
}

async fn search_posts(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<PostSearchResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(20).min(100);
    let offset = params.offset.unwrap_or(0);

    let (posts, total) = state
        .storage
        .search_posts(&params.q, limit, offset)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(PostSearchResponse {
        posts,
        total,
        query: params.q,
    }))
}

async fn get_post_interactions(
    State(state): State<AppState>,
    Path((author, post_id)): Path<(String, String)>,
) -> Result<Json<InteractionsResponse>, StatusCode> {
    let interactions = state
        .storage
        .get_post_interactions(&author, &post_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let like_count = state
        .storage
        .get_post_like_count(&author, &post_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
