use super::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct TrendingParams {
    limit: Option<i64>,
}

#[derive(Serialize)]
struct TrendingResponse {
    hashtags: Vec<crate::storage::TrendingHashtag>,
    computed_at: Option<i64>,
}

async fn get_trending(
    State(state): State<AppState>,
    Query(params): Query<TrendingParams>,
) -> Result<Json<TrendingResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(10).min(50);

    let hashtags = state
        .storage
        .get_trending_hashtags(limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let computed_at = hashtags.first().map(|t| t.computed_at);

    Ok(Json(TrendingResponse {
        hashtags,
        computed_at,
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/trending", get(get_trending))
}
