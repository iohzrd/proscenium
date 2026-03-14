use crate::error::{AppError, CmdResult};
use crate::state::AppState;
use iroh::Endpoint;
use iroh_blobs::{BlobsProtocol, HashAndFormat, store::fs::FsStore, ticket::BlobTicket};
use iroh_social_types::MAX_BLOB_SIZE;
use std::sync::Arc;
use tauri::State;

fn parse_ticket(s: &str) -> Result<BlobTicket, AppError> {
    s.parse::<BlobTicket>()
        .map_err(|e| AppError::Other(format!("{e}")))
}

async fn add_blob_data(
    blob_store: &FsStore,
    endpoint: &Endpoint,
    data: &[u8],
) -> CmdResult<serde_json::Value> {
    if data.len() > MAX_BLOB_SIZE {
        return Err(format!(
            "blob too large: {} bytes (max {} bytes)",
            data.len(),
            MAX_BLOB_SIZE
        )
        .into());
    }
    let size = data.len();
    let tag = blob_store.add_slice(data).await?;
    let addr = endpoint.addr();
    let ticket = BlobTicket::new(addr, tag.hash, tag.format);
    log::info!("[blob] added blob {} ({size} bytes)", tag.hash);
    Ok(serde_json::json!({
        "hash": tag.hash.to_string(),
        "ticket": ticket.to_string(),
    }))
}

async fn fetch_blob_data(
    endpoint: &Endpoint,
    blobs: &BlobsProtocol,
    blob_store: &FsStore,
    ticket: &BlobTicket,
) -> CmdResult<Vec<u8>> {
    let conn = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await?;
    let hash_and_format: HashAndFormat = ticket.hash_and_format();
    blobs.remote().fetch(conn, hash_and_format).await?;
    let bytes = blob_store.get_bytes(ticket.hash()).await?;
    Ok(bytes.to_vec())
}

#[tauri::command]
pub async fn add_blob(
    state: State<'_, Arc<AppState>>,
    content: String,
) -> CmdResult<serde_json::Value> {
    add_blob_data(&state.blob_store, &state.endpoint, content.as_bytes()).await
}

#[tauri::command]
pub async fn fetch_blob(state: State<'_, Arc<AppState>>, ticket: String) -> CmdResult<String> {
    let ticket = parse_ticket(&ticket)?;
    log::info!("[blob] fetching text blob {}...", ticket.hash());
    let bytes = fetch_blob_data(&state.endpoint, &state.blobs, &state.blob_store, &ticket).await?;
    log::info!(
        "[blob] fetched text blob {} ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    let text = String::from_utf8(bytes)?;
    Ok(text)
}

#[tauri::command]
pub async fn add_blob_bytes(
    state: State<'_, Arc<AppState>>,
    data: Vec<u8>,
) -> CmdResult<serde_json::Value> {
    add_blob_data(&state.blob_store, &state.endpoint, &data).await
}

#[tauri::command]
pub async fn fetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> CmdResult<Vec<u8>> {
    let ticket = parse_ticket(&ticket)?;
    if let Ok(bytes) = state.blob_store.get_bytes(ticket.hash()).await {
        return Ok(bytes.to_vec());
    }
    log::info!("[blob] fetching {} from remote...", ticket.hash());
    let bytes = fetch_blob_data(&state.endpoint, &state.blobs, &state.blob_store, &ticket).await?;
    log::info!(
        "[blob] fetched {} from remote ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    Ok(bytes)
}

#[tauri::command]
pub async fn refetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> CmdResult<Vec<u8>> {
    let ticket = parse_ticket(&ticket)?;
    log::info!(
        "[blob] re-fetching {} from remote (ignoring local cache)...",
        ticket.hash()
    );
    let bytes = fetch_blob_data(&state.endpoint, &state.blobs, &state.blob_store, &ticket).await?;
    log::info!(
        "[blob] re-fetched {} from remote ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    Ok(bytes)
}
