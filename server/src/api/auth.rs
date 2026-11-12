use super::AppState;
use crate::config::normalize_url;
use crate::http::client_ip;
use crate::storage::{ChangeError, RegistrationChange, Rejection};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use proscenium_types::{
    ProfileUpdateRequest, REGISTRATION_ACTION_UNREGISTER, RegistrationRequest, Visibility,
    is_recent_timestamp, verify_profile_update, verify_registration_signature,
};
use serde::Serialize;
use std::net::SocketAddr;
use std::time::Instant;

/// Largest accepted request body. A profile update with every field at its maximum
/// length and every character JSON-escaped is below this.
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024;

#[derive(Serialize)]
struct RegisterResponse {
    pubkey: String,
    registered_at: u64,
    message: String,
}

#[derive(Debug, Serialize)]
struct MessageResponse {
    message: String,
}

type ApiError = (StatusCode, Json<MessageResponse>);

fn reject(status: StatusCode, message: impl Into<String>) -> ApiError {
    (
        status,
        Json(MessageResponse {
            message: message.into(),
        }),
    )
}

/// Checks of the signed `timestamp` and `server_url` fields, shared by all three
/// methods: the timestamp is within `MAX_TIMESTAMP_DRIFT_MS` of the server clock, and
/// the URL names this server (compared without one trailing `/`).
fn check_timestamp_and_url(
    timestamp: u64,
    server_url: &str,
    public_url: &str,
) -> Result<(), ApiError> {
    if !is_recent_timestamp(timestamp) {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "timestamp too far from server time",
        ));
    }
    if normalize_url(server_url) != normalize_url(public_url) {
        return Err(reject(StatusCode::BAD_REQUEST, "server_url mismatch"));
    }
    Ok(())
}

/// Checks of a `POST /api/v1/register` body that need no storage read.
fn check_register_request(req: &RegistrationRequest, public_url: &str) -> Result<(), ApiError> {
    if req.action.is_some() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "action must be absent for registration; use DELETE to unregister",
        ));
    }
    verify_registration_signature(req)
        .map_err(|e| reject(StatusCode::BAD_REQUEST, format!("invalid signature: {e}")))?;
    check_timestamp_and_url(req.timestamp, &req.server_url, public_url)?;
    if req.visibility == Visibility::Private {
        return Err(reject(
            StatusCode::FORBIDDEN,
            Rejection::PrivateVisibility.to_string(),
        ));
    }
    Ok(())
}

/// Checks of a `DELETE /api/v1/register` body that need no storage read.
fn check_unregister_request(req: &RegistrationRequest, public_url: &str) -> Result<(), ApiError> {
    if req.action.as_deref() != Some(REGISTRATION_ACTION_UNREGISTER) {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "action must be 'unregister'",
        ));
    }
    verify_registration_signature(req)
        .map_err(|e| reject(StatusCode::BAD_REQUEST, format!("invalid signature: {e}")))?;
    check_timestamp_and_url(req.timestamp, &req.server_url, public_url)
}

/// Checks of a `PUT /api/v1/register` body that need no storage read.
fn check_profile_update_request(
    req: &ProfileUpdateRequest,
    public_url: &str,
) -> Result<(), ApiError> {
    verify_profile_update(req)
        .map_err(|e| reject(StatusCode::BAD_REQUEST, format!("invalid request: {e}")))?;
    check_timestamp_and_url(req.timestamp, &req.server_url, public_url)
}

