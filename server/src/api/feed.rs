use super::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct FeedParams {
    limit: Option<i64>,
    before: Option<i64>,
    authors: Option<String>,
}

#[derive(Serialize)]
struct FeedResponse {
    posts: Vec<crate::storage::StoredPost>,
}

async fn get_feed(
    State(state): State<AppState>,
    Query(params): Query<FeedParams>,
) -> Result<Json<FeedResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(50).min(200);
    let authors: Option<Vec<String>> = params
        .authors
        .map(|a| a.split(',').map(|s| s.trim().to_string()).collect());

    let posts = state
        .storage
        .get_feed(limit, params.before, authors.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(FeedResponse { posts }))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/feed", get(get_feed))
}
