mod layout;
mod pages;

use crate::api::AppState;
use axum::Router;

pub fn routes() -> Router<AppState> {
    pages::routes()
}
