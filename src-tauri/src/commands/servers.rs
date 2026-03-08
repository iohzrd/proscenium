use crate::ext::ResultExt;
use crate::state::AppState;
use crate::storage::servers::ServerEntry;
use iroh::SecretKey;
use iroh_social_types::{
    RegistrationPayload, RegistrationRequest, Visibility, now_millis, sign_registration,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub description: String,
    pub version: String,
    pub node_id: String,
    pub registered_users: i64,
    pub total_posts: i64,
    pub uptime_seconds: u64,
    pub registration_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeedPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub reply_to: Option<String>,
    pub media_hashes: Option<String>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeedResponse {
    pub posts: Vec<ServerFeedPost>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendingHashtag {
    pub tag: String,
    pub post_count: i64,
    pub computed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendingResponse {
    pub hashtags: Vec<TrendingHashtag>,
    pub computed_at: Option<i64>,
}

#[tauri::command]
pub async fn add_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<ServerEntry, String> {
    let url = url.trim_end_matches('/').to_string();
    state.storage.add_server(&url).str_err()?;

    // Try to fetch server info
    if let Ok(info) = fetch_server_info_inner(&url).await {
        state
            .storage
            .update_server_info(&url, &info.name, &info.description, &info.node_id)
            .str_err()?;
    }

    state
        .storage
        .get_server(&url)
        .str_err()?
        .ok_or_else(|| "server not found after adding".to_string())
}

#[tauri::command]
pub async fn remove_server(state: State<'_, Arc<AppState>>, url: String) -> Result<(), String> {
    state.storage.remove_server(&url).str_err()
}

#[tauri::command]
pub async fn list_servers(state: State<'_, Arc<AppState>>) -> Result<Vec<ServerEntry>, String> {
    state.storage.get_servers().str_err()
}

#[tauri::command]
pub async fn refresh_server_info(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<ServerInfo, String> {
    let info = fetch_server_info_inner(&url).await?;
    state
        .storage
        .update_server_info(&url, &info.name, &info.description, &info.node_id)
        .str_err()?;
    Ok(info)
}

#[tauri::command]
pub async fn register_with_server(
    state: State<'_, Arc<AppState>>,
    url: String,
    visibility: String,
) -> Result<(), String> {
    let vis: Visibility = visibility.parse().map_err(|_| "invalid visibility")?;
    let pubkey = state.endpoint.id().to_string();
    let secret_key = SecretKey::from_bytes(&state.secret_key_bytes);

    let payload = RegistrationPayload {
        pubkey: pubkey.clone(),
        server_url: url.clone(),
        timestamp: now_millis(),
        visibility: vis,
        action: None,
    };
    let signature = sign_registration(&payload, &secret_key);

    let request = RegistrationRequest {
        pubkey,
        server_url: url.clone(),
        timestamp: payload.timestamp,
        visibility: vis,
        action: None,
        signature,
    };

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/api/v1/register"))
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("registration failed ({status}): {body}"));
    }

    state
        .storage
        .mark_server_registered(&url, &vis.to_string())
        .str_err()
}

#[tauri::command]
pub async fn unregister_from_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<(), String> {
    let pubkey = state.endpoint.id().to_string();
    let secret_key = SecretKey::from_bytes(&state.secret_key_bytes);

    let payload = RegistrationPayload {
        pubkey: pubkey.clone(),
        server_url: url.clone(),
        timestamp: now_millis(),
        visibility: Visibility::Public,
        action: Some("unregister".to_string()),
    };
    let signature = sign_registration(&payload, &secret_key);

    let request = RegistrationRequest {
        pubkey,
        server_url: url.clone(),
        timestamp: payload.timestamp,
        visibility: Visibility::Public,
        action: Some("unregister".to_string()),
        signature,
    };

    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("{url}/api/v1/register"))
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("unregistration failed ({status}): {body}"));
    }

    state.storage.mark_server_unregistered(&url).str_err()
}

#[tauri::command]
pub async fn server_get_feed(
    url: String,
    limit: Option<i64>,
    before: Option<i64>,
) -> Result<ServerFeedResponse, String> {
    let mut endpoint = format!("{url}/api/v1/feed?limit={}", limit.unwrap_or(50));
    if let Some(b) = before {
        endpoint.push_str(&format!("&before={b}"));
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("feed request failed: {}", resp.status()));
    }

    resp.json::<ServerFeedResponse>()
        .await
        .map_err(|e| format!("failed to parse feed: {e}"))
}

#[tauri::command]
pub async fn server_get_trending(
    url: String,
    limit: Option<i64>,
) -> Result<TrendingResponse, String> {
    let endpoint = format!("{url}/api/v1/trending?limit={}", limit.unwrap_or(10));

    let client = reqwest::Client::new();
    let resp = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("trending request failed: {}", resp.status()));
    }

    resp.json::<TrendingResponse>()
        .await
        .map_err(|e| format!("failed to parse trending: {e}"))
}

async fn fetch_server_info_inner(url: &str) -> Result<ServerInfo, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{url}/api/v1/info"))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("server info request failed: {}", resp.status()));
    }

    resp.json::<ServerInfo>()
        .await
        .map_err(|e| format!("failed to parse server info: {e}"))
}
