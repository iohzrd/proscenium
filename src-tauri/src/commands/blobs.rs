use crate::error::{AppError, CmdResult};
use crate::state::AppState;
use futures_lite::StreamExt;
use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::api::remote::{GetProgressItem, Remote};
use iroh_blobs::get::request::{get_blob, get_verified_size};
use iroh_blobs::{BlobFormat, Hash, HashAndFormat, store::fs::FsStore, ticket::BlobTicket};
use proscenium_types::{MAX_BLOB_SIZE, MAX_BLOB_TICKET_LEN};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::ipc::Response;
use tauri::{Manager, State};
use tokio::io::AsyncReadExt;

/// Time limit for connecting to the node named in a ticket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Time limit for the size request that precedes a download.
const SIZE_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// A download into the store is aborted if no progress report arrives for this long.
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Time limit for one download.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Number of dropped paths remembered. The oldest entry is removed when the list is full.
const MAX_DROPPED_PATHS: usize = 256;
/// Largest RGBA pixel buffer `add_blob_from_rgba` encodes. PNG encoding shrinks typical
/// screen content several times; the encoded size is checked against `MAX_BLOB_SIZE`
/// when the blob is added.
const MAX_RGBA_BYTES: usize = 4 * MAX_BLOB_SIZE;

/// File paths the operating system delivered through drag-and-drop onto a window.
///
/// `add_blob_from_path` reads only paths in this list, so the webview cannot name an
/// arbitrary file. Managed as Tauri state; filled by `record_dropped_paths`.
#[derive(Default)]
pub struct DroppedPaths(Mutex<VecDeque<PathBuf>>);

impl DroppedPaths {
    fn record(&self, paths: &[PathBuf]) {
        let Ok(mut list) = self.0.lock() else {
            return;
        };
        for path in paths {
            if list.contains(path) {
                continue;
            }
            if list.len() >= MAX_DROPPED_PATHS {
                list.pop_front();
            }
            list.push_back(path.clone());
        }
    }

    fn contains(&self, path: &Path) -> bool {
        self.0
            .lock()
            .is_ok_and(|list| list.iter().any(|p| p == path))
    }
}

/// Window event handler: stores the paths of a drag-and-drop `Drop` event in the managed
/// `DroppedPaths`. Tauri calls window event handlers in the same event-loop callback that
/// emits the event to the webview, so a path is recorded before the frontend can invoke
/// `add_blob_from_path` for it.
pub fn record_dropped_paths(window: &tauri::Window, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event
        && let Some(dropped) = window.try_state::<DroppedPaths>()
    {
        dropped.record(paths);
    }
}

fn check_size_limit(size: u64, max_size: u64) -> Result<(), AppError> {
    if size > max_size {
        return Err(format!("blob too large: {size} bytes (max {max_size} bytes)").into());
    }
    Ok(())
}

fn check_blob_size(size: u64) -> Result<(), AppError> {
    check_size_limit(size, MAX_BLOB_SIZE as u64)
}

/// Parse a ticket for a single raw blob. Hash sequences (collections) are rejected: their
/// total size is not bounded by the size of the root blob.
fn parse_ticket(s: &str) -> Result<BlobTicket, AppError> {
    if s.len() > MAX_BLOB_TICKET_LEN {
        return Err("blob ticket is too long".into());
    }
    let ticket = s
        .parse::<BlobTicket>()
        .map_err(|e| AppError::Other(format!("invalid blob ticket: {e}")))?;
    if ticket.format() != BlobFormat::Raw {
        return Err("blob ticket does not name a single raw blob".into());
    }
    Ok(ticket)
}

/// Add `data` to the blob store. Returns `{ hash, ticket }`.
async fn add_blob_data(
    blob_store: &FsStore,
    endpoint: &Endpoint,
    data: Vec<u8>,
) -> CmdResult<serde_json::Value> {
    let size = data.len();
    check_blob_size(size as u64)?;
    let tag = blob_store.add_bytes(data).await?;
    let addr = endpoint.addr();
    let ticket = BlobTicket::new(addr, tag.hash, tag.format);
    log::info!("[blob] added blob {} ({size} bytes)", tag.hash);
    Ok(serde_json::json!({
        "hash": tag.hash.to_string(),
        "ticket": ticket.to_string(),
    }))
}

