use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_blobs::{HashAndFormat, ticket::BlobTicket};
use iroh_social_types::MAX_BLOB_SIZE;
use std::sync::Arc;
use tauri::State;

async fn add_blob_inner(state: &AppState, data: &[u8]) -> Result<serde_json::Value, String> {
    if data.len() > MAX_BLOB_SIZE {
        return Err(format!(
            "blob too large: {} bytes (max {} bytes)",
            data.len(),
            MAX_BLOB_SIZE
        ));
    }

    let size = data.len();
    let tag = state.store.add_slice(data).await.str_err()?;

    let addr = state.endpoint.addr();
    let ticket = BlobTicket::new(addr, tag.hash, tag.format);
    log::info!("[blob] added blob {} ({size} bytes)", tag.hash);

    Ok(serde_json::json!({
        "hash": tag.hash.to_string(),
        "ticket": ticket.to_string(),
    }))
}

async fn fetch_blob_inner(state: &AppState, ticket: &BlobTicket) -> Result<Vec<u8>, String> {
    let conn = state
        .endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .str_err()?;

    let hash_and_format: HashAndFormat = ticket.hash_and_format();
    state
        .blobs
        .remote()
        .fetch(conn, hash_and_format)
        .await
        .str_err()?;

    let bytes = state.store.get_bytes(ticket.hash()).await.str_err()?;
    Ok(bytes.to_vec())
}

#[tauri::command]
pub async fn add_blob(
    state: State<'_, Arc<AppState>>,
    content: String,
) -> Result<serde_json::Value, String> {
    add_blob_inner(&state, content.as_bytes()).await
}

#[tauri::command]
pub async fn fetch_blob(state: State<'_, Arc<AppState>>, ticket: String) -> Result<String, String> {
    let ticket: BlobTicket = ticket.parse().str_err()?;
    log::info!("[blob] fetching text blob {}...", ticket.hash());
    let bytes = fetch_blob_inner(&state, &ticket).await?;
    log::info!(
        "[blob] fetched text blob {} ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    String::from_utf8(bytes).str_err()
}

#[tauri::command]
pub async fn add_blob_bytes(
    state: State<'_, Arc<AppState>>,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    add_blob_inner(&state, &data).await
}

#[tauri::command]
pub async fn fetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> Result<Vec<u8>, String> {
    let ticket: BlobTicket = ticket.parse().str_err()?;

    if let Ok(bytes) = state.store.get_bytes(ticket.hash()).await {
        return Ok(bytes.to_vec());
    }

    log::info!("[blob] fetching {} from remote...", ticket.hash());
    let bytes = fetch_blob_inner(&state, &ticket).await?;
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
) -> Result<Vec<u8>, String> {
    let ticket: BlobTicket = ticket.parse().str_err()?;
    log::info!(
        "[blob] re-fetching {} from remote (ignoring local cache)...",
        ticket.hash()
    );
    let bytes = fetch_blob_inner(&state, &ticket).await?;
    log::info!(
        "[blob] re-fetched {} from remote ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    Ok(bytes)
}
