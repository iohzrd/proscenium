//! Upload of a file from the webview into the blob store in chunks.
//!
//! One IPC call with a whole file holds several copies of it: on Android every call is a
//! JSON message, in which a byte array becomes an array of numbers. The webview sends a
//! file in chunks of at most `MAX_UPLOAD_CHUNK` bytes, as a raw body on desktop and as
//! base64 on Android, and the import into the blob store takes each chunk as it arrives.

use super::blobs::{check_blob_size, ticket_addr};
use crate::error::{AppError, CmdResult};
use crate::state::AppState;
use base64::Engine;
use bytes::Bytes;
use iroh_blobs::{BlobFormat, Hash, store::fs::FsStore, ticket::BlobTicket};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::State;
use tauri::ipc::{InvokeBody, Request};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Largest chunk one `append_blob_upload` call carries.
const MAX_UPLOAD_CHUNK: usize = 1024 * 1024;
/// Uploads stored at once.
const MAX_UPLOADS: usize = 4;
/// An upload without a call for this long is removed when another upload begins.
const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Chunks held between the IPC calls and the import; a call waits while it is full.
const UPLOAD_CHANNEL_CHUNKS: usize = 2;
/// Header of `append_blob_upload` that names the upload.
const UPLOAD_ID_HEADER: &str = "upload-id";

type ImportResult = Result<(Hash, BlobFormat), String>;

struct Upload {
    chunks: mpsc::Sender<std::io::Result<Bytes>>,
    /// The import; `None` once `finish_blob_upload` awaits it.
    import: Option<JoinHandle<ImportResult>>,
    size: u64,
    received: u64,
    last_call: Instant,
}

impl Drop for Upload {
    /// Aborts the import unless `finish_blob_upload` awaits it: an import whose chunk
    /// stream ends without an error stores the bytes received so far as a blob.
    fn drop(&mut self) {
        if let Some(import) = self.import.take() {
            import.abort();
        }
    }
}

/// Uploads in progress by id. Managed as Tauri state. An upload is taken out of the map
/// while a call handles it, so it handles one call at a time.
#[derive(Default)]
pub struct Uploads(Mutex<HashMap<String, Upload>>);

impl Uploads {
    fn take(&self, id: &str) -> CmdResult<Upload> {
        let mut uploads = self.0.lock().map_err(|_| "upload table lock poisoned")?;
        uploads
            .remove(id)
            .ok_or_else(|| "no upload with this id is waiting for a call".into())
    }

    fn put(&self, id: String, upload: Upload) {
        if let Ok(mut uploads) = self.0.lock() {
            uploads.insert(id, upload);
        }
    }
}

/// Start an import into `store` of the chunks sent on the returned sender. The import
/// stores a blob once the sender is dropped, and nothing if its task is aborted before.
fn start_import(
    store: FsStore,
) -> (
    mpsc::Sender<std::io::Result<Bytes>>,
    JoinHandle<ImportResult>,
) {
    let (chunks, receiver) = mpsc::channel(UPLOAD_CHANNEL_CHUNKS);
    let stream = futures_lite::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    let import = tokio::spawn(async move {
        let tag = store
            .add_stream(stream)
            .await
            .await
            .map_err(|e| e.to_string())?;
        Ok((tag.hash, tag.format))
    });
    (chunks, import)
}

/// Start an upload of `size` bytes and return its id. Fails if `size` exceeds
/// `MAX_BLOB_SIZE` or `MAX_UPLOADS` uploads are stored; uploads idle for
/// `UPLOAD_IDLE_TIMEOUT` are removed first.
#[tauri::command]
pub async fn begin_blob_upload(
    state: State<'_, Arc<AppState>>,
    uploads: State<'_, Uploads>,
    size: u64,
) -> CmdResult<String> {
    check_blob_size(size)?;
    let store = state.blob_store();
    let id = crate::util::generate_id();
    let mut table = uploads.0.lock().map_err(|_| "upload table lock poisoned")?;
    let now = Instant::now();
    table.retain(|_, upload| now.duration_since(upload.last_call) < UPLOAD_IDLE_TIMEOUT);
    if table.len() >= MAX_UPLOADS {
        return Err(format!("{MAX_UPLOADS} uploads are in progress; try again later").into());
    }
    let (chunks, import) = start_import(store);
    table.insert(
        id.clone(),
        Upload {
            chunks,
            import: Some(import),
            size,
            received: 0,
            last_call: now,
        },
    );
    Ok(id)
}

/// The bytes of one chunk: a raw body (desktop), or `{ "chunk": <base64> }` (Android).
fn chunk_bytes(body: &InvokeBody) -> CmdResult<Bytes> {
    match body {
        InvokeBody::Raw(bytes) => Ok(Bytes::copy_from_slice(bytes)),
        InvokeBody::Json(value) => {
            let encoded = value
                .get("chunk")
                .and_then(serde_json::Value::as_str)
                .ok_or("chunk must be raw bytes or a base64 string in `chunk`")?;
            if encoded.len() > MAX_UPLOAD_CHUNK.div_ceil(3) * 4 {
                return Err("chunk too large".into());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| AppError::Other(format!("invalid base64 chunk: {e}")))?;
            Ok(Bytes::from(bytes))
        }
    }
}

