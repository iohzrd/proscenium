use crate::error::CmdResult;
use crate::state::AppState;
use crate::storage::Storage;
use proscenium_types::{
    Post, PostSearchResponse, ProfileUpdateRequest, REGISTRATION_ACTION_UNREGISTER,
    RegistrationPayload, RegistrationRequest, ServerEntry, ServerFeedPost, ServerFeedResponse,
    ServerInfo, ServerUser, TrendingResponse, UserSearchResponse, Visibility, is_valid_pubkey,
    now_millis, sign_profile_update, sign_registration, truncate_chars, validate_post,
    verify_post_signature,
};
use reqwest::Url;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::State;

/// Largest response body accepted from a discovery server.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Number of bytes read from the body of a non-success response.
const MAX_ERROR_BODY_BYTES: usize = 4 * 1024;
/// Largest number of characters of a server-supplied error message passed to the webview.
const MAX_ERROR_MESSAGE_CHARS: usize = 200;
const MAX_SERVER_URL_LEN: usize = 2_048;
const MAX_SERVER_NAME_CHARS: usize = 100;
const MAX_SERVER_DESCRIPTION_CHARS: usize = 500;
const MAX_SERVER_VERSION_CHARS: usize = 32;
const MAX_SEARCH_QUERY_LEN: usize = 256;
/// Largest page sizes the server accepts for posts and for users and hashtags.
const MAX_POST_PAGE: i64 = 200;
const MAX_LIST_PAGE: i64 = 100;

