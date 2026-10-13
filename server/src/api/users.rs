use super::{AppState, clamp_limit, clamp_offset, internal_error};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

/// Maximum byte length of a user search query.
const MAX_USER_QUERY_LEN: usize = 200;

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
    let limit = clamp_limit(params.limit, 20, 100);
    let offset = clamp_offset(params.offset);

    let (users, total) = state
        .storage
        .list_users(limit, offset)
        .await
        .map_err(internal_error)?;

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
    let limit = clamp_limit(params.limit, 20, 100);
    if params.q.len() > MAX_USER_QUERY_LEN {
        return Err(StatusCode::BAD_REQUEST);
    }

    let users = state
        .storage
        .search_users(&params.q, limit)
        .await
        .map_err(internal_error)?;

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
        .map_err(internal_error)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(user))
}

#[derive(Serialize)]
struct DevicesResponse {
    master_pubkey: String,
    transport_node_ids: Vec<String>,
}

/// Transport NodeIds of a user's devices. Responds 404 unless the user has an active
/// registration.
async fn get_devices(
    State(state): State<AppState>,
    Path(pubkey): Path<String>,
) -> Result<Json<DevicesResponse>, StatusCode> {
    let transport_node_ids = state
        .storage
        .get_device_node_ids(&pubkey)
        .await
        .map_err(internal_error)?
        .ok_or(StatusCode::NOT_FOUND)?;

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
