use crate::ext::ResultExt;
use crate::state::AppState;
use crate::storage::Storage;
use iroh_social_types::{
    ConversationMeta, DirectMessage, DmPayload, MediaAttachment, StoredMessage, now_millis,
    short_id,
};
use std::sync::Arc;
use tauri::State;

use crate::constants::DEFAULT_DM_LIMIT;

#[tauri::command]
pub async fn send_dm(
    state: State<'_, Arc<AppState>>,
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

    let my_id = state.master_pubkey.clone();
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

    state
        .storage
        .upsert_conversation(&to, &my_id, timestamp, &preview)
        .map_err(|e| {
            log::error!("[dm-cmd] upsert_conversation error: {e}");
            e.to_string()
        })?;
    state.storage.insert_dm_message(&stored).map_err(|e| {
        log::error!("[dm-cmd] insert_dm_message error: {e}");
        e.to_string()
    })?;

    log::info!("[dm-cmd] stored message {} locally", short_id(&msg_id));

    let endpoint = state.endpoint.clone();
    let dm_handler = state.dm.clone();
    let to_clone = to.clone();
    tokio::spawn(async move {
        log::info!("[dm-cmd] async send starting to {}", short_id(&to_clone));
        match dm_handler.send_dm(&endpoint, &to_clone, dm_msg).await {
            Ok(()) => log::info!("[dm-cmd] async send completed to {}", short_id(&to_clone)),
            Err(e) => log::error!("[dm-cmd] async send failed to {}: {e}", short_id(&to_clone)),
        }
    });

    Ok(stored)
}

#[tauri::command]
pub async fn get_conversations(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<ConversationMeta>, String> {
    let convos = state.storage.get_conversations().str_err()?;
    log::info!("[dm-cmd] get_conversations: {} conversations", convos.len());
    Ok(convos)
}

#[tauri::command]
pub async fn get_dm_messages(
    state: State<'_, Arc<AppState>>,
    peer_pubkey: String,
    limit: Option<usize>,
    before: Option<u64>,
) -> Result<Vec<StoredMessage>, String> {
    let my_id = state.master_pubkey.clone();
    let conv_id = Storage::conversation_id(&my_id, &peer_pubkey);
    let msgs = state
        .storage
        .get_dm_messages(&conv_id, limit.unwrap_or(DEFAULT_DM_LIMIT), before)
        .str_err()?;
    log::info!(
        "[dm-cmd] get_dm_messages: peer={}, conv={}, {} messages",
        short_id(&peer_pubkey),
        short_id(&conv_id),
        msgs.len()
    );
    Ok(msgs)
}

#[tauri::command]
pub async fn mark_dm_read(
    state: State<'_, Arc<AppState>>,
    peer_pubkey: String,
) -> Result<(), String> {
    let my_id = state.master_pubkey.clone();
    state
        .storage
        .mark_conversation_read(&peer_pubkey, &my_id)
        .str_err()
}

#[tauri::command]
pub async fn delete_dm_message(
    state: State<'_, Arc<AppState>>,
    message_id: String,
) -> Result<(), String> {
    state.storage.delete_dm_message(&message_id).str_err()?;
    Ok(())
}

#[tauri::command]
pub async fn flush_dm_outbox(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let peers = state.storage.get_all_outbox_peers().str_err()?;
    let endpoint = state.endpoint.clone();
    let dm_handler = state.dm.clone();

    let mut total_sent = 0u32;
    let mut total_failed = 0u32;
    for peer in peers {
        match dm_handler.flush_outbox_for_peer(&endpoint, &peer).await {
            Ok((sent, failed)) => {
                total_sent += sent;
                total_failed += failed;
            }
            Err(e) => {
                log::error!("[dm-outbox] flush error for {}: {e}", short_id(&peer));
                total_failed += 1;
            }
        }
    }

    Ok(serde_json::json!({
        "sent": total_sent,
        "failed": total_failed,
    }))
}

#[tauri::command]
pub async fn get_unread_dm_count(state: State<'_, Arc<AppState>>) -> Result<u32, String> {
    state.storage.get_total_unread_count().str_err()
}

#[tauri::command]
pub async fn send_dm_signal(
    state: State<'_, Arc<AppState>>,
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

    let dm_handler = state.dm.clone();
    let endpoint = state.endpoint.clone();

    tokio::spawn(async move {
        if let Err(e) = dm_handler.send_signal(&endpoint, &to, payload).await {
            log::info!(
                "[dm-signal] failed to send {signal_type} to {}: {e}",
                short_id(&to)
            );
        }
    });

    Ok(())
}