/// Parse a discovery server URL and return its normalized form: scheme, host, port (if not
/// the default) and path as serialized by `Url`, without a trailing slash.
///
/// Returns an error unless the scheme is `http` or `https`. A URL with credentials, a query
/// or a fragment is rejected.
pub(crate) fn normalize_server_url(input: &str) -> CmdResult<String> {
    let input = input.trim();
    if input.len() > MAX_SERVER_URL_LEN {
        return Err("server URL is too long".into());
    }
    let url = Url::parse(input).map_err(|e| format!("invalid server URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("server URL must start with http:// or https://".into());
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("server URL has no host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("server URL must not contain credentials".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("server URL must not contain a query or a fragment".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Normalize `url` and return it if the `servers` table has a row for it.
///
/// Every command that sends a request to an existing server resolves its URL here, so
/// requests go only to servers the user added with `add_server`.
async fn stored_server_url(storage: &Storage, url: &str) -> CmdResult<String> {
    let url = normalize_server_url(url)?;
    if storage.get_server(&url).await?.is_none() {
        return Err(format!("server {url} is not in the server list").into());
    }
    Ok(url)
}

/// Read a response body. Returns an error if it is longer than `limit` bytes.
async fn read_body(mut resp: reqwest::Response, limit: usize) -> CmdResult<Vec<u8>> {
    if resp.content_length().is_some_and(|len| len > limit as u64) {
        return Err("server response is too large".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Err("server response is too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Replace control characters with spaces, merge runs of whitespace and keep at most
/// `MAX_ERROR_MESSAGE_CHARS` characters.
fn sanitize_message(raw: &str) -> String {
    let printable: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let merged = printable.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&merged, MAX_ERROR_MESSAGE_CHARS).to_string()
}

/// Describe a non-success response: the status code, followed by the sanitized `message`
/// field of a JSON body or the sanitized start of a text body, if there is one.
async fn describe_failure(mut resp: reqwest::Response) -> String {
    let status = resp.status();
    let mut body: Vec<u8> = Vec::new();
    while body.len() < MAX_ERROR_BODY_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let room = MAX_ERROR_BODY_BYTES - body.len();
                body.extend(chunk.iter().take(room));
            }
            _ => break,
        }
    }
    let message = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(json) => json
            .get("message")
            .and_then(|m| m.as_str())
            .map(sanitize_message)
            .unwrap_or_default(),
        Err(_) => sanitize_message(&String::from_utf8_lossy(&body)),
    };
    if message.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {message}")
    }
}

/// Send a GET request and parse the JSON body. `what` names the request in error strings.
async fn get_json<T: DeserializeOwned>(
    http_client: &reqwest::Client,
    url: String,
    what: &str,
) -> CmdResult<T> {
    let resp = http_client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(format!("{what} failed: {}", resp.status()).into());
    }
    let body = read_body(resp, MAX_RESPONSE_BYTES).await?;
    // The serde error text can quote the body, so it is logged and not returned.
    serde_json::from_slice(&body).map_err(|e| {
        log::warn!("[server] {what}: unparsable response: {e}");
        format!("{what} failed: the server response has an unexpected format").into()
    })
}

/// Truncate the text fields of a server's self-description and clear a `node_id` that is
/// not a public key.
fn bound_server_info(mut info: ServerInfo) -> ServerInfo {
    info.name = truncate_chars(&info.name, MAX_SERVER_NAME_CHARS).to_string();
    info.description = truncate_chars(&info.description, MAX_SERVER_DESCRIPTION_CHARS).to_string();
    info.version = truncate_chars(&info.version, MAX_SERVER_VERSION_CHARS).to_string();
    if !is_valid_pubkey(&info.node_id) {
        info.node_id.clear();
    }
    info
}

/// Request `/api/v1/info` from the server at `url` (already normalized).
async fn fetch_server_info(http_client: &reqwest::Client, url: &str) -> CmdResult<ServerInfo> {
    let info: ServerInfo = get_json(
        http_client,
        format!("{url}/api/v1/info"),
        "server info request",
    )
    .await?;
    Ok(bound_server_info(info))
}

/// Drop users whose `pubkey` is not a public key or repeats an earlier entry, and clear a
/// `transport_node_id` that is not a public key.
fn check_server_users(users: Vec<ServerUser>) -> Vec<ServerUser> {
    let mut seen = HashSet::new();
    users
        .into_iter()
        .filter(|user| is_valid_pubkey(&user.pubkey) && seen.insert(user.pubkey.clone()))
        .map(|mut user| {
            if user
                .transport_node_id
                .as_deref()
                .is_some_and(|id| !is_valid_pubkey(id))
            {
                user.transport_node_id = None;
            }
            user
        })
        .collect()
}

/// Rebuild the signed `Post` from a server row. Returns `None` if the row cannot be
/// rebuilt, the post fails `validate_post` (which requires the id to be the hash of the
/// content and author), or its author did not sign it.
fn verified_post(server_post: &ServerFeedPost) -> Option<Post> {
    let post = server_post.to_post()?;
    validate_post(&post).ok()?;
    verify_post_signature(&post).ok()?;
    Some(post)
}

/// Check server-supplied posts before they reach the webview. A post is dropped if
/// `verified_post` rejects it or it repeats an earlier id.
fn check_server_posts(posts: Vec<ServerFeedPost>) -> Vec<ServerFeedPost> {
    let mut seen = HashSet::new();
    posts
        .into_iter()
        .filter(|server_post| verified_post(server_post).is_some_and(|post| seen.insert(post.id)))
        .collect()
}

/// Add a discovery server. The row is stored only after the server answered
/// `/api/v1/info`. Adding a URL that is already stored replaces its name, description
/// and NodeId.
#[tauri::command]
pub async fn add_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<ServerEntry> {
    let url = normalize_server_url(&url)?;
    let info = fetch_server_info(&state.http_client, &url).await?;
    state
        .storage
        .add_server(&url, &info.name, &info.description, &info.node_id)
        .await?;
    state
        .storage
        .get_server(&url)
        .await?
        .ok_or_else(|| "server not found after adding".to_string().into())
}

/// Delete the server row whose URL equals `url` as given or its normalized form.
#[tauri::command]
pub async fn remove_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<()> {
    state.storage.remove_server(&url).await?;
    if let Ok(normalized) = normalize_server_url(&url)
        && normalized != url
    {
        state.storage.remove_server(&normalized).await?;
    }
    Ok(())
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
    let url = stored_server_url(&state.storage, &url).await?;
    let info = fetch_server_info(&state.http_client, &url).await?;
    state
        .storage
        .update_server_info(&url, &info.name, &info.description, &info.node_id)
        .await?;
    Ok(info)
}

/// Build a signed registration request for the server at `url`.
async fn build_registration_request(
    identity: &crate::state::SharedIdentity,
    url: &str,
    visibility: Visibility,
    action: Option<&str>,
) -> RegistrationRequest {
    let (master_pubkey, transport_node_id, master_key) = {
        let id = identity.read().await;
        (
            id.master_pubkey.clone(),
            id.transport_node_id.clone(),
            id.master_key.clone(),
        )
    };
    let payload = RegistrationPayload {
        master_pubkey,
        transport_node_id,
        server_url: url.to_string(),
        timestamp: now_millis(),
        visibility,
        action: action.map(str::to_string),
    };
    let signature = sign_registration(&payload, &master_key);
    RegistrationRequest {
        master_pubkey: payload.master_pubkey,
        transport_node_id: payload.transport_node_id,
        server_url: payload.server_url,
        timestamp: payload.timestamp,
        visibility: payload.visibility,
        action: payload.action,
        signature,
    }
}

/// Send a signed registration with `visibility` to the server at `url` and record the
/// server as registered.
async fn register_inner(
    http_client: &reqwest::Client,
    storage: &Storage,
    identity: &crate::state::SharedIdentity,
    url: &str,
    visibility: Visibility,
) -> CmdResult<()> {
    let request = build_registration_request(identity, url, visibility, None).await;
    let resp = http_client
        .post(format!("{url}/api/v1/register"))
        .json(&request)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(format!("registration failed: {}", describe_failure(resp).await).into());
    }
    storage
        .mark_server_registered(url, &visibility.to_string())
        .await
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
    let url = stored_server_url(&state.storage, &url).await?;
    register_inner(
        &state.http_client,
        &state.storage,
        &state.identity,
        &url,
        vis,
    )
    .await?;
    if let Err(e) =
        sync_profile_to_server_inner(&state.http_client, &state.storage, &state.identity, &url)
            .await
    {
        // A profile the server refuses ends the registration; any other failure is retried
        // by the server profile task.
        if state
            .storage
            .get_server(&url)
            .await?
            .is_some_and(|server| server.registered_at.is_none())
        {
            return Err(e);
        }
        log::warn!("[server] profile sync to {url} after registration failed: {e}");
    }
    Ok(())
}

