use crate::error::CmdResult;
use crate::state::AppState;
use iroh::SecretKey;
use proscenium_types::{
    PostSearchResponse, RegistrationPayload, RegistrationRequest, ServerEntry, ServerFeedResponse,
    ServerInfo, ServerUser, TrendingResponse, UserSearchResponse, Visibility, now_millis,
    sign_registration,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn add_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<ServerEntry> {
    let url = url.trim_end_matches('/').to_string();
    state.storage.add_server(&url).await?;
    if let Ok(info) = fetch_server_info(&state.http_client, &url).await {
        state
            .storage
            .update_server_info(&url, &info.name, &info.description, &info.node_id)
            .await?;
    }
    state
        .storage
        .get_server(&url)
        .await?
        .ok_or_else(|| "server not found after adding".to_string().into())
}

#[tauri::command]
pub async fn remove_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<()> {
    state.storage.remove_server(&url).await
}

#[tauri::command]
pub async fn list_servers(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<ServerEntry>> {
    state.storage.get_servers().await
}

#[tauri::command]
pub async fn refresh_server_info(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> CmdResult<ServerInfo> {
    let info = fetch_server_info(&state.http_client, &url).await?;
    state
        .storage
        .update_server_info(&url, &info.name, &info.description, &info.node_id)
        .await?;
    Ok(info)
}

#[tauri::command]
pub async fn register_with_server(
    state: State<'_, Arc<AppState>>,
    url: String,
    visibility: String,
) -> CmdResult<()> {
    let vis: Visibility = visibility
        .parse()
        .map_err(|_: String| "invalid visibility")?;
    let (master_pubkey, transport_node_id, signing_key_bytes, delegation) = {
        let id = state.identity.read().await;
        (
            id.master_pubkey.clone(),
            id.transport_node_id.clone(),
            id.signing_secret_key_bytes,
            id.delegation.clone(),
        )
    };
    let payload = RegistrationPayload {
        master_pubkey: master_pubkey.clone(),
        transport_node_id: transport_node_id.clone(),
        server_url: url.clone(),
        timestamp: now_millis(),
        visibility: vis,
        action: None,
    };
    let signature = sign_registration(&payload, &SecretKey::from_bytes(&signing_key_bytes));
    let request = RegistrationRequest {
        master_pubkey,
        transport_node_id,
        server_url: url.clone(),
        timestamp: payload.timestamp,
        visibility: vis,
        action: None,
        signature,
        delegation,
    };
    let resp = state
        .http_client
        .post(format!("{url}/api/v1/register"))
        .json(&request)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("registration failed ({status}): {body}").into());
    }
    state
        .storage
        .mark_server_registered(&url, &vis.to_string())
        .await?;
    let _ = sync_profile_to_server_inner(&state.http_client, &state.storage, &state.identity, &url)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn unregister_from_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<()> {
    let (master_pubkey, transport_node_id, signing_key_bytes, delegation) = {
        let id = state.identity.read().await;
        (
            id.master_pubkey.clone(),
            id.transport_node_id.clone(),
            id.signing_secret_key_bytes,
            id.delegation.clone(),
        )
    };
    let payload = RegistrationPayload {
        master_pubkey: master_pubkey.clone(),
        transport_node_id: transport_node_id.clone(),
        server_url: url.clone(),
        timestamp: now_millis(),
        visibility: Visibility::Public,
        action: Some("unregister".to_string()),
    };
    let signature = sign_registration(&payload, &SecretKey::from_bytes(&signing_key_bytes));
    let request = RegistrationRequest {
        master_pubkey,
        transport_node_id,
        server_url: url.clone(),
        timestamp: payload.timestamp,
        visibility: Visibility::Public,
        action: Some("unregister".to_string()),
        signature,
        delegation,
    };
    let resp = state
        .http_client
        .delete(format!("{url}/api/v1/register"))
        .json(&request)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("unregistration failed ({status}): {body}").into());
    }
    state.storage.mark_server_unregistered(&url).await
}

#[tauri::command]
pub async fn server_get_feed(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
    before: Option<i64>,
) -> CmdResult<ServerFeedResponse> {
    let mut endpoint = format!("{url}/api/v1/feed?limit={}", limit.unwrap_or(50));
    if let Some(b) = before {
        endpoint.push_str(&format!("&before={b}"));
    }
    let resp = state.http_client.get(&endpoint).send().await?;
    if !resp.status().is_success() {
        return Err(format!("feed request failed: {}", resp.status()).into());
    }
    Ok(resp.json::<ServerFeedResponse>().await?)
}