/// Append the chunk in the body to the upload named by the `upload-id` header. A chunk
/// over `MAX_UPLOAD_CHUNK` bytes or past the declared size ends the upload with an error.
#[tauri::command]
pub async fn append_blob_upload(
    uploads: State<'_, Uploads>,
    request: Request<'_>,
) -> CmdResult<()> {
    let id = request
        .headers()
        .get(UPLOAD_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or("missing upload-id header")?
        .to_string();
    let chunk = chunk_bytes(request.body())?;
    let mut upload = uploads.take(&id)?;
    let len = chunk.len() as u64;
    if chunk.len() > MAX_UPLOAD_CHUNK || upload.received.saturating_add(len) > upload.size {
        return Err("chunk exceeds the chunk limit or the declared size".into());
    }
    if upload.chunks.send(Ok(chunk)).await.is_err() {
        return Err("the blob import stopped".into());
    }
    upload.received += len;
    upload.last_call = Instant::now();
    uploads.put(id, upload);
    Ok(())
}

/// Complete the upload once all declared bytes were appended, and return
/// `{ hash, ticket }` of the stored blob.
#[tauri::command]
pub async fn finish_blob_upload(
    state: State<'_, Arc<AppState>>,
    uploads: State<'_, Uploads>,
    upload_id: String,
) -> CmdResult<serde_json::Value> {
    let mut upload = uploads.take(&upload_id)?;
    if upload.received != upload.size {
        return Err(format!(
            "the upload holds {} of {} bytes",
            upload.received, upload.size
        )
        .into());
    }
    let size = upload.size;
    let import = upload.import.take().ok_or("the upload has no import")?;
    // Closes the chunk stream, which completes the import.
    drop(upload);
    let (hash, format) = import
        .await
        .map_err(|e| AppError::Other(format!("blob import task failed: {e}")))?
        .map_err(AppError::Other)?;
    let ticket = BlobTicket::new(ticket_addr(&state.endpoint()), hash, format);
    log::info!("[blob] added blob {hash} ({size} bytes) by upload");
    Ok(serde_json::json!({
        "hash": hash.to_string(),
        "ticket": ticket.to_string(),
    }))
}

/// End the upload without storing a blob. Succeeds if no such upload is stored.
#[tauri::command]
pub async fn abort_blob_upload(uploads: State<'_, Uploads>, upload_id: String) -> CmdResult<()> {
    // Dropping the upload aborts its import.
    drop(uploads.take(&upload_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_blobs::api::blobs::BlobStatus;

    #[test]
    fn chunks_are_read_from_raw_bodies_and_base64() {
        let raw = InvokeBody::Raw(vec![1, 2, 3]);
        assert_eq!(chunk_bytes(&raw).unwrap().as_ref(), &[1, 2, 3]);
        let encoded = InvokeBody::Json(serde_json::json!({ "chunk": "AQID" }));
        assert_eq!(chunk_bytes(&encoded).unwrap().as_ref(), &[1, 2, 3]);
        let numbers = InvokeBody::Json(serde_json::json!([1, 2, 3]));
        assert!(chunk_bytes(&numbers).is_err());
        let too_long =
            base64::engine::general_purpose::STANDARD.encode(vec![0; MAX_UPLOAD_CHUNK + 3]);
        assert!(chunk_bytes(&InvokeBody::Json(serde_json::json!({ "chunk": too_long }))).is_err());
    }

    /// Chunks become one blob once the sender is dropped; a dropped `Upload` aborts its
    /// import, which then stores nothing.
    #[tokio::test]
    async fn uploaded_chunks_form_one_blob_and_a_dropped_upload_stores_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "proscenium-upload-test-{}",
            crate::util::generate_id()
        ));
        let store = FsStore::load(&dir).await.unwrap();

        let data: Vec<u8> = (0..MAX_UPLOAD_CHUNK * 3 / 2)
            .map(|i| (i % 251) as u8)
            .collect();
        let (chunks, import) = start_import(store.clone());
        for chunk in data.chunks(MAX_UPLOAD_CHUNK) {
            chunks
                .send(Ok(Bytes::copy_from_slice(chunk)))
                .await
                .unwrap();
        }
        drop(chunks);
        let (hash, format) = import.await.unwrap().unwrap();
        assert_eq!(hash, Hash::new(&data));
        assert_eq!(format, BlobFormat::Raw);
        assert_eq!(store.get_bytes(hash).await.unwrap().as_ref(), &data[..]);

        let partial = b"the first chunk of an upload that is never finished".to_vec();
        let (chunks, import) = start_import(store.clone());
        chunks.send(Ok(Bytes::from(partial.clone()))).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(Upload {
            chunks,
            import: Some(import),
            size: 1_000,
            received: partial.len() as u64,
            last_call: Instant::now(),
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(matches!(
            store.status(Hash::new(&partial)).await.unwrap(),
            BlobStatus::NotFound
        ));

        store.shutdown().await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
