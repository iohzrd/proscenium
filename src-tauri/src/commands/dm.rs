use crate::state::AppState;
use crate::storage::Storage;
use iroh_social_types::{
    ConversationMeta, DirectMessage, DmPayload, MediaAttachment, StoredMessage, now_millis,
    short_id,
};
use std::sync::Arc;
use tauri::State;

use crate::constants::DEFAULT_DM_LIMIT;

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn send_dm(
    state: State<'_, Arc<AppState>>,
    to: String,
    content: String,
    media: Option<Vec<MediaAttachment>>,
    reply_to: Option<String>,
) -> Result<StoredMessage, String> {
    state.send_dm(to, content, media, reply_to).await
}

#[tauri::command]
pub async fn get_conversations(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ConversationMeta>, String> {
    state.get_conversations().await
}

#[tauri::command]
pub async fn get_dm_messages(
    state: State<'_, Arc<AppState>>,
    peer_pubkey: String,
    limit: Option<usize>,
    before: Option<u64>,
) -> Result<Vec<StoredMessage>, String> {
    state.get_dm_messages(peer_pubkey, limit, before).await
}

#[tauri::command]
pub async fn mark_dm_read(
    state: State<'_, Arc<AppState>>,
    peer_pubkey: String,
) -> Result<(), String> {
    state.mark_dm_read(peer_pubkey).await
}

#[tauri::command]
pub async fn delete_dm_message(
    state: State<'_, Arc<AppState>>,
    message_id: String,
) -> Result<(), String> {
    state.delete_dm_message(message_id).await
}

#[tauri::command]
pub async fn get_unread_dm_count(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.get_unread_dm_count().await
}

#[tauri::command]
pub async fn send_dm_signal(
    state: State<'_, Arc<AppState>>,
    to: String,
    signal_type: String,
    message_id: Option<String>,
) -> Result<(), String> {
    state.send_dm_signal(to, signal_type, message_id).await
}

// ── AppState impl ─────────────────────────────────────────────────────────────

impl AppState {
    pub(crate) async fn send_dm(
        &self,
        to: String,
        content: String,
        media: Option<Vec<MediaAttachment>>,
        reply_to: Option<String>,
    ) -> Result<StoredMessage, String> {
        log::info!(
            "[dm-cmd] send_dm called: to={}, content_len={}, media={:?}, reply_to={:?}",
            short_id(&to),
            content.len(),
            media.as_ref().map(|m| m.len()),
            reply_to
        );

        let my_id = self.identity.read().await.master_pubkey.clone();
        let msg_id = uuid::Uuid::new_v4().to_string();
        let timestamp = now_millis();

        let dm_msg = DirectMessage {
            id: msg_id.clone(),
            content: content.clone(),
            timestamp,
            media: media.clone().unwrap_or_default(),
            reply_to: reply_to.clone(),
        };

        let conv_id = Storage::conversation_id(&my_id, &to);
        let preview = if content.len() > 80 {
            format!("{}...", &content[..77])
        } else {
            content.clone()
        };

        let stored = StoredMessage {
            id: msg_id.clone(),
            conversation_id: conv_id,
            from_pubkey: my_id.clone(),
            to_pubkey: to.clone(),
            content,
            timestamp,
            media: media.unwrap_or_default(),
            read: false,
            delivered: false,
            reply_to,
        };

        self.storage
            .upsert_conversation(&to, &my_id, timestamp, &preview)
            .await
            .map_err(|e| {
                log::error!("[dm-cmd] upsert_conversation error: {e}");
                e.to_string()
            })?;
        self.storage.insert_dm_message(&stored).await.map_err(|e| {
            log::error!("[dm-cmd] insert_dm_message error: {e}");
            e.to_string()
        })?;

        log::info!("[dm-cmd] stored message {} locally", short_id(&msg_id));
        log::info!("[dm-cmd] sending to {}", short_id(&to));
        match self.dm.send_dm(&to, dm_msg).await {
            Ok(()) => log::info!("[dm-cmd] send completed to {}", short_id(&to)),
            Err(e) => log::error!("[dm-cmd] send failed to {}: {e}", short_id(&to)),
        }

        Ok(stored)
    }

    pub(crate) async fn get_conversations(&self) -> Result<Vec<ConversationMeta>, String> {
        let convos = self
            .storage
            .get_conversations()
            .await
            .map_err(|e| e.to_string())?;
        log::info!("[dm-cmd] get_conversations: {} conversations", convos.len());
        Ok(convos)
    }

    pub(crate) async fn get_dm_messages(
        &self,
        peer_pubkey: String,
        limit: Option<usize>,
        before: Option<u64>,
    ) -> Result<Vec<StoredMessage>, String> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        let conv_id = Storage::conversation_id(&my_id, &peer_pubkey);
        let msgs = self
            .storage
            .get_dm_messages(&conv_id, limit.unwrap_or(DEFAULT_DM_LIMIT), before)
            .await
            .map_err(|e| e.to_string())?;
        log::info!(
            "[dm-cmd] get_dm_messages: peer={}, conv={}, {} messages",
            short_id(&peer_pubkey),
            short_id(&conv_id),
            msgs.len()
        );
        Ok(msgs)
    }

    pub(crate) async fn mark_dm_read(&self, peer_pubkey: String) -> Result<(), String> {
        let my_id = self.identity.read().await.master_pubkey.clone();
        self.storage
            .mark_conversation_read(&peer_pubkey, &my_id)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn delete_dm_message(&self, message_id: String) -> Result<(), String> {
        self.storage
            .delete_dm_message(&message_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(crate) async fn get_unread_dm_count(&self) -> Result<u32, String> {
        self.storage
            .get_total_unread_count()
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn send_dm_signal(
        &self,
        to: String,
        signal_type: String,
        message_id: Option<String>,
    ) -> Result<(), String> {
        let payload = match signal_type.as_str() {
            "typing" => DmPayload::Typing,
            "read" => {
                let id = message_id.ok_or("message_id required for read signal")?;
                DmPayload::Read { message_id: id }
            }
            other => return Err(format!("unknown signal type: {other}")),
        };
        if let Err(e) = self.dm.send_signal(&to, payload).await {
            log::info!(
                "[dm-signal] failed to send {signal_type} to {}: {e}",
                short_id(&to)
            );
        }
        Ok(())
    }
}