/// Map a rejected or failed change to a response. Storage error text is logged and not
/// returned.
fn change_error(e: ChangeError) -> ApiError {
    match e {
        ChangeError::Rejected(rejection) => {
            let status = match rejection {
                Rejection::Replayed => StatusCode::CONFLICT,
                Rejection::NotRegistered => StatusCode::NOT_FOUND,
                Rejection::RegistrationClosed
                | Rejection::UserLimitReached
                | Rejection::PrivateVisibility => StatusCode::FORBIDDEN,
            };
            reject(status, rejection.to_string())
        }
        ChangeError::Storage(e) => {
            tracing::error!("[auth] storage error: {e}");
            reject(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegistrationRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), ApiError> {
    check_register_request(&req, &state.config.server.public_url)?;
    let outcome = state
        .ingestion
        .apply_registration_change(RegistrationChange::Register(&req))
        .await
        .map_err(change_error)?;

    let (status, message) = if outcome.was_active {
        (StatusCode::OK, "registration updated")
    } else {
        (StatusCode::CREATED, "registered successfully")
    };
    Ok((
        status,
        Json(RegisterResponse {
            pubkey: req.master_pubkey,
            registered_at: u64::try_from(outcome.registered_at).unwrap_or(0),
            message: message.to_string(),
        }),
    ))
}

async fn unregister(
    State(state): State<AppState>,
    Json(req): Json<RegistrationRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    check_unregister_request(&req, &state.config.server.public_url)?;
    let message = match state
        .ingestion
        .apply_registration_change(RegistrationChange::Unregister(&req))
        .await
    {
        Ok(_) => "unregistered successfully",
        // The requested end state already holds.
        Err(ChangeError::Rejected(Rejection::NotRegistered)) => "not registered",
        Err(e) => return Err(change_error(e)),
    };
    Ok(Json(MessageResponse {
        message: message.to_string(),
    }))
}

async fn update_profile(
    State(state): State<AppState>,
    Json(req): Json<ProfileUpdateRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    check_profile_update_request(&req, &state.config.server.public_url)?;
    let outcome = state
        .ingestion
        .apply_registration_change(RegistrationChange::UpdateProfile(&req))
        .await
        .map_err(change_error)?;

    if !outcome.is_active {
        // A private profile ends the registration; see `Storage::apply_registration_change`.
        return Err(reject(
            StatusCode::FORBIDDEN,
            format!("{}; registration removed", Rejection::PrivateVisibility),
        ));
    }
    let message = if outcome.profile_stored {
        "profile updated"
    } else {
        "stored profile is the same or newer; not replaced"
    };
    Ok(Json(MessageResponse {
        message: message.to_string(),
    }))
}

/// Respond 429 if the client address exceeded
/// `limits.register_requests_per_minute`; otherwise run the handler.
async fn limit_request_rate(
    State(state): State<AppState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let ip = client_ip(
        request.headers(),
        remote_addr,
        state.config.server.trust_forwarded_for,
    );
    if !state.register_limiter.allow(ip, Instant::now()) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(RETRY_AFTER, "60")],
            Json(MessageResponse {
                message: "too many requests".to_string(),
            }),
        )
            .into_response();
    }
    next.run(request).await
}

pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/register",
            post(register).put(update_profile).delete(unregister),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state, limit_request_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TestUser;
    use proscenium_types::{MAX_TIMESTAMP_DRIFT_MS, now_millis};

    const URL: &str = "https://server.test";

    fn status(result: Result<(), ApiError>) -> Option<StatusCode> {
        result.err().map(|(status, _)| status)
    }

    #[test]
    fn valid_requests_pass() {
        let user = TestUser::new(1);
        let req = user.registration(URL, now_millis(), Visibility::Public, None);
        assert_eq!(status(check_register_request(&req, URL)), None);
        // One trailing slash on either side is ignored.
        assert_eq!(
            status(check_register_request(&req, "https://server.test/")),
            None
        );

        let req = user.registration(
            URL,
            now_millis(),
            Visibility::Public,
            Some(REGISTRATION_ACTION_UNREGISTER),
        );
        assert_eq!(status(check_unregister_request(&req, URL)), None);

        let profile = user.profile("name", Visibility::Public, 5);
        let req = user.profile_update(URL, now_millis(), profile);
        assert_eq!(status(check_profile_update_request(&req, URL)), None);
    }

    #[test]
    fn request_signed_by_another_key_is_rejected_on_every_method() {
        let victim = TestUser::new(1);
        // Names the victim as master and signs with the attacker's key.
        let forger = TestUser::new(2).forging_as(&victim);

        let req = forger.registration(URL, now_millis(), Visibility::Public, None);
        assert_eq!(
            status(check_register_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );

        let unregister = Some(REGISTRATION_ACTION_UNREGISTER);
        let req = forger.registration(URL, now_millis(), Visibility::Public, unregister);
        assert_eq!(
            status(check_unregister_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );

        let profile = forger.profile("name", Visibility::Public, 5);
        let update = forger.profile_update(URL, now_millis(), profile);
        assert_eq!(
            status(check_profile_update_request(&update, URL)),
            Some(StatusCode::BAD_REQUEST)
        );

        // A profile signed by the victim, as published over gossip, in a request signed
        // by the attacker.
        let profile = victim.profile("name", Visibility::Public, 5);
        let update = forger.profile_update(URL, now_millis(), profile);
        assert_eq!(
            status(check_profile_update_request(&update, URL)),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn profile_fields_outside_the_signature_are_rejected() {
        let user = TestUser::new(1);
        let profile = user.profile("name", Visibility::Public, 5);
        let mut update = user.profile_update(URL, now_millis(), profile);
        update.profile.bio = "changed after signing".to_string();
        assert_eq!(
            status(check_profile_update_request(&update, URL)),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn stale_timestamp_and_other_server_are_rejected_on_every_method() {
        let user = TestUser::new(1);
        let old = now_millis() - MAX_TIMESTAMP_DRIFT_MS - 1_000;
        let future = now_millis() + MAX_TIMESTAMP_DRIFT_MS + 60_000;
        let unregister = Some(REGISTRATION_ACTION_UNREGISTER);

        for timestamp in [old, future] {
            let req = user.registration(URL, timestamp, Visibility::Public, None);
            assert_eq!(
                status(check_register_request(&req, URL)),
                Some(StatusCode::BAD_REQUEST)
            );
            let req = user.registration(URL, timestamp, Visibility::Public, unregister);
            assert_eq!(
                status(check_unregister_request(&req, URL)),
                Some(StatusCode::BAD_REQUEST)
            );
            let profile = user.profile("name", Visibility::Public, 5);
            let req = user.profile_update(URL, timestamp, profile);
            assert_eq!(
                status(check_profile_update_request(&req, URL)),
                Some(StatusCode::BAD_REQUEST)
            );
        }

        // A request signed for another server.
        let other = "https://other.test";
        let req = user.registration(other, now_millis(), Visibility::Public, None);
        assert_eq!(
            status(check_register_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );
        let req = user.registration(other, now_millis(), Visibility::Public, unregister);
        assert_eq!(
            status(check_unregister_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );
        let profile = user.profile("name", Visibility::Public, 5);
        let req = user.profile_update(other, now_millis(), profile);
        assert_eq!(
            status(check_profile_update_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn action_and_visibility_rules() {
        let user = TestUser::new(1);
        let unregister = Some(REGISTRATION_ACTION_UNREGISTER);

        // POST with an action, DELETE without the unregister action.
        let req = user.registration(URL, now_millis(), Visibility::Public, unregister);
        assert_eq!(
            status(check_register_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );
        let req = user.registration(URL, now_millis(), Visibility::Public, None);
        assert_eq!(
            status(check_unregister_request(&req, URL)),
            Some(StatusCode::BAD_REQUEST)
        );

        let req = user.registration(URL, now_millis(), Visibility::Private, None);
        assert_eq!(
            status(check_register_request(&req, URL)),
            Some(StatusCode::FORBIDDEN)
        );
        let req = user.registration(URL, now_millis(), Visibility::Listed, None);
        assert_eq!(status(check_register_request(&req, URL)), None);
    }

    #[test]
    fn storage_error_text_is_not_returned() {
        let (status, Json(body)) = change_error(ChangeError::Storage(anyhow::anyhow!(
            "table registrations is locked"
        )));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.message, "internal error");

        let (status, _) = change_error(ChangeError::Rejected(Rejection::Replayed));
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = change_error(ChangeError::Rejected(Rejection::NotRegistered));
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = change_error(ChangeError::Rejected(Rejection::UserLimitReached));
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
