use crate::ext::ResultExt;
use crate::state::AppState;
use iroh_blobs::{HashAndFormat, ticket::BlobTicket};
use iroh_social_types::MAX_BLOB_SIZE;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn add_blob(
    state: State<'_, Arc<AppState>>,
    content: String,
) -> Result<serde_json::Value, String> {
    if content.len() > MAX_BLOB_SIZE {
        return Err(format!(
            "blob too large: {} bytes (max {} bytes)",
            content.len(),
            MAX_BLOB_SIZE
        ));
    }

    let tag = state.store.add_slice(content.as_bytes()).await.str_err()?;

    let addr = state.endpoint.addr();
    let ticket = BlobTicket::new(addr, tag.hash, tag.format);
    log::info!("[blob] added text blob {}", tag.hash);

    Ok(serde_json::json!({
        "hash": tag.hash.to_string(),
        "ticket": ticket.to_string(),
    }))
}

#[tauri::command]
pub async fn fetch_blob(state: State<'_, Arc<AppState>>, ticket: String) -> Result<String, String> {
    let ticket: BlobTicket = ticket.parse().str_err()?;
    let store = state.store.clone();
    let endpoint = state.endpoint.clone();
    let blobs = state.blobs.clone();

    log::info!("[blob] fetching text blob {}...", ticket.hash());
    let conn = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .str_err()?;

    let hash_and_format: HashAndFormat = ticket.hash_and_format();
    blobs
        .remote()
        .fetch(conn, hash_and_format)
        .await
        .str_err()?;

    let bytes = store.get_bytes(ticket.hash()).await.str_err()?;

    log::info!(
        "[blob] fetched text blob {} ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    String::from_utf8(bytes.to_vec()).str_err()
}

#[tauri::command]
pub async fn add_blob_bytes(
    state: State<'_, Arc<AppState>>,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    if data.len() > MAX_BLOB_SIZE {
        return Err(format!(
            "blob too large: {} bytes (max {} bytes)",
            data.len(),
            MAX_BLOB_SIZE
        ));
    }

    let size = data.len();
    let tag = state.store.add_slice(&data).await.str_err()?;

    let addr = state.endpoint.addr();
    let ticket = BlobTicket::new(addr, tag.hash, tag.format);
    log::info!("[blob] added blob {} ({size} bytes)", tag.hash);

    Ok(serde_json::json!({
        "hash": tag.hash.to_string(),
        "ticket": ticket.to_string(),
    }))
}

#[tauri::command]
pub async fn fetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> Result<Vec<u8>, String> {
    let ticket: BlobTicket = ticket.parse().str_err()?;
    let store = state.store.clone();
    let endpoint = state.endpoint.clone();
    let blobs = state.blobs.clone();

    if let Ok(bytes) = store.get_bytes(ticket.hash()).await {
        return Ok(bytes.to_vec());
    }

    log::info!("[blob] fetching {} from remote...", ticket.hash());
    let conn = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .str_err()?;

    let hash_and_format: HashAndFormat = ticket.hash_and_format();
    blobs
        .remote()
        .fetch(conn, hash_and_format)
        .await
        .str_err()?;

    let bytes = store.get_bytes(ticket.hash()).await.str_err()?;

    log::info!(
        "[blob] fetched {} from remote ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    Ok(bytes.to_vec())
}