#[tauri::command]
pub async fn unregister_from_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<()> {
    let url = stored_server_url(&state.storage, &url).await?;
    let request = build_registration_request(
        &state.identity,
        &url,
        Visibility::Public,
        Some(REGISTRATION_ACTION_UNREGISTER),
    )
    .await;
    let resp = state
        .http_client
        .delete(format!("{url}/api/v1/register"))
        .json(&request)
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(format!("unregistration failed: {}", describe_failure(resp).await).into());
    }
    state.storage.mark_server_unregistered(&url).await
}

/// Posts from the server's feed. Each post has passed `verified_post`.
#[tauri::command]
pub async fn server_get_feed(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
    before: Option<i64>,
) -> CmdResult<ServerFeedResponse> {
    let url = stored_server_url(&state.storage, &url).await?;
    let mut endpoint = format!(
        "{url}/api/v1/feed?limit={}",
        limit.unwrap_or(50).clamp(1, MAX_POST_PAGE)
    );
    if let Some(b) = before.filter(|b| *b >= 0) {
        endpoint.push_str(&format!("&before={b}"));
    }
    let response: ServerFeedResponse =
        get_json(&state.http_client, endpoint, "feed request").await?;
    Ok(ServerFeedResponse {
        posts: check_server_posts(response.posts),
    })
}

#[tauri::command]
pub async fn server_get_trending(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
) -> CmdResult<TrendingResponse> {
    let url = stored_server_url(&state.storage, &url).await?;
    let endpoint = format!(
        "{url}/api/v1/trending?limit={}",
        limit.unwrap_or(10).clamp(1, MAX_LIST_PAGE)
    );
    get_json(&state.http_client, endpoint, "trending request").await
}

/// Return an error if a search query is longer than `MAX_SEARCH_QUERY_LEN` bytes.
fn check_search_query(query: &str) -> CmdResult<()> {
    if query.len() > MAX_SEARCH_QUERY_LEN {
        return Err(format!("search query is longer than {MAX_SEARCH_QUERY_LEN} bytes").into());
    }
    Ok(())
}

