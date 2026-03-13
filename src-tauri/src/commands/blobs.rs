use crate::state::AppState;
use iroh_blobs::{HashAndFormat, ticket::BlobTicket};
use iroh_social_types::MAX_BLOB_SIZE;
use std::sync::Arc;
use tauri::State;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn add_blob(
    state: State<'_, Arc<AppState>>,
    content: String,
) -> Result<serde_json::Value, String> {
    state.add_blob(content).await
}

#[tauri::command]
pub async fn fetch_blob(state: State<'_, Arc<AppState>>, ticket: String) -> Result<String, String> {
    state.fetch_blob(ticket).await
}

#[tauri::command]
pub async fn add_blob_bytes(
    state: State<'_, Arc<AppState>>,
    data: Vec<u8>,
) -> Result<serde_json::Value, String> {
    state.add_blob_bytes(data).await
}

#[tauri::command]
pub async fn fetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> Result<Vec<u8>, String> {
    state.fetch_blob_bytes(ticket).await
}

#[tauri::command]
pub async fn refetch_blob_bytes(
    state: State<'_, Arc<AppState>>,
    ticket: String,
) -> Result<Vec<u8>, String> {
    state.refetch_blob_bytes(ticket).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    async fn add_blob_data(&self, data: &[u8]) -> Result<serde_json::Value, String> {
        if data.len() > MAX_BLOB_SIZE {
            return Err(format!(
                "blob too large: {} bytes (max {} bytes)",
                data.len(),
                MAX_BLOB_SIZE
            ));
        }
        let size = data.len();
        let tag = self
            .blob_store
            .add_slice(data)
            .await
            .map_err(|e| e.to_string())?;
        let addr = self.endpoint.addr();
        let ticket = BlobTicket::new(addr, tag.hash, tag.format);
        log::info!("[blob] added blob {} ({size} bytes)", tag.hash);
        Ok(serde_json::json!({
            "hash": tag.hash.to_string(),
            "ticket": ticket.to_string(),
        }))
    }

    async fn fetch_blob_data(&self, ticket: &BlobTicket) -> Result<Vec<u8>, String> {
        let conn = self
            .endpoint
            .connect(ticket.addr().clone(), iroh_blobs::ALPN)
            .await
            .map_err(|e| e.to_string())?;
        let hash_and_format: HashAndFormat = ticket.hash_and_format();
        self.blobs
            .remote()
            .fetch(conn, hash_and_format)
            .await
            .map_err(|e| e.to_string())?;
        let bytes = self
            .blob_store
            .get_bytes(ticket.hash())
            .await
            .map_err(|e| e.to_string())?;
        Ok(bytes.to_vec())
    }

    pub(crate) async fn add_blob(&self, content: String) -> Result<serde_json::Value, String> {
        self.add_blob_data(content.as_bytes()).await
    }

    pub(crate) async fn fetch_blob(&self, ticket: String) -> Result<String, String> {
        let ticket: BlobTicket = ticket.parse().map_err(|e| format!("{e:?}"))?;
        log::info!("[blob] fetching text blob {}...", ticket.hash());
        let bytes = self.fetch_blob_data(&ticket).await?;
        log::info!(
            "[blob] fetched text blob {} ({} bytes)",
            ticket.hash(),
            bytes.len()
        );
        String::from_utf8(bytes).map_err(|e| e.to_string())
    }

    pub(crate) async fn add_blob_bytes(&self, data: Vec<u8>) -> Result<serde_json::Value, String> {
        self.add_blob_data(&data).await
    }

    pub(crate) async fn fetch_blob_bytes(&self, ticket: String) -> Result<Vec<u8>, String> {
        let ticket: BlobTicket = ticket.parse().map_err(|e| format!("{e:?}"))?;
        if let Ok(bytes) = self.blob_store.get_bytes(ticket.hash()).await {
            return Ok(bytes.to_vec());
        }
        log::info!("[blob] fetching {} from remote...", ticket.hash());
        let bytes = self.fetch_blob_data(&ticket).await?;
        log::info!(
            "[blob] fetched {} from remote ({} bytes)",
            ticket.hash(),
            bytes.len()
        );
        Ok(bytes)
    }

    pub(crate) async fn refetch_blob_bytes(&self, ticket: String) -> Result<Vec<u8>, String> {
        let ticket: BlobTicket = ticket.parse().map_err(|e| format!("{e:?}"))?;
        log::info!(
            "[blob] re-fetching {} from remote (ignoring local cache)...",
            ticket.hash()
        );
        let bytes = self.fetch_blob_data(&ticket).await?;
        log::info!(
            "[blob] re-fetched {} from remote ({} bytes)",
            ticket.hash(),
            bytes.len()
        );
        Ok(bytes)
    }
}
