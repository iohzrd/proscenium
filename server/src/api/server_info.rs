use super::AppState;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

#[derive(Serialize)]
struct ServerInfo {
    name: String,
    description: String,
    version: String,
    node_id: String,
    registered_users: i64,
    total_posts: i64,
    uptime_seconds: u64,
    registration_open: bool,
}

async fn info(State(state): State<AppState>) -> Json<ServerInfo> {
    let registered_users = state.storage.registration_count().await.unwrap_or(0);
    let total_posts = state.storage.total_post_count().await.unwrap_or(0);
    let uptime = state.start_time.elapsed().as_secs();

    Json(ServerInfo {
        name: state.config.server.name.clone(),
        description: state.config.server.description.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        node_id: state.ingestion.endpoint.id().to_string(),
        registered_users,
        total_posts,
        uptime_seconds: uptime,
        registration_open: state.config.server.registration_open,
    })
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/info", get(info))
}
