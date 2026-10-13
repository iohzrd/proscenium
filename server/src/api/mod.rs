mod auth;
mod feed;
mod posts;
mod server_info;
mod trending;
mod users;

use crate::config::Config;
use crate::http::RateLimiter;
use crate::ingestion::IngestionManager;
use crate::storage::Storage;
use axum::Router;
use axum::http::header::{HeaderName, HeaderValue, X_CONTENT_TYPE_OPTIONS};
use axum::http::{Method, StatusCode};
use std::sync::Arc;
use std::time::Duration;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Time a request may take from the end of its header to the start of the response,
/// including the wait for a free request slot. After it the response is 408.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct AppState {
    pub storage: Arc<Storage>,
    pub config: Arc<Config>,
    pub ingestion: Arc<IngestionManager>,
    pub start_time: std::time::Instant,
    /// Request counter per client address for `/api/v1/register`.
    pub register_limiter: Arc<RateLimiter>,
}

/// Clamp a `limit` query parameter to `1..=max`; `default` if absent.
pub fn clamp_limit(limit: Option<i64>, default: i64, max: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, max)
}

/// An `offset` query parameter, at least 0.
pub fn clamp_offset(offset: Option<i64>) -> i64 {
    offset.unwrap_or(0).max(0)
}

/// Log a storage error and return status 500 without the error text.
pub fn internal_error(e: anyhow::Error) -> StatusCode {
    tracing::error!("[api] storage error: {e}");
    StatusCode::INTERNAL_SERVER_ERROR
}

pub fn build_router(state: AppState) -> Router {
    // Read-only JSON routes: any origin may read them.
    let read_api = Router::new()
        .merge(server_info::routes())
        .merge(users::routes())
        .merge(posts::routes())
        .merge(feed::routes())
        .merge(trending::routes())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET]),
        );

    // The registration routes send no CORS headers, so a browser does not deliver a
    // cross-origin POST, PUT or DELETE with a JSON body to them.
    Router::new()
        .merge(read_api)
        .merge(auth::routes(state.clone()))
        .merge(crate::web::routes())
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(GlobalConcurrencyLimitLayer::new(
            state.config.limits.max_concurrent_requests,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Response headers for HTML pages: no script, frame, form or external resource is
/// allowed; styles come only from the inline `<style>` element.
pub fn html_security_headers() -> [(HeaderName, HeaderValue); 3] {
    [
        (
            axum::http::header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; \
                 form-action 'none'; frame-ancestors 'none'",
            ),
        ),
        (
            axum::http::header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ),
        (
            axum::http::header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_and_offset_are_clamped() {
        assert_eq!(clamp_limit(None, 50, 200), 50);
        assert_eq!(clamp_limit(Some(-1), 50, 200), 1);
        assert_eq!(clamp_limit(Some(0), 50, 200), 1);
        assert_eq!(clamp_limit(Some(i64::MAX), 50, 200), 200);
        assert_eq!(clamp_limit(Some(7), 50, 200), 7);
        assert_eq!(clamp_offset(None), 0);
        assert_eq!(clamp_offset(Some(-5)), 0);
        assert_eq!(clamp_offset(Some(40)), 40);
    }
}
