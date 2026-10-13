use super::{AppState, clamp_limit, internal_error};
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
    let limit = clamp_limit(params.limit, 10, 50);

    let hashtags = state
        .storage
        .get_trending_hashtags(limit)
        .await
        .map_err(internal_error)?;

    let computed_at = hashtags.first().map(|t| t.computed_at);

    Ok(Json(TrendingResponse {
        hashtags,
        computed_at,
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/trending", get(get_trending))
}
