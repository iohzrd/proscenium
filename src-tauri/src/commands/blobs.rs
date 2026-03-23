use crate::error::{AppError, CmdResult};
use crate::state::AppState;
use iroh::Endpoint;
use iroh_blobs::{BlobsProtocol, HashAndFormat, store::fs::FsStore, ticket::BlobTicket};
use proscenium_types::MAX_BLOB_SIZE;
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
    let ep = state.endpoint();
    add_blob_data(&state.blob_store, &ep, content.as_bytes()).await
}

#[tauri::command]
pub async fn fetch_blob(state: State<'_, Arc<AppState>>, ticket: String) -> CmdResult<String> {
    let ticket = parse_ticket(&ticket)?;
    log::info!("[blob] fetching text blob {}...", ticket.hash());
    let ep = state.endpoint();
    let blobs = state.blobs();
    let bytes = fetch_blob_data(&ep, &blobs, &state.blob_store, &ticket).await?;
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
    let ep = state.endpoint();
    add_blob_data(&state.blob_store, &ep, &data).await
}

/// Add a blob from RGBA pixel data (used by clipboard paste).
/// Returns `{ hash, ticket, filename, size, mime_type }`.
#[tauri::command]
pub async fn add_blob_from_rgba(
    state: State<'_, Arc<AppState>>,
    data: Vec<u8>,
    width: u32,
    height: u32,
) -> CmdResult<serde_json::Value> {
    let png_data = tokio::task::spawn_blocking(move || {
        let mut buf = Vec::new();
        let mut encoder = png::Encoder::new(std::io::Cursor::new(&mut buf), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| AppError::Other(format!("png encode error: {e}")))?;
        writer
            .write_image_data(&data)
            .map_err(|e| AppError::Other(format!("png write error: {e}")))?;
        drop(writer);
        Ok::<Vec<u8>, AppError>(buf)
    })
    .await
    .map_err(|e| AppError::Other(format!("task join error: {e}")))??;

    let size = png_data.len();
    let ep = state.endpoint();
    let blob_result = add_blob_data(&state.blob_store, &ep, &png_data).await?;
    let hash = blob_result["hash"].as_str().unwrap_or_default().to_string();
    let ticket = blob_result["ticket"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Ok(serde_json::json!({
        "hash": hash,
        "ticket": ticket,
        "filename": "clipboard.png",
        "size": size,
        "mime_type": "image/png",
    }))
}

/// Add a blob from a local file path (used by native drag-and-drop).
/// Returns `{ hash, ticket, filename, size, mime_type }`.
#[tauri::command]
pub async fn add_blob_from_path(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> CmdResult<serde_json::Value> {
    let file_path = std::path::Path::new(&path);
    let data = tokio::fs::read(file_path)
        .await
        .map_err(|e| AppError::Other(format!("failed to read {path}: {e}")))?;
    let filename = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let size = data.len();
    let mime_type = mime_guess::from_path(file_path)
        .first_or_octet_stream()
        .to_string();
    let ep = state.endpoint();
    let blob_result = add_blob_data(&state.blob_store, &ep, &data).await?;
    let hash = blob_result["hash"].as_str().unwrap_or_default().to_string();
    let ticket = blob_result["ticket"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Ok(serde_json::json!({
        "hash": hash,
        "ticket": ticket,
        "filename": filename,
        "size": size,
        "mime_type": mime_type,
    }))
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
    let ep = state.endpoint();
    let blobs = state.blobs();
    let bytes = fetch_blob_data(&ep, &blobs, &state.blob_store, &ticket).await?;
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
    let ep = state.endpoint();
    let blobs = state.blobs();
    let bytes = fetch_blob_data(&ep, &blobs, &state.blob_store, &ticket).await?;
    log::info!(
        "[blob] re-fetched {} from remote ({} bytes)",
        ticket.hash(),
        bytes.len()
    );
    Ok(bytes)
}