/// Add `data` to the blob store. Returns `{ hash, ticket, filename, size, mime_type }`.
async fn add_named_blob(
    state: &AppState,
    data: Vec<u8>,
    filename: &str,
    mime_type: &str,
) -> CmdResult<serde_json::Value> {
    let size = data.len();
    let mut result = add_blob_data(&state.blob_store, &state.endpoint(), data).await?;
    if let Some(fields) = result.as_object_mut() {
        fields.insert("filename".into(), filename.into());
        fields.insert("size".into(), size.into());
        fields.insert("mime_type".into(), mime_type.into());
    }
    Ok(result)
}

/// Read the blob from the local store. Returns `None` unless the store holds all of it.
async fn read_local_blob(blob_store: &FsStore, hash: Hash) -> CmdResult<Option<Vec<u8>>> {
    let status = blob_store
        .status(hash)
        .await
        .map_err(|e| AppError::Other(format!("blob store status request failed: {e}")))?;
    match status {
        BlobStatus::Complete { size } => {
            check_blob_size(size)?;
            let bytes = blob_store.get_bytes(hash).await?;
            Ok(Some(bytes.into()))
        }
        BlobStatus::Partial { .. } | BlobStatus::NotFound => Ok(None),
    }
}

/// Connect to the node named in `ticket` and request the size of the blob.
///
/// The provider answers with the last chunk and its hash path, which proves the size for
/// this hash, so a blob above `max_size` is rejected before its content is requested.
async fn connect_and_check_size(
    endpoint: &Endpoint,
    ticket: &BlobTicket,
    max_size: u64,
) -> CmdResult<(Connection, u64)> {
    let hash = ticket.hash();
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        endpoint.connect(ticket.addr().clone(), iroh_blobs::ALPN),
    )
    .await
    .map_err(|_| "timed out connecting to the blob provider")??;
    let (size, _stats) =
        tokio::time::timeout(SIZE_REQUEST_TIMEOUT, get_verified_size(&conn, &hash))
            .await
            .map_err(|_| "timed out requesting the blob size")??;
    check_size_limit(size, max_size)?;
    Ok((conn, size))
}

/// Download the parts of the blob that the local store lacks, from `conn` into the store.
///
/// Returns an error, which stops the transfer, if no progress is reported for
/// `TRANSFER_IDLE_TIMEOUT`, if the whole transfer exceeds `TRANSFER_TIMEOUT`, or if the
/// provider sends more payload than `size`.
async fn download_to_store(
    remote: &Remote,
    conn: Connection,
    hash: Hash,
    size: u64,
) -> CmdResult<()> {
    let progress = remote.fetch(conn, HashAndFormat::raw(hash));
    let mut stream = std::pin::pin!(progress.stream());
    let deadline = tokio::time::Instant::now() + TRANSFER_TIMEOUT;
    loop {
        let item = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + TRANSFER_IDLE_TIMEOUT),
            stream.next(),
        )
        .await
        .map_err(|_| "blob download timed out")?;
        match item {
            Some(GetProgressItem::Progress(received)) if received > size => {
                return Err("blob provider sent more data than the blob size".into());
            }
            Some(GetProgressItem::Progress(_)) => {}
            Some(GetProgressItem::Done(_)) => return Ok(()),
            Some(GetProgressItem::Error(e)) => return Err(e.into()),
            None => return Err("blob download ended without a result".into()),
        }
    }
}

/// Download the whole blob from `conn` into memory, without reading or writing the local
/// store. Every received chunk is verified against the blob hash. Returns an error if the
/// transfer exceeds `TRANSFER_TIMEOUT` or the received length differs from `size`.
async fn download_to_memory(conn: Connection, hash: Hash, size: u64) -> CmdResult<Vec<u8>> {
    let bytes = tokio::time::timeout(TRANSFER_TIMEOUT, get_blob(conn, hash).bytes())
        .await
        .map_err(|_| "blob download timed out")??;
    if bytes.len() as u64 != size {
        return Err("blob provider sent a different amount of data than the blob size".into());
    }
    Ok(bytes.into())
}

