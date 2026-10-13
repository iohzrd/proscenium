use super::{AppState, clamp_limit, internal_error};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use proscenium_types::is_valid_pubkey;
use serde::{Deserialize, Serialize};

/// Maximum number of pubkeys in the `authors` query parameter.
const MAX_FEED_AUTHORS: usize = 100;

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

/// Split a comma-separated `authors` parameter. `None` if an entry is not a canonical
/// pubkey or there are more than `MAX_FEED_AUTHORS` entries.
fn parse_authors(authors: &str) -> Option<Vec<String>> {
    let authors: Vec<String> = authors
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (authors.len() <= MAX_FEED_AUTHORS && authors.iter().all(|a| is_valid_pubkey(a)))
        .then_some(authors)
}

async fn get_feed(
    State(state): State<AppState>,
    Query(params): Query<FeedParams>,
) -> Result<Json<FeedResponse>, StatusCode> {
    let limit = clamp_limit(params.limit, 50, 200);
    let authors = match params.authors.as_deref() {
        Some(authors) => Some(parse_authors(authors).ok_or(StatusCode::BAD_REQUEST)?),
        None => None,
    };

    let posts = state
        .storage
        .get_feed(limit, params.before, authors.as_deref())
        .await
        .map_err(internal_error)?;

    Ok(Json(FeedResponse { posts }))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/feed", get(get_feed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authors_parameter_is_validated_and_bounded() {
        let key = iroh::SecretKey::from_bytes(&[3u8; 32]).public().to_string();
        assert_eq!(
            parse_authors(&format!("{key}, {key},")),
            Some(vec![key.clone(), key.clone()])
        );
        assert_eq!(parse_authors(""), Some(vec![]));
        assert_eq!(parse_authors("not-a-key"), None);
        let many = vec![key; MAX_FEED_AUTHORS + 1].join(",");
        assert_eq!(parse_authors(&many), None);
    }
}
