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
    #[serde(default)]
    pub retention_days: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFeedPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub media_json: Option<String>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
    pub indexed_at: i64,
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
    state.storage.add_server(&url).await.str_err()?;

    // Try to fetch server info
    if let Ok(info) = fetch_server_info_inner(&url).await {
        state
            .storage
            .update_server_info(&url, &info.name, &info.description, &info.node_id)
            .await
            .str_err()?;
    }

    state
        .storage
        .get_server(&url)
        .await
        .str_err()?
        .ok_or_else(|| "server not found after adding".to_string())
}

#[tauri::command]
pub async fn remove_server(state: State<'_, Arc<AppState>>, url: String) -> Result<(), String> {
    state.storage.remove_server(&url).await.str_err()
}

#[tauri::command]
pub async fn list_servers(state: State<'_, Arc<AppState>>) -> Result<Vec<ServerEntry>, String> {
    state.storage.get_servers().await.str_err()
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
        .await
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
    let master_pubkey = state.master_pubkey.clone();
    let transport_node_id = state.transport_node_id.clone();
    let secret_key = SecretKey::from_bytes(&state.signing_secret_key_bytes);

    let payload = RegistrationPayload {
        master_pubkey: master_pubkey.clone(),
        transport_node_id: transport_node_id.clone(),
        server_url: url.clone(),
        timestamp: now_millis(),
        visibility: vis,
        action: None,
    };
    let signature = sign_registration(&payload, &secret_key);

    let request = RegistrationRequest {
        master_pubkey,
        transport_node_id,
        server_url: url.clone(),
        timestamp: payload.timestamp,
        visibility: vis,
        action: None,
        signature,
        delegation: state.delegation.clone(),
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
        .await
        .str_err()?;

    // Auto-sync profile to the server after registration
    let _ = sync_profile_inner(&state, &url).await;

    Ok(())
}

#[tauri::command]
pub async fn unregister_from_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<(), String> {
    let master_pubkey = state.master_pubkey.clone();
    let transport_node_id = state.transport_node_id.clone();
    let secret_key = SecretKey::from_bytes(&state.signing_secret_key_bytes);

    let payload = RegistrationPayload {
        master_pubkey: master_pubkey.clone(),
        transport_node_id: transport_node_id.clone(),
        server_url: url.clone(),
        timestamp: now_millis(),
        visibility: Visibility::Public,
        action: Some("unregister".to_string()),
    };
    let signature = sign_registration(&payload, &secret_key);

    let request = RegistrationRequest {
        master_pubkey,
        transport_node_id,
        server_url: url.clone(),
        timestamp: payload.timestamp,
        visibility: Visibility::Public,
        action: Some("unregister".to_string()),
        signature,
        delegation: state.delegation.clone(),
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

    state.storage.mark_server_unregistered(&url).await.str_err()
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerUser {
    pub pubkey: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_hash: Option<String>,
    pub visibility: String,
    pub registered_at: i64,
    pub post_count: i64,
    pub latest_post_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSearchResponse {
    pub users: Vec<ServerUser>,
    pub total: usize,
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSearchPost {
    pub id: String,
    pub author: String,
    pub content: String,
    pub timestamp: i64,
    pub media_json: Option<String>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,
    pub indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostSearchResponse {
    pub posts: Vec<ServerSearchPost>,
    pub total: i64,
    pub query: String,
}

#[tauri::command]
pub async fn server_search_users(
    url: String,
    query: String,
    limit: Option<i64>,
) -> Result<UserSearchResponse, String> {
    let endpoint = format!(
        "{url}/api/v1/users/search?q={}&limit={}",
        urlencoding::encode(&query),
        limit.unwrap_or(20)
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("user search failed: {}", resp.status()));
    }

    resp.json::<UserSearchResponse>()
        .await
        .map_err(|e| format!("failed to parse user search: {e}"))
}

#[tauri::command]
pub async fn server_search_posts(
    url: String,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<PostSearchResponse, String> {
    let mut endpoint = format!(
        "{url}/api/v1/posts/search?q={}&limit={}",
        urlencoding::encode(&query),
        limit.unwrap_or(20)
    );
    if let Some(o) = offset {
        endpoint.push_str(&format!("&offset={o}"));
    }

    let client = reqwest::Client::new();
    let resp = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("post search failed: {}", resp.status()));
    }

    resp.json::<PostSearchResponse>()
        .await
        .map_err(|e| format!("failed to parse post search: {e}"))
}

#[tauri::command]
pub async fn server_list_users(
    url: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<UserSearchResponse, String> {
    let endpoint = format!(
        "{url}/api/v1/users?limit={}&offset={}",
        limit.unwrap_or(20),
        offset.unwrap_or(0)
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("user list failed: {}", resp.status()));
    }

    let list: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse user list: {e}"))?;

    let users: Vec<ServerUser> =
        serde_json::from_value(list.get("users").cloned().unwrap_or_default())
            .map_err(|e| format!("failed to parse users: {e}"))?;

    Ok(UserSearchResponse {
        total: users.len(),
        users,
        query: String::new(),
    })
}

#[tauri::command]
pub async fn sync_profile_to_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<(), String> {
    sync_profile_inner(&state, &url).await
}

pub async fn sync_profile_inner(state: &AppState, url: &str) -> Result<(), String> {
    let master_pubkey = state.master_pubkey.clone();
    let secret_key = SecretKey::from_bytes(&state.signing_secret_key_bytes);

    let profile = state
        .storage
        .get_profile(&master_pubkey)
        .await
        .str_err()?
        .ok_or("no profile to sync")?;

    let vis = state
        .storage
        .get_server(url)
        .await
        .str_err()?
        .map(|s| s.visibility)
        .unwrap_or_else(|| "public".to_string());

    let payload = RegistrationPayload {
        master_pubkey: master_pubkey.clone(),
        transport_node_id: state.transport_node_id.clone(),
        server_url: url.to_string(),
        timestamp: now_millis(),
        visibility: vis.parse().unwrap_or_default(),
        action: None,
    };
    let signature = sign_registration(&payload, &secret_key);

    #[derive(Serialize)]
    struct ProfileUpdate {
        master_pubkey: String,
        transport_node_id: String,
        server_url: String,
        timestamp: u64,
        visibility: String,
        display_name: Option<String>,
        bio: Option<String>,
        avatar_hash: Option<String>,
        signature: String,
        delegation: iroh_social_types::SigningKeyDelegation,
    }

    let update = ProfileUpdate {
        master_pubkey,
        transport_node_id: state.transport_node_id.clone(),
        server_url: url.to_string(),
        timestamp: payload.timestamp,
        visibility: vis,
        display_name: Some(profile.display_name).filter(|s| !s.is_empty()),
        bio: Some(profile.bio).filter(|s| !s.is_empty()),
        avatar_hash: profile.avatar_hash,
        signature,
        delegation: state.delegation.clone(),
    };

    let client = reqwest::Client::new();
    let resp = client
        .put(format!("{url}/api/v1/register"))
        .json(&update)
        .send()
        .await
        .map_err(|e| format!("profile sync failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("profile sync failed ({status}): {body}"));
    }

    Ok(())
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
