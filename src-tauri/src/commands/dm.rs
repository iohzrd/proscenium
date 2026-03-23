use crate::constants::DEFAULT_DM_LIMIT;
use crate::error::CmdResult;
use crate::state::AppState;
use crate::storage::Storage;
use proscenium_types::{
    ConversationMeta, DirectMessage, DmPayload, MediaAttachment, StoredMessage, now_millis,
    short_id,
};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn send_dm(
    state: State<'_, Arc<AppState>>,
    to: String,
    content: String,
    media: Option<Vec<MediaAttachment>>,
    reply_to: Option<String>,
) -> CmdResult<StoredMessage> {
    log::info!(
        "[dm-cmd] send_dm called: to={}, content_len={}, media={:?}, reply_to={:?}",
        short_id(&to),
        content.len(),
        media.as_ref().map(|m| m.len()),
        reply_to
    );

    let my_id = state.identity.read().await.master_pubkey.clone();
    let msg_id = crate::util::generate_id();
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
        .await
        .map_err(|e| {
            log::error!("[dm-cmd] upsert_conversation error: {e}");
            format!("{e}")
        })?;
    state
        .storage
        .insert_dm_message(&stored)
        .await
        .map_err(|e| {
            log::error!("[dm-cmd] insert_dm_message error: {e}");
            format!("{e}")
        })?;

    log::info!("[dm-cmd] stored message {} locally", short_id(&msg_id));
    log::info!("[dm-cmd] sending to {}", short_id(&to));
    match state.dm().send_dm(&to, dm_msg).await {
        Ok(()) => log::info!("[dm-cmd] send completed to {}", short_id(&to)),
        Err(e) => log::error!("[dm-cmd] send failed to {}: {e}", short_id(&to)),
    }

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

#[tauri::command]
pub async fn get_dm_messages(
    state: State<'_, Arc<AppState>>,
    peer_pubkey: String,
    limit: Option<usize>,
    before: Option<u64>,
) -> CmdResult<Vec<StoredMessage>> {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let conv_id = Storage::conversation_id(&my_id, &peer_pubkey);
    let msgs = state
        .storage
        .get_dm_messages(&conv_id, limit.unwrap_or(DEFAULT_DM_LIMIT), before)
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
    let payload = match signal_type.as_str() {
        "typing" => DmPayload::Typing,
        "read" => {
            let id = message_id.ok_or("message_id required for read signal")?;
            DmPayload::Read { message_id: id }
        }
        other => return Err(format!("unknown signal type: {other}").into()),
    };
    if let Err(e) = state.dm().send_signal(&to, payload).await {
        log::info!(
            "[dm-signal] failed to send {signal_type} to {}: {e}",
            short_id(&to)
        );
    }
    Ok(())
}
