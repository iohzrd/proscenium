mod layout;
mod pages;

use crate::api::{AppState, html_security_headers};
use axum::Router;
use tower_http::set_header::SetResponseHeaderLayer;

/// HTML page routes. Every response carries the headers of `html_security_headers`.
pub fn routes() -> Router<AppState> {
    let mut router = pages::routes();
    for (name, value) in html_security_headers() {
        router = router.layer(SetResponseHeaderLayer::overriding(name, value));
    }
    router
}