#[tauri::command]
pub async fn add_blob_bytes(
    state: State<'_, Arc<AppState>>,
    data: Vec<u8>,
) -> CmdResult<serde_json::Value> {
    add_blob_data(&state.blob_store, &state.endpoint(), data).await
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
    // 4 bytes per pixel.
    let expected_len = u64::from(width) * u64::from(height) * 4;
    if width == 0 || height == 0 || data.len() as u64 != expected_len {
        return Err("pixel data length does not match width x height x 4".into());
    }
    if data.len() > MAX_RGBA_BYTES {
        return Err(format!(
            "image too large: {} bytes of pixel data (max {MAX_RGBA_BYTES} bytes)",
            data.len()
        )
        .into());
    }

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

    add_named_blob(&state, png_data, "clipboard.png", "image/png").await
}

/// Add a blob from a file that the user dropped onto the window.
///
/// `path` must equal a path delivered by a native drag-and-drop event (see
/// `DroppedPaths`); any other path is rejected without touching the file system.
/// The file size is checked against `MAX_BLOB_SIZE` before the file is read.
/// Returns `{ hash, ticket, filename, size, mime_type }`.
#[tauri::command]
pub async fn add_blob_from_path(
    state: State<'_, Arc<AppState>>,
    dropped: State<'_, DroppedPaths>,
    path: String,
) -> CmdResult<serde_json::Value> {
    let file_path = Path::new(&path);
    if !dropped.contains(file_path) {
        return Err("path was not delivered by a drag-and-drop event".into());
    }
    let read_error = |e: std::io::Error| AppError::Other(format!("failed to read {path}: {e}"));

    let file = tokio::fs::File::open(file_path).await.map_err(read_error)?;
    let metadata = file.metadata().await.map_err(read_error)?;
    if !metadata.is_file() {
        return Err(format!("{path} is not a regular file").into());
    }
    check_blob_size(metadata.len())?;

    // The length can change after the check, so the read stops one byte past the limit.
    let mut data = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BLOB_SIZE as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(read_error)?;
    check_blob_size(data.len() as u64)?;

    let filename = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mime_type = mime_guess::from_path(file_path)
        .first_or_octet_stream()
        .to_string();
    add_named_blob(&state, data, &filename, &mime_type).await
}

/// Return the bytes of the blob named by `ticket` as a raw IPC response (an `ArrayBuffer`
/// in the webview).
///
/// A complete local copy is returned without network access. Otherwise the blob is
/// downloaded from the node in the ticket into the local store, after that node proved
/// that its size is at most `MAX_BLOB_SIZE`.
#[tauri::command]
pub async fn fetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> CmdResult<Response> {
    let ticket = parse_ticket(&ticket)?;
    let hash = ticket.hash();
    if let Some(bytes) = read_local_blob(&state.blob_store, hash).await? {
        return Ok(Response::new(bytes));
    }
    log::info!("[blob] fetching {hash} from remote...");
    let (conn, size) =
        connect_and_check_size(&state.endpoint(), &ticket, MAX_BLOB_SIZE as u64).await?;
    download_to_store(state.blobs().remote(), conn, hash, size).await?;
    let bytes = read_local_blob(&state.blob_store, hash)
        .await?
        .ok_or("blob is incomplete after download")?;
    log::info!("[blob] fetched {hash} from remote ({} bytes)", bytes.len());
    Ok(Response::new(bytes))
}