#[tauri::command]
pub async fn server_search_users(
    state: State<'_, Arc<AppState>>,
    url: String,
    query: String,
    limit: Option<i64>,
) -> CmdResult<UserSearchResponse> {
    check_search_query(&query)?;
    let url = stored_server_url(&state.storage, &url).await?;
    let endpoint = format!(
        "{url}/api/v1/users/search?q={}&limit={}",
        urlencoding::encode(&query),
        limit.unwrap_or(20).clamp(1, MAX_LIST_PAGE)
    );
    let response: UserSearchResponse =
        get_json(&state.http_client, endpoint, "user search").await?;
    let users = check_server_users(response.users);
    Ok(UserSearchResponse {
        total: users.len(),
        users,
        query,
    })
}

/// Posts matching `query` on the server. Each post has passed `verified_post`.
#[tauri::command]
pub async fn server_search_posts(
    state: State<'_, Arc<AppState>>,
    url: String,
    query: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<PostSearchResponse> {
    check_search_query(&query)?;
    let url = stored_server_url(&state.storage, &url).await?;
    let endpoint = format!(
        "{url}/api/v1/posts/search?q={}&limit={}&offset={}",
        urlencoding::encode(&query),
        limit.unwrap_or(20).clamp(1, MAX_LIST_PAGE),
        offset.unwrap_or(0).max(0)
    );
    let response: PostSearchResponse =
        get_json(&state.http_client, endpoint, "post search").await?;
    Ok(PostSearchResponse {
        posts: check_server_posts(response.posts),
        total: response.total.max(0),
        query,
    })
}

/// One page of the server's user list. `total` is the number of users the server reports.
#[tauri::command]
pub async fn server_list_users(
    state: State<'_, Arc<AppState>>,
    url: String,
    limit: Option<i64>,
    offset: Option<i64>,
) -> CmdResult<UserSearchResponse> {
    #[derive(Deserialize)]
    struct UserListResponse {
        users: Vec<ServerUser>,
        total: Option<u64>,
    }

    let url = stored_server_url(&state.storage, &url).await?;
    let endpoint = format!(
        "{url}/api/v1/users?limit={}&offset={}",
        limit.unwrap_or(20).clamp(1, MAX_LIST_PAGE),
        offset.unwrap_or(0).max(0)
    );
    let response: UserListResponse = get_json(&state.http_client, endpoint, "user list").await?;
    let users = check_server_users(response.users);
    let total = response
        .total
        .and_then(|total| usize::try_from(total).ok())
        .unwrap_or(users.len());
    Ok(UserSearchResponse {
        total,
        users,
        query: String::new(),
    })
}

#[tauri::command]
pub async fn sync_profile_to_server(state: State<'_, Arc<AppState>>, url: String) -> CmdResult<()> {
    sync_profile_to_server_inner(&state.http_client, &state.storage, &state.identity, &url).await
}

/// Send the stored own profile to a server the user is listed on, as a signed
/// `ProfileUpdateRequest`, and record its `updated_at` and the time as accepted by that
/// server. Called from `save_my_profile`, `register_with_server`, the
/// `sync_profile_to_server` command and the task that sends the profile to servers that
/// lack its current version or did not receive it within `SERVER_PROFILE_REFRESH`.
///
/// The server answers 410 if it expired the registration for inactivity; then the
/// registration is sent again with the stored visibility, and the profile after it. It
/// answers 404 if it holds no registration (also after an unregistration from another
/// device of the user) and 403 if the profile ended the registration (a Private
/// profile). Then the server is recorded as not registered, so the profile is not sent to
/// it again, and an error is returned.
pub(crate) async fn sync_profile_to_server_inner(
    http_client: &reqwest::Client,
    storage: &Storage,
    identity: &crate::state::SharedIdentity,
    url: &str,
) -> CmdResult<()> {
    let url = stored_server_url(storage, url).await?;
    let mut registered_again = false;
    loop {
        // Signs the stored profile again if its signature does not verify under the
        // master key.
        let profile = crate::announcement::build_own_identity_response(storage, identity)
            .await?
            .profile
            .ok_or("no profile to sync")?;
        let (master_pubkey, master_key) = {
            let id = identity.read().await;
            (id.master_pubkey.clone(), id.master_key.clone())
        };
        let timestamp = now_millis();
        let updated_at = profile.updated_at;
        let signature = sign_profile_update(&master_pubkey, &url, timestamp, &profile, &master_key);
        let request = ProfileUpdateRequest {
            master_pubkey,
            server_url: url.clone(),
            timestamp,
            profile,
            signature,
        };

        let resp = http_client
            .put(format!("{url}/api/v1/register"))
            .json(&request)
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::GONE && !registered_again {
            let visibility: Visibility = storage
                .get_server(&url)
                .await?
                .ok_or("unknown server")?
                .visibility
                .parse()
                .map_err(|_: String| "invalid stored visibility")?;
            log::info!("[server] {url} expired the registration; registering again");
            if let Err(e) = register_inner(http_client, storage, identity, &url, visibility).await {
                storage.mark_server_unregistered(&url).await?;
                return Err(format!(
                    "the server expired the registration and refused a new one: {e}"
                )
                .into());
            }
            registered_again = true;
            continue;
        }
        if status == reqwest::StatusCode::NOT_FOUND
            || status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::GONE
        {
            let failure = describe_failure(resp).await;
            storage.mark_server_unregistered(&url).await?;
            return Err(format!("the server removed the registration: {failure}").into());
        }
        if !status.is_success() {
            return Err(format!("profile sync failed: {}", describe_failure(resp).await).into());
        }
        return storage
            .mark_server_profile_synced(&url, updated_at, now_millis())
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn server_url_is_normalized() {
        assert_eq!(
            normalize_server_url(" https://Example.COM/ ").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_server_url("http://127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            normalize_server_url("https://example.com:443/base/").unwrap(),
            "https://example.com/base"
        );
    }

    #[test]
    fn server_url_rejects_other_schemes_and_parts() {
        for bad in [
            "foo",
            "",
            "ftp://example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "example.com:3000",
            "https://user:pw@example.com",
            "https://example.com/?q=1",
            "https://example.com/#frag",
        ] {
            assert!(normalize_server_url(bad).is_err(), "accepted {bad:?}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_SERVER_URL_LEN));
        assert!(normalize_server_url(&long).is_err());
    }

    #[test]
    fn server_message_is_sanitized_and_truncated() {
        assert_eq!(sanitize_message("a\n\tb\u{0}  c"), "a b c");
        let long = "\u{65e5}".repeat(MAX_ERROR_MESSAGE_CHARS * 2);
        assert_eq!(
            sanitize_message(&long).chars().count(),
            MAX_ERROR_MESSAGE_CHARS
        );
    }

    fn signed_server_post(key: &SecretKey, content: &str) -> ServerFeedPost {
        let mut post = Post {
            id: String::new(),
            author: key.public().to_string(),
            content: content.to_string(),
            timestamp: now_millis(),
            media: vec![proscenium_types::MediaAttachment {
                hash: "h".to_string(),
                ticket: "t".to_string(),
                mime_type: "image/png".to_string(),
                filename: "a.png".to_string(),
                size: 1,
            }],
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: String::new(),
        };
        proscenium_types::sign_post(&mut post, key);
        ServerFeedPost {
            id: post.id,
            author: post.author,
            content: post.content,
            timestamp: post.timestamp as i64,
            media_json: Some(serde_json::to_string(&post.media).unwrap()),
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: post.signature,
            indexed_at: 0,
        }
    }

    #[test]
    fn server_post_is_rebuilt_and_verified() {
        let key = SecretKey::from_bytes(&[4u8; 32]);
        let server_post = signed_server_post(&key, "hello");
        assert!(verified_post(&server_post).is_some());

        // A signature that its author did not make.
        let mut forged = signed_server_post(&key, "hello");
        forged.signature =
            signed_server_post(&SecretKey::from_bytes(&[5u8; 32]), "hello").signature;
        assert!(verified_post(&forged).is_none());

        // Repeated ids are dropped.
        let checked = check_server_posts(vec![server_post.clone(), server_post, forged]);
        assert_eq!(checked.len(), 1);
    }

    #[test]
    fn server_post_with_changed_fields_is_dropped() {
        let key = SecretKey::from_bytes(&[4u8; 32]);

        // Content that the id does not cover.
        let mut changed = signed_server_post(&key, "hello");
        changed.content = "other text".to_string();
        assert!(verified_post(&changed).is_none());

        // Author replaced by another identity.
        let mut reattributed = signed_server_post(&key, "hello");
        reattributed.author = SecretKey::from_bytes(&[5u8; 32]).public().to_string();
        assert!(verified_post(&reattributed).is_none());

        let mut negative_time = signed_server_post(&key, "hello");
        negative_time.timestamp = -1;
        assert!(verified_post(&negative_time).is_none());

        let mut bad_media = signed_server_post(&key, "hello");
        bad_media.media_json = Some("{not json".to_string());
        assert!(verified_post(&bad_media).is_none());

        // The server stores no `media_json` for a post without media; here the signed
        // post had one attachment, so the rebuilt post has a different id.
        let mut dropped_media = signed_server_post(&key, "hello");
        dropped_media.media_json = None;
        assert!(verified_post(&dropped_media).is_none());
    }

    /// Serve `response` (a complete HTTP/1.1 response) to every connection on an ephemeral
    /// local port. Returns the base URL.
    async fn serve_canned_response(response: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let response = response.clone();
                tokio::spawn(async move {
                    // Read the request head, then answer and close.
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut buf).await {
                            Ok(n) if n > 0 => request.extend_from_slice(&buf[..n]),
                            _ => break,
                        }
                    }
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        url
    }

    /// HTTP client for tests. reqwest requires a process-wide rustls crypto provider,
    /// which `run()` installs in the app.
    fn test_client() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::new()
    }

    fn http_response(status: &str, content_length: bool, body: &[u8]) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        if content_length {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        let mut response = head.into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[tokio::test]
    async fn response_body_is_read_up_to_the_limit() {
        let client = test_client();
        for content_length in [true, false] {
            let url =
                serve_canned_response(http_response("200 OK", content_length, &[b'a'; 5000])).await;

            let resp = client.get(&url).send().await.unwrap();
            assert_eq!(read_body(resp, 5000).await.unwrap().len(), 5000);

            let resp = client.get(&url).send().await.unwrap();
            assert!(read_body(resp, 4999).await.is_err());
        }
    }

    #[tokio::test]
    async fn json_response_with_unexpected_format_does_not_echo_the_body() {
        let client = test_client();
        let url = serve_canned_response(http_response(
            "200 OK",
            true,
            br#"{"name": "<script>secret</script>"}"#,
        ))
        .await;
        let err = get_json::<ServerInfo>(&client, url, "server info request")
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains("script"), "{err}");

        let info = serde_json::json!({
            "name": "n".repeat(500), "description": "d", "version": "1", "node_id": "x",
            "registered_users": 1, "total_posts": 2, "uptime_seconds": 3,
            "registration_open": true,
        });
        let url =
            serve_canned_response(http_response("200 OK", true, info.to_string().as_bytes())).await;
        let info = fetch_server_info(&client, &url).await.unwrap();
        assert_eq!(info.name.chars().count(), MAX_SERVER_NAME_CHARS);
        // "x" is not a public key.
        assert_eq!(info.node_id, "");
    }

    #[tokio::test]
    async fn failure_description_holds_status_and_bounded_message() {
        let client = test_client();

        let body =
            serde_json::json!({ "message": format!("bad\n\u{7}request {}", "x".repeat(5000)) });
        let url = serve_canned_response(http_response(
            "400 Bad Request",
            true,
            body.to_string().as_bytes(),
        ))
        .await;
        let resp = client.get(&url).send().await.unwrap();
        // The JSON is cut at MAX_ERROR_BODY_BYTES and no longer parses, so the start of
        // the text is used.
        let text = describe_failure(resp).await;
        assert!(text.starts_with("400 Bad Request: "), "{text}");
        assert!(text.chars().count() <= "400 Bad Request: ".len() + MAX_ERROR_MESSAGE_CHARS);
        assert!(!text.chars().any(char::is_control));

        let url = serve_canned_response(http_response(
            "403 Forbidden",
            true,
            br#"{"message":"registration is closed"}"#,
        ))
        .await;
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(
            describe_failure(resp).await,
            "403 Forbidden: registration is closed"
        );

        let url =
            serve_canned_response(http_response("500 Internal Server Error", true, b"")).await;
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(describe_failure(resp).await, "500 Internal Server Error");
    }

    #[test]
    fn server_users_with_invalid_keys_are_dropped() {
        let key = SecretKey::from_bytes(&[3u8; 32]).public().to_string();
        let user = |pubkey: &str, node: Option<&str>| ServerUser {
            pubkey: pubkey.to_string(),
            display_name: None,
            bio: None,
            avatar_hash: None,
            visibility: "public".to_string(),
            registered_at: 0,
            post_count: 0,
            latest_post_at: None,
            transport_node_id: node.map(str::to_string),
        };
        let checked = check_server_users(vec![
            user(&key, Some("not a key")),
            user("not a key", None),
            user(&key, None),
        ]);
        assert_eq!(checked.len(), 1);
        assert_eq!(checked[0].pubkey, key);
        assert_eq!(checked[0].transport_node_id, None);
    }

    /// Answer the requests on an ephemeral local port with `responses` in order, one per
    /// connection, after reading each request with its body. Returns the base URL and the
    /// methods of the requests received.
    async fn serve_responses(
        responses: Vec<Vec<u8>>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let methods = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = methods.clone();
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                let head_end = loop {
                    if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                };
                let head = String::from_utf8_lossy(&request[..head_end]).to_lowercase();
                let body_len: usize = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map_or(0, |len| len.trim().parse().unwrap());
                while request.len() < head_end + body_len {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                }
                seen.lock()
                    .unwrap()
                    .push(head.split(' ').next().unwrap().to_uppercase());
                socket.write_all(&response).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        (url, methods)
    }

    /// A storage with the own profile and the server at `url` recorded as registered.
    async fn registered_storage(url: &str) -> (Storage, crate::state::SharedIdentity, u64) {
        let identity = Arc::new(tokio::sync::RwLock::new(crate::state::Identity::new(
            SecretKey::from_bytes(&[7u8; 32]),
            SecretKey::from_bytes(&[8u8; 32]),
        )));
        let storage = Storage::open_in_memory().await.unwrap();
        let master_pubkey = identity.read().await.master_pubkey.clone();
        let profile = proscenium_types::Profile {
            display_name: "name".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Public,
            updated_at: 1_000,
            signature: String::new(),
        };
        storage
            .save_profile(&master_pubkey, &profile)
            .await
            .unwrap();
        storage.add_server(url, "", "", "").await.unwrap();
        storage.mark_server_registered(url, "public").await.unwrap();
        // The profile is signed when it is sent, one millisecond newer.
        (storage, identity, 1_001)
    }

    /// A server that expired the registration (410) receives a new registration, then
    /// the profile.
    #[tokio::test]
    async fn expired_registration_is_sent_again_before_the_profile() {
        let (url, methods) = serve_responses(vec![
            http_response("410 Gone", true, br#"{"message":"expired"}"#),
            http_response("201 Created", true, b"{}"),
            http_response("200 OK", true, b"{}"),
        ])
        .await;
        let (storage, identity, updated_at) = registered_storage(&url).await;

        sync_profile_to_server_inner(&test_client(), &storage, &identity, &url)
            .await
            .unwrap();
        assert_eq!(*methods.lock().unwrap(), vec!["PUT", "POST", "PUT"]);
        let server = storage.get_server(&url).await.unwrap().unwrap();
        assert!(server.registered_at.is_some());
        assert!(
            storage
                .get_servers_to_send_profile(updated_at, now_millis() - 60_000)
                .await
                .unwrap()
                .is_empty(),
            "the server accepted the current profile now"
        );
    }

    /// A server without the registration (404), as after an unregistration from another
    /// device of the user, is recorded as not registered and receives nothing more.
    #[tokio::test]
    async fn missing_registration_is_recorded_as_not_registered() {
        let (url, methods) = serve_responses(vec![http_response(
            "404 Not Found",
            true,
            br#"{"message":"user not registered"}"#,
        )])
        .await;
        let (storage, identity, _) = registered_storage(&url).await;

        assert!(
            sync_profile_to_server_inner(&test_client(), &storage, &identity, &url)
                .await
                .is_err()
        );
        assert_eq!(*methods.lock().unwrap(), vec!["PUT"]);
        let server = storage.get_server(&url).await.unwrap().unwrap();
        assert!(server.registered_at.is_none());
    }
}
