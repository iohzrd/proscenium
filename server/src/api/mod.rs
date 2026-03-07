mod auth;
mod feed;
mod posts;
mod server_info;
mod trending;
mod users;

use crate::config::Config;
use crate::ingestion::IngestionManager;
use crate::storage::Storage;
use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<Storage>,
    pub config: Arc<Config>,
    pub ingestion: Arc<IngestionManager>,
    pub start_time: std::time::Instant,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(server_info::routes())
        .merge(auth::routes())
        .merge(users::routes())
        .merge(posts::routes())
        .merge(feed::routes())
        .merge(trending::routes())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