/// Download the blob named by `ticket` from the node in the ticket and return its bytes as
/// a raw IPC response, without reading or writing the local store.
///
/// The provider first proves that the size is at most `MAX_BLOB_SIZE`; every received
/// chunk is verified against the blob hash.
#[tauri::command]
pub async fn refetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> CmdResult<Response> {
    let ticket = parse_ticket(&ticket)?;
    let hash = ticket.hash();
    log::info!("[blob] downloading {hash} from remote without using the local store...");
    let (conn, size) =
        connect_and_check_size(&state.endpoint(), &ticket, MAX_BLOB_SIZE as u64).await?;
    let bytes = download_to_memory(conn, hash, size).await?;
    log::info!("[blob] downloaded {hash} from remote ({size} bytes)");
    Ok(Response::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_holds_only_recorded_paths_up_to_the_cap() {
        let dropped = DroppedPaths::default();
        assert!(!dropped.contains(Path::new("/tmp/a")));

        dropped.record(&[PathBuf::from("/tmp/a"), PathBuf::from("/tmp/a")]);
        assert!(dropped.contains(Path::new("/tmp/a")));
        assert!(!dropped.contains(Path::new("/tmp/a/../b")));
        assert_eq!(dropped.0.lock().unwrap().len(), 1);

        let more: Vec<PathBuf> = (0..MAX_DROPPED_PATHS)
            .map(|i| PathBuf::from(format!("/tmp/file{i}")))
            .collect();
        dropped.record(&more);
        assert_eq!(dropped.0.lock().unwrap().len(), MAX_DROPPED_PATHS);
        // The oldest entry was removed to stay within the cap.
        assert!(!dropped.contains(Path::new("/tmp/a")));
        assert!(dropped.contains(Path::new("/tmp/file0")));
    }

    #[test]
    fn ticket_must_be_short_and_name_a_raw_blob() {
        let addr = iroh::EndpointAddr::from(iroh::SecretKey::from_bytes(&[5u8; 32]).public());
        let hash = Hash::new(b"content");

        let raw = BlobTicket::new(addr.clone(), hash, BlobFormat::Raw).to_string();
        assert_eq!(parse_ticket(&raw).unwrap().hash(), hash);

        let hash_seq = BlobTicket::new(addr, hash, BlobFormat::HashSeq).to_string();
        assert!(parse_ticket(&hash_seq).is_err());

        assert!(parse_ticket("not a ticket").is_err());
        assert!(parse_ticket(&"a".repeat(MAX_BLOB_TICKET_LEN + 1)).is_err());
    }

    /// Downloads over QUIC between two endpoints of this process, bound to ephemeral
    /// local ports with relays disabled.
    #[tokio::test]
    async fn blob_is_downloaded_only_if_the_proven_size_is_within_the_limit() {
        use iroh::endpoint::presets;
        use iroh::protocol::Router;
        use iroh_blobs::store::mem::MemStore;
        use std::net::{Ipv4Addr, SocketAddr};

        let provider_store = MemStore::new();
        let provider = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let router = Router::builder(provider.clone())
            .accept(
                iroh_blobs::ALPN,
                iroh_blobs::BlobsProtocol::new(&provider_store, None),
            )
            .spawn();
        // 300,000 bytes: several 16 KiB chunk groups.
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let size = data.len() as u64;
        let hash = provider_store.add_bytes(data.clone()).await.unwrap().hash;
        let port = provider
            .bound_sockets()
            .into_iter()
            .find(SocketAddr::is_ipv4)
            .unwrap()
            .port();
        let addr = iroh::EndpointAddr::new(provider.id())
            .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        let ticket = BlobTicket::new(addr, hash, BlobFormat::Raw);

        let dir = std::env::temp_dir().join(format!(
            "proscenium-blob-test-{}",
            crate::util::generate_id()
        ));
        let store = FsStore::load(&dir).await.unwrap();
        let fetcher = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        assert!(read_local_blob(&store, hash).await.unwrap().is_none());

        // A limit one byte below the blob size: rejected, and nothing is stored.
        assert!(
            connect_and_check_size(&fetcher, &ticket, size - 1)
                .await
                .is_err()
        );
        assert!(read_local_blob(&store, hash).await.unwrap().is_none());

        let (conn, proven_size) = connect_and_check_size(&fetcher, &ticket, size)
            .await
            .unwrap();
        assert_eq!(proven_size, size);
        // A transfer that reports more payload than the given size is stopped.
        assert!(
            download_to_store(store.remote(), conn.clone(), hash, size - 1)
                .await
                .is_err()
        );
        download_to_store(store.remote(), conn.clone(), hash, size)
            .await
            .unwrap();
        assert_eq!(read_local_blob(&store, hash).await.unwrap().unwrap(), data);

        assert_eq!(download_to_memory(conn, hash, size).await.unwrap(), data);

        fetcher.close().await;
        router.shutdown().await.unwrap();
        store.shutdown().await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn blob_size_limit_is_inclusive() {
        assert!(check_blob_size(MAX_BLOB_SIZE as u64).is_ok());
        assert!(check_blob_size(MAX_BLOB_SIZE as u64 + 1).is_err());
    }
}
