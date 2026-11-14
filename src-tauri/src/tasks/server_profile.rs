use crate::constants::{
    SERVER_PROFILE_REFRESH, SERVER_PROFILE_SYNC_INITIAL_DELAY, SERVER_PROFILE_SYNC_INTERVAL,
};
use crate::state::AppState;
use std::sync::Arc;

/// Send the own profile again to every registered server that has not accepted its
/// current version, for example after a failed send during a visibility change, or that
/// accepted none within `SERVER_PROFILE_REFRESH`, so the server keeps the registration.
pub fn spawn(state: Arc<AppState>) {
    let token = state.shutdown().child_token();
    tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = run(state) => {}
        }
        log::info!("[server] profile sync task stopped");
    });
}

async fn run(state: Arc<AppState>) {
    tokio::time::sleep(SERVER_PROFILE_SYNC_INITIAL_DELAY).await;
    loop {
        sync_unsynced_servers(&state).await;
        tokio::time::sleep(SERVER_PROFILE_SYNC_INTERVAL).await;
    }
}

async fn sync_unsynced_servers(state: &AppState) {
    let my_id = state.identity.read().await.master_pubkey.clone();
    let updated_at = match state.storage.get_profile(&my_id).await {
        Ok(Some(profile)) => profile.updated_at,
        Ok(None) => return,
        Err(e) => {
            log::error!("[server] failed to read the own profile: {e}");
            return;
        }
    };
    let refresh_ms = u64::try_from(SERVER_PROFILE_REFRESH.as_millis()).unwrap_or(u64::MAX);
    let sent_before = proscenium_types::now_millis().saturating_sub(refresh_ms);
    let urls = match state
        .storage
        .get_servers_to_send_profile(updated_at, sent_before)
        .await
    {
        Ok(urls) => urls,
        Err(e) => {
            log::error!("[server] failed to read the server list: {e}");
            return;
        }
    };
    for url in urls {
        if let Err(e) = crate::commands::servers::sync_profile_to_server_inner(
            &state.http_client,
            &state.storage,
            &state.identity,
            &url,
        )
        .await
        {
            log::warn!("[server] profile sync to {url} failed: {e}");
        }
    }
}