#[tauri::command]
pub async fn server_get_trending(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
) -> CmdResult<TrendingResponse> {
    let endpoint = format!("{url}/api/v1/trending?limit={}", limit.unwrap_or(10));
    let resp = state.http_client.get(&endpoint).send().await?;
    if !resp.status().is_success() {
        return Err(format!("trending request failed: {}", resp.status()).into());
    }
    Ok(resp.json::<TrendingResponse>().await?)
}

#[tauri::command]
pub async fn server_search_users(
    state: State<'_, Arc<AppState>>,
    url: String,
    query: String,
    limit: Option<i64>,
) -> CmdResult<UserSearchResponse> {
    let endpoint = format!(
        "{url}/api/v1/users/search?q={}&limit={}",
        urlencoding::encode(&query),
        limit.unwrap_or(20)
    );
    let resp = state.http_client.get(&endpoint).send().await?;
    if !resp.status().is_success() {
        return Err(format!("user search failed: {}", resp.status()).into());
    }
    Ok(resp.json::<UserSearchResponse>().await?)
}

#[tauri::command]
pub async fn server_search_posts(
    state: State<'_, Arc<AppState>>,
    url: String,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<PostSearchResponse> {
    let mut endpoint = format!(
        "{url}/api/v1/posts/search?q={}&limit={}",
        urlencoding::encode(&query),
        limit.unwrap_or(20)
    );
    if let Some(o) = offset {
        endpoint.push_str(&format!("&offset={o}"));
    }
    let resp = state.http_client.get(&endpoint).send().await?;
    if !resp.status().is_success() {
        return Err(format!("post search failed: {}", resp.status()).into());
    }
    Ok(resp.json::<PostSearchResponse>().await?)
}

#[tauri::command]
pub async fn server_list_users(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<UserSearchResponse> {
    let endpoint = format!(
        "{url}/api/v1/users?limit={}&offset={}",
        limit.unwrap_or(20),
        offset.unwrap_or(0)
    );
    let resp = state.http_client.get(&endpoint).send().await?;
    if !resp.status().is_success() {
        return Err(format!("user list failed: {}", resp.status()).into());
    }
    let list: serde_json::Value = resp.json().await?;
    let users: Vec<ServerUser> =
        serde_json::from_value(list.get("users").cloned().unwrap_or_default())?;
    Ok(UserSearchResponse {
        total: users.len(),
        users,
        query: String::new(),
    })
}

#[tauri::command]
pub async fn sync_profile_to_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<()> {
    sync_profile_to_server_inner(&state.http_client, &state.storage, &state.identity, &url).await
}

/// Sync the local profile to a server. Called from `save_my_profile`,
/// `register_with_server`, and `rotate_signing_key`.
pub(crate) async fn sync_profile_to_server_inner(
    http_client: &reqwest::Client,
    storage: &crate::storage::Storage,
    identity: &crate::state::SharedIdentity,
    url: &str,
) -> CmdResult<()> {
    let (master_pubkey, transport_node_id, signing_key_bytes, delegation) = {
        let id = identity.read().await;
        (
            id.master_pubkey.clone(),
            id.transport_node_id.clone(),
            id.signing_secret_key_bytes,
            id.delegation.clone(),
        )
    };
    let profile = storage
        .get_profile(&master_pubkey)
        .await?
        .ok_or("no profile to sync")?;

    let vis = storage
        .get_server(url)
        .await?
        .map(|s| s.visibility)
        .unwrap_or_else(|| "public".to_string());

    let payload = RegistrationPayload {
        master_pubkey: master_pubkey.clone(),
        transport_node_id: transport_node_id.clone(),
        server_url: url.to_string(),
        timestamp: now_millis(),
        visibility: vis.parse().unwrap_or_default(),
        action: None,
    };
    let signature = sign_registration(&payload, &SecretKey::from_bytes(&signing_key_bytes));

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
        delegation: proscenium_types::SigningKeyDelegation,
    }

    let update = ProfileUpdate {
        master_pubkey,
        transport_node_id,
        server_url: url.to_string(),
        timestamp: payload.timestamp,
        visibility: vis,
        display_name: Some(profile.display_name).filter(|s| !s.is_empty()),
        bio: Some(profile.bio).filter(|s| !s.is_empty()),
        avatar_hash: profile.avatar_hash,
        signature,
        delegation,
    };

    let resp = http_client
        .put(format!("{url}/api/v1/register"))
        .json(&update)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("profile sync failed ({status}): {body}").into());
    }

    Ok(())
}

pub(crate) async fn fetch_server_info(
    http_client: &reqwest::Client,
    url: &str,
) -> CmdResult<ServerInfo> {
    let resp = http_client.get(format!("{url}/api/v1/info")).send().await?;
    if !resp.status().is_success() {
        return Err(format!("server info request failed: {}", resp.status()).into());
    }
    Ok(resp.json::<ServerInfo>().await?)
}
