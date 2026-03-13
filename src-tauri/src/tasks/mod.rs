mod device_sync;
mod dm_outbox;
mod housekeeping;
mod network;
mod push_outbox;
mod sync;

use crate::dm::DmHandler;
use crate::gossip::{GossipService, ReconnectRequest};
use crate::state::{Identity, TaskManager};
use crate::storage::Storage;
use iroh::Endpoint;
use iroh_social_types::FollowEntry;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

/// Spawn all background tasks into the given TaskManager.
#[allow(clippy::too_many_arguments)]
pub fn spawn_all(
    tm: &mut TaskManager,
    endpoint: Endpoint,
    gossip: GossipService,
    dm_handler: DmHandler,
    storage: Arc<Storage>,
    identity: Arc<Identity>,
    app_handle: AppHandle,
    follows: Vec<FollowEntry>,
    reconnect_rx: mpsc::UnboundedReceiver<ReconnectRequest>,
    sync_rx: mpsc::Receiver<crate::state::SyncCommand>,
    dm_outbox_notify: Arc<Notify>,
    shutdown: CancellationToken,
) {
    #[cfg(target_os = "android")]
    {
        use crate::constants::ANDROID_NET_INTERVAL;
        let ep = endpoint.clone();
        let token = shutdown.child_token();
        tm.spawn("android-network", async move {
            ep.network_change().await;
            log::info!("[android-net] initial network_change() sent");
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep(ANDROID_NET_INTERVAL) => {
                        ep.network_change().await;
                    }
                }
            }
        });
    }

    tm.spawn(
        "gossip-reconnect",
        gossip.reconnect_loop(reconnect_rx, shutdown.child_token()),
    );

    tm.spawn(
        "subscribe-and-sync",
        sync::subscribe_and_sync_task(
            gossip.clone(),
            endpoint.clone(),
            storage.clone(),
            app_handle,
            identity.master_pubkey.clone(),
            follows,
            sync_rx,
            shutdown.child_token(),
        ),
    );

    tm.spawn(
        "dm-outbox-flush",
        dm_outbox::dm_outbox_flush_task(
            dm_handler,
            endpoint.clone(),
            storage.clone(),
            dm_outbox_notify,
            shutdown.child_token(),
        ),
    );

    tm.spawn(
        "push-outbox-flush",
        push_outbox::push_outbox_flush_task(
            endpoint.clone(),
            storage.clone(),
            identity.master_pubkey.clone(),
            shutdown.child_token(),
        ),
    );

    tm.spawn(
        "housekeeping",
        housekeeping::housekeeping_task(storage.clone(), shutdown.child_token()),
    );

    tm.spawn(
        "device-sync",
        device_sync::device_sync_task(
            endpoint.clone(),
            storage.clone(),
            identity.master_pubkey.clone(),
            identity.signing_secret_key_bytes,
            shutdown.child_token(),
        ),
    );

    tm.spawn(
        "network-health",
        network::network_health_task(endpoint, gossip, shutdown.child_token()),
    );
}
