use super::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, post, put};
use axum::{Json, Router};
use iroh_social_types::{
    RegistrationRequest, Visibility, now_millis, verify_registration_signature,
};
use serde::{Deserialize, Serialize};

const TIMESTAMP_TOLERANCE_MS: u64 = 5 * 60 * 1000; // 5 minutes

#[derive(Serialize)]
struct RegisterResponse {
    pubkey: String,
    registered_at: u64,
    message: String,
}

#[derive(Serialize)]
struct MessageResponse {
    message: String,
}

#[derive(Deserialize)]
struct ProfileUpdateRequest {
    pubkey: String,
    server_url: String,
    timestamp: u64,
    visibility: String,
    display_name: Option<String>,
    bio: Option<String>,
    avatar_hash: Option<String>,
    signature: String,
}

fn validate_registration(
    req: &RegistrationRequest,
    config: &crate::config::Config,
) -> Result<(), (StatusCode, String)> {
    // Verify signature
    if let Err(e) = verify_registration_signature(req) {
        return Err((StatusCode::BAD_REQUEST, format!("invalid signature: {e}")));
    }

    // Check timestamp
    let now = now_millis();
    if req.timestamp.abs_diff(now) > TIMESTAMP_TOLERANCE_MS {
        return Err((
            StatusCode::BAD_REQUEST,
            "timestamp too far from server time".to_string(),
        ));
    }

    // Check server URL
    if req.server_url != config.server.public_url {
        return Err((StatusCode::BAD_REQUEST, "server_url mismatch".to_string()));
    }

    // Check visibility
    if req.visibility == Visibility::Private {
        return Err((
            StatusCode::FORBIDDEN,
            "private users cannot register with servers".to_string(),
        ));
    }

    // Check registration open
    if !config.server.registration_open {
        return Err((StatusCode::FORBIDDEN, "registration is closed".to_string()));
    }

    Ok(())
}

async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegistrationRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), (StatusCode, Json<MessageResponse>)> {
    if req.action.as_deref() == Some("unregister") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: "use DELETE endpoint for unregistration".to_string(),
            }),
        ));
    }

    validate_registration(&req, &state.config)
        .map_err(|(code, msg)| (code, Json(MessageResponse { message: msg })))?;

    // Check if already registered
    if let Ok(Some(existing)) = state.storage.get_registration(&req.pubkey).await
        && existing.is_active != 0
    {
        let vis = req.visibility.to_string();
        state
            .storage
            .register_user(&req.pubkey, &vis, None)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(MessageResponse {
                        message: format!("failed to update registration: {e}"),
                    }),
                )
            })?;

        return Ok((
            StatusCode::OK,
            Json(RegisterResponse {
                pubkey: req.pubkey,
                registered_at: existing.registered_at as u64,
                message: "registration updated".to_string(),
            }),
        ));
    }

    let vis = req.visibility.to_string();
    let now = now_millis();
    state
        .storage
        .register_user(&req.pubkey, &vis, None)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MessageResponse {
                    message: format!("failed to register: {e}"),
                }),
            )
        })?;

    // If Public, subscribe to gossip and trigger sync
    if req.visibility == Visibility::Public
        && let Err(e) = state.ingestion.subscribe(&req.pubkey).await
    {
        tracing::error!(
            "[auth] failed to subscribe to {}: {e}",
            iroh_social_types::short_id(&req.pubkey)
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(RegisterResponse {
            pubkey: req.pubkey,
            registered_at: now,
            message: "registered successfully".to_string(),
        }),
    ))
}

async fn unregister(
    State(state): State<AppState>,
    Json(req): Json<RegistrationRequest>,
) -> Result<Json<MessageResponse>, (StatusCode, Json<MessageResponse>)> {
    if req.action.as_deref() != Some("unregister") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: "action must be 'unregister'".to_string(),
            }),
        ));
    }

    if let Err(e) = verify_registration_signature(&req) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: format!("invalid signature: {e}"),
            }),
        ));
    }

    state.ingestion.unsubscribe(&req.pubkey).await;
    state
        .storage
        .unregister_user(&req.pubkey)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MessageResponse {
                    message: format!("failed to unregister: {e}"),
                }),
            )
        })?;

    Ok(Json(MessageResponse {
        message: "unregistered successfully".to_string(),
    }))
}

async fn update_profile(
    State(state): State<AppState>,
    Json(req): Json<ProfileUpdateRequest>,
) -> Result<Json<MessageResponse>, (StatusCode, Json<MessageResponse>)> {
    // Verify via RegistrationRequest
    let reg_req = RegistrationRequest {
        pubkey: req.pubkey.clone(),
        server_url: req.server_url.clone(),
        timestamp: req.timestamp,
        visibility: req.visibility.parse().map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(MessageResponse {
                    message: "invalid visibility".to_string(),
                }),
            )
        })?,
        action: None,
        signature: req.signature.clone(),
    };

    if let Err(e) = verify_registration_signature(&reg_req) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: format!("invalid signature: {e}"),
            }),
        ));
    }

    let visibility: Visibility = req.visibility.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                message: "invalid visibility".to_string(),
            }),
        )
    })?;

    let profile = iroh_social_types::Profile {
        display_name: req.display_name.unwrap_or_default(),
        bio: req.bio.unwrap_or_default(),
        avatar_hash: req.avatar_hash,
        avatar_ticket: None,
        visibility,
    };

    let updated = state
        .storage
        .update_profile(&req.pubkey, &profile)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(MessageResponse {
                    message: format!("failed to update profile: {e}"),
                }),
            )
        })?;

    if !updated {
        return Err((
            StatusCode::NOT_FOUND,
            Json(MessageResponse {
                message: "user not registered".to_string(),
            }),
        ));
    }

    Ok(Json(MessageResponse {
        message: "profile updated".to_string(),
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/register", post(register))
        .route("/api/v1/register", put(update_profile))
        .route("/api/v1/register", delete(unregister))
}
