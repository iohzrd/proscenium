use super::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct ListParams {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct UserListResponse {
    users: Vec<crate::storage::UserInfo>,
    total: i64,
    limit: i64,
    offset: i64,
}

#[derive(Serialize)]
struct UserSearchResponse {
    users: Vec<crate::storage::UserInfo>,
    total: usize,
    query: String,
}

async fn list_users(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<UserListResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(20).min(100);
    let offset = params.offset.unwrap_or(0);

    let (users, total) = state
        .storage
        .list_users(limit, offset)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(UserListResponse {
        users,
        total,
        limit,
        offset,
    }))
}

async fn search_users(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<UserSearchResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(20).min(100);

    let users = state
        .storage
        .search_users(&params.q, limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total = users.len();
    Ok(Json(UserSearchResponse {
        users,
        total,
        query: params.q,
    }))
}

async fn get_user(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> Result<Json<crate::storage::UserInfo>, StatusCode> {
    let user = state
        .storage
        .get_user_info(&pubkey)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(user))
}

#[derive(Serialize)]
struct DevicesResponse {
    master_pubkey: String,
    transport_node_ids: Vec<String>,
}

async fn get_devices(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> Result<Json<DevicesResponse>, StatusCode> {
    let transport_node_id = state
        .storage
        .get_transport_node_id(&pubkey)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let transport_node_ids = transport_node_id.into_iter().collect();

    Ok(Json(DevicesResponse {
        master_pubkey: pubkey,
        transport_node_ids,
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/users", get(list_users))
        .route("/api/v1/users/search", get(search_users))
        .route("/api/v1/users/{pubkey}", get(get_user))
        .route("/api/v1/users/{pubkey}/devices", get(get_devices))
}
