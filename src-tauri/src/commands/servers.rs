use crate::state::AppState;
use iroh::SecretKey;
use iroh_social_types::{
    PostSearchResponse, RegistrationPayload, RegistrationRequest, ServerEntry, ServerFeedResponse,
    ServerInfo, ServerUser, TrendingResponse, UserSearchResponse, Visibility, now_millis,
    sign_registration,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn add_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<ServerEntry, String> {
    state.add_server(url).await
}

#[tauri::command]
pub async fn remove_server(state: State<'_, Arc<AppState>>, url: String) -> Result<(), String> {
    state.remove_server(url).await
}

#[tauri::command]
pub async fn list_servers(state: State<'_, Arc<AppState>>) -> Result<Vec<ServerEntry>, String> {
    state.list_servers().await
}

#[tauri::command]
pub async fn refresh_server_info(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<ServerInfo, String> {
    state.refresh_server_info(url).await
}

#[tauri::command]
pub async fn register_with_server(
    state: State<'_, Arc<AppState>>,
    url: String,
    visibility: String,
) -> Result<(), String> {
    state.register_with_server(url, visibility).await
}

#[tauri::command]
pub async fn unregister_from_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<(), String> {
    state.unregister_from_server(url).await
}

#[tauri::command]
pub async fn server_get_feed(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
    before: Option<i64>,
) -> Result<ServerFeedResponse, String> {
    state.server_get_feed(url, limit, before).await
}

#[tauri::command]
pub async fn server_get_trending(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
) -> Result<TrendingResponse, String> {
    state.server_get_trending(url, limit).await
}

#[tauri::command]
pub async fn server_search_users(
    state: State<'_, Arc<AppState>>,
    url: String,
    query: String,
    limit: Option<i64>,
) -> Result<UserSearchResponse, String> {
    state.server_search_users(url, query, limit).await
}

#[tauri::command]
pub async fn server_search_posts(
    state: State<'_, Arc<AppState>>,
    url: String,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<PostSearchResponse, String> {
    state.server_search_posts(url, query, limit, offset).await
}

#[tauri::command]
pub async fn server_list_users(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<UserSearchResponse, String> {
    state.server_list_users(url, limit, offset).await
}

#[tauri::command]
pub async fn sync_profile_to_server(
    state: State<'_, Arc<AppState>>,
    url: String,
) -> Result<(), String> {
    state.sync_profile_to_server_inner(&url).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn add_server(&self, url: String) -> Result<ServerEntry, String> {
        let url = url.trim_end_matches('/').to_string();
        self.storage
            .add_server(&url)
            .await
            .map_err(|e| e.to_string())?;
        if let Ok(info) = self.fetch_server_info(&url).await {
            self.storage
                .update_server_info(&url, &info.name, &info.description, &info.node_id)
                .await
                .map_err(|e| e.to_string())?;
        }
        self.storage
            .get_server(&url)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "server not found after adding".to_string())
    }

    pub(crate) async fn remove_server(&self, url: String) -> Result<(), String> {
        self.storage
            .remove_server(&url)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn list_servers(&self) -> Result<Vec<ServerEntry>, String> {
        self.storage.get_servers().await.map_err(|e| e.to_string())
    }

    pub(crate) async fn refresh_server_info(&self, url: String) -> Result<ServerInfo, String> {
        let info = self.fetch_server_info(&url).await?;
        self.storage
            .update_server_info(&url, &info.name, &info.description, &info.node_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(info)
    }

    pub(crate) async fn register_with_server(
        &self,
        url: String,
        visibility: String,
    ) -> Result<(), String> {
        let vis: Visibility = visibility.parse().map_err(|_| "invalid visibility")?;
        let (master_pubkey, transport_node_id, signing_key_bytes, delegation) = {
            let id = self.identity.read().await;
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
        let resp = self
            .http_client
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
        self.storage
            .mark_server_registered(&url, &vis.to_string())
            .await
            .map_err(|e| e.to_string())?;
        let _ = self.sync_profile_to_server_inner(&url).await;
        Ok(())
    }

    pub(crate) async fn unregister_from_server(&self, url: String) -> Result<(), String> {
        let (master_pubkey, transport_node_id, signing_key_bytes, delegation) = {
            let id = self.identity.read().await;
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
        let resp = self
            .http_client
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
        self.storage
            .mark_server_unregistered(&url)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn server_get_feed(
        &self,
        url: String,
        limit: Option<i64>,
        before: Option<i64>,
    ) -> Result<ServerFeedResponse, String> {
        let mut endpoint = format!("{url}/api/v1/feed?limit={}", limit.unwrap_or(50));
        if let Some(b) = before {
            endpoint.push_str(&format!("&before={b}"));
        }
        let resp = self
            .http_client
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

    pub(crate) async fn server_get_trending(
        &self,
        url: String,
        limit: Option<i64>,
    ) -> Result<TrendingResponse, String> {
        let endpoint = format!("{url}/api/v1/trending?limit={}", limit.unwrap_or(10));
        let resp = self
            .http_client
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

    pub(crate) async fn server_search_users(
        &self,
        url: String,
        query: String,
        limit: Option<i64>,
    ) -> Result<UserSearchResponse, String> {
        let endpoint = format!(
            "{url}/api/v1/users/search?q={}&limit={}",
            urlencoding::encode(&query),
            limit.unwrap_or(20)
        );
        let resp = self
            .http_client
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

    pub(crate) async fn server_search_posts(
        &self,
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
        let resp = self
            .http_client
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

    pub(crate) async fn server_list_users(
        &self,
        url: String,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<UserSearchResponse, String> {
        let endpoint = format!(
            "{url}/api/v1/users?limit={}&offset={}",
            limit.unwrap_or(20),
            offset.unwrap_or(0)
        );
        let resp = self
            .http_client
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

    /// Sync the local profile to a server. Called internally from `save_my_profile`,
    /// `register_with_server`, and `rotate_signing_key`.
    pub(crate) async fn sync_profile_to_server_inner(&self, url: &str) -> Result<(), String> {
        let (master_pubkey, transport_node_id, signing_key_bytes, delegation) = {
            let id = self.identity.read().await;
            (
                id.master_pubkey.clone(),
                id.transport_node_id.clone(),
                id.signing_secret_key_bytes,
                id.delegation.clone(),
            )
        };
        let profile = self
            .storage
            .get_profile(&master_pubkey)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("no profile to sync")?;

        let vis = self
            .storage
            .get_server(url)
            .await
            .map_err(|e| e.to_string())?
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
            delegation: iroh_social_types::SigningKeyDelegation,
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

        let resp = self
            .http_client
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

    async fn fetch_server_info(&self, url: &str) -> Result<ServerInfo, String> {
        let resp = self
            .http_client
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
}
