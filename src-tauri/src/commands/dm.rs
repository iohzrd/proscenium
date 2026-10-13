use crate::constants::DEFAULT_DM_LIMIT;
use crate::error::{AppError, CmdResult};
use crate::state::AppState;
use crate::storage::Storage;
use proscenium_types::{
    ConversationMeta, DmPayload, MediaAttachment, StoredMessage, is_valid_id, is_valid_pubkey,
    now_millis, short_id,
};
use std::sync::Arc;
use tauri::State;

/// Maximum number of messages `get_dm_messages` returns per call.
const MAX_DM_PAGE_LIMIT: usize = 200;

/// Store a message to `to` and attempt delivery.
///
/// Returns the stored message; `delivered` is true if the peer acknowledged it before
/// this command returned. A message that was not acknowledged stays in the outbox and is
/// sent again by the flush loop. Returns an error, and stores nothing, if `to` is not a
/// canonical pubkey of another user, or if the content, attachments or `reply_to` are
/// invalid or exceed the size limits.
#[tauri::command]
pub async fn send_dm(
    state: State<'_, Arc<AppState>>,
    to: String,
    content: String,
    media: Option<Vec<MediaAttachment>>,
    reply_to: Option<String>,
) -> CmdResult<StoredMessage> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    if !is_valid_pubkey(&to) {
        return Err(AppError::Other("invalid recipient pubkey".into()));
    }
    if to == my_id {
        return Err(AppError::Other("cannot send a message to yourself".into()));
    }
    let media = media.unwrap_or_default();
    if content.is_empty() && media.is_empty() {
        return Err(AppError::Other("message is empty".into()));
    }

    let mut stored = StoredMessage {
        id: crate::util::generate_id(),
        conversation_id: Storage::conversation_id(&my_id, &to),
        from_pubkey: my_id,
        to_pubkey: to,
        content,
        timestamp: now_millis(),
        media,
        read: false,
        delivered: false,
        reply_to,
    };

    // Validates the content, attachments and `reply_to` before any row is written.
    stored.delivered = state.dm().send_dm(&stored).await?;
    log::info!(
        "[dm-cmd] message {} to {}: delivered={}",
        short_id(&stored.id),
        short_id(&stored.to_pubkey),
        stored.delivered
    );
    Ok(stored)
}

#[tauri::command]
pub async fn get_conversations(
    state: State<'_, Arc<AppState>>,
) -> CmdResult<Vec<ConversationMeta>> {
    let convos = state.storage.get_conversations().await?;
    log::info!("[dm-cmd] get_conversations: {} conversations", convos.len());
    Ok(convos)
}

/// Return up to `limit` (at most `MAX_DM_PAGE_LIMIT`) messages of the conversation with
/// `peer_pubkey`, ascending by `(timestamp, id)`. `before` and `before_id` are the
/// timestamp and id of the oldest message of the previous page; only older messages are
/// returned.
#[tauri::command]
pub async fn get_dm_messages(
    state: State<'_, Arc<AppState>>,
    peer_pubkey: String,
    limit: Option<usize>,
    before: Option<u64>,
    before_id: Option<String>,
) -> CmdResult<Vec<StoredMessage>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let conv_id = Storage::conversation_id(&my_id, &peer_pubkey);
    let limit = limit.unwrap_or(DEFAULT_DM_LIMIT).min(MAX_DM_PAGE_LIMIT);
    let msgs = state
        .storage
        .get_dm_messages(&conv_id, limit, before, before_id.as_deref())
        .await?;
    log::info!(
        "[dm-cmd] get_dm_messages: peer={}, conv={}, {} messages",
        short_id(&peer_pubkey),
        short_id(&conv_id),
        msgs.len()
    );
    Ok(msgs)
}

#[tauri::command]
pub async fn mark_dm_read(state: State<'_, Arc<AppState>>, peer_pubkey: String) -> CmdResult<()> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    state
        .storage
        .mark_conversation_read(&peer_pubkey, &my_id)
        .await?;
    Ok(())
}

/// Delete a message and, if it is still queued for delivery, its outbox entry.
#[tauri::command]
pub async fn delete_dm_message(
    state: State<'_, Arc<AppState>>,
    message_id: String,
) -> CmdResult<()> {
    state.storage.delete_dm_message(&message_id).await?;
    Ok(())
}

#[tauri::command]
pub async fn get_unread_dm_count(state: State<'_, Arc<AppState>>) -> CmdResult<u32> {
    let count = state.storage.get_total_unread_count().await?;
    Ok(count)
}

#[tauri::command]
pub async fn send_dm_signal(
    state: State<'_, Arc<AppState>>,
    to: String,
    signal_type: String,
    message_id: Option<String>,
) -> CmdResult<()> {
    if !is_valid_pubkey(&to) {
        return Err(AppError::Other("invalid recipient pubkey".into()));
    }
    let payload = match signal_type.as_str() {
        "typing" => DmPayload::Typing,
        "read" => {
            let id = message_id.ok_or("message_id required for read signal")?;
            if !is_valid_id(&id) {
                return Err(AppError::Other("invalid message_id".into()));
            }
            DmPayload::Read { message_id: id }
        }
        other => return Err(format!("unknown signal type: {other}").into()),
    };
    if let Err(e) = state.dm().send_signal(&to, payload).await {
        log::debug!(
            "[dm-signal] failed to send {signal_type} to {}: {e}",
            short_id(&to)
        );
    }
    Ok(())
}

/// Wake the outbox flush loop. The pass it starts also attempts messages whose retry
/// delay has not elapsed. Returns before the pass runs; `dm-delivered` is emitted for
/// each message a peer acknowledges.
#[tauri::command]
pub async fn flush_dm_outbox(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.dm().request_outbox_flush();
    Ok(())
}
