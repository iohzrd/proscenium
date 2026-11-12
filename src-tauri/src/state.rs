use crate::call::CallHandler;
use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::stage::StageHandler;
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

/// Commands sent to the peer sync task to trigger on-demand syncs.
pub enum SyncCommand {
    /// Sync a specific peer immediately (e.g. after following).
    SyncPeer(String),
    /// Sync all followed peers immediately (e.g. app foreground, manual refresh).
    SyncAll,
}

/// Cryptographic identity and key material for this node.
/// Wrapped in `SharedIdentity` (Arc<RwLock<Identity>>) so `setup::replace_identity` can
/// replace it in place.
pub struct Identity {
    /// Master key secret bytes. Every device of the user holds the master key.
    pub master_secret_key_bytes: [u8; 32],
    /// The master key. It is the user's identity and signs all of the user's content.
    pub master_key: SecretKey,
    /// Master public key string (the user ID).
    pub master_pubkey: String,
    /// Secret key of this device's iroh endpoint, generated at random for each device.
    /// Its X25519 form decrypts the DMs sent to this device.
    pub transport_key: SecretKey,
    /// Transport NodeId string (the public key of `transport_key`).
    pub transport_node_id: String,
}

impl Identity {
    pub fn new(master_secret_key_bytes: [u8; 32], transport_key: SecretKey) -> Self {
        let master_key = SecretKey::from_bytes(&master_secret_key_bytes);
        Self {
            master_secret_key_bytes,
            master_pubkey: master_key.public().to_string(),
            master_key,
            transport_node_id: transport_key.public().to_string(),
            transport_key,
        }
    }
}

/// Overwrites the master key bytes with zeros. `SecretKey` zeroizes its own bytes.
impl Drop for Identity {
    fn drop(&mut self) {
        self.master_secret_key_bytes.zeroize();
    }
}

/// Prints the public fields only.
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("master_pubkey", &self.master_pubkey)
            .field("transport_node_id", &self.transport_node_id)
            .finish_non_exhaustive()
    }
}

/// Shared, mutable identity -- the canonical source for all key material.
pub type SharedIdentity = Arc<tokio::sync::RwLock<Identity>>;

/// Networking and protocol state that gets rebuilt when the identity is replaced.
pub struct NetworkStack {
    /// The blob store. `router` closes it when it shuts down.
    pub(crate) blob_store: FsStore,
    pub(crate) endpoint: Endpoint,
    pub(crate) gossip: GossipService,
    pub(crate) dm: DmHandler,
    pub(crate) call: CallHandler,
    pub(crate) peer: PeerHandler,
    pub(crate) stage: StageHandler,
    pub(crate) blobs: BlobsProtocol,
    pub(crate) sync_tx: mpsc::Sender<SyncCommand>,
    pub(crate) shutdown: CancellationToken,
    pub(crate) router: Router,
}

pub struct AppState {
    pub(crate) app_handle: tauri::AppHandle,
    pub(crate) identity: SharedIdentity,
    pub(crate) storage: Arc<Storage>,
    pub(crate) http_client: reqwest::Client,
    pub(crate) net: std::sync::RwLock<NetworkStack>,
    /// Set when the exit path has started shutting the network down.
    pub(crate) exit_started: AtomicBool,
    /// Set while `setup::replace_identity` runs.
    pub(crate) replacing_identity: AtomicBool,
}

impl AppState {
    /// Read access to the current network stack.
    ///
    /// A poisoned lock is read anyway: the only writer, `replace_net`, assigns a whole
    /// `NetworkStack`, so the value is complete even if a thread panicked while holding
    /// the lock.
    fn net(&self) -> std::sync::RwLockReadGuard<'_, NetworkStack> {
        self.net
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn endpoint(&self) -> Endpoint {
        self.net().endpoint.clone()
    }

    pub fn gossip(&self) -> GossipService {
        self.net().gossip.clone()
    }

    pub fn dm(&self) -> DmHandler {
        self.net().dm.clone()
    }

    pub fn call(&self) -> CallHandler {
        self.net().call.clone()
    }

    pub fn peer(&self) -> PeerHandler {
        self.net().peer.clone()
    }

    pub fn stage(&self) -> StageHandler {
        self.net().stage.clone()
    }

    pub fn blobs(&self) -> BlobsProtocol {
        self.net().blobs.clone()
    }

    pub fn blob_store(&self) -> FsStore {
        self.net().blob_store.clone()
    }

    pub fn sync_tx(&self) -> mpsc::Sender<SyncCommand> {
        self.net().sync_tx.clone()
    }

    pub fn shutdown(&self) -> CancellationToken {
        self.net().shutdown.clone()
    }

    pub fn router(&self) -> Router {
        self.net().router.clone()
    }

    /// Store `net` as the current network stack. The previous stack is dropped after
    /// the lock is released.
    pub fn replace_net(&self, net: NetworkStack) {
        let previous = {
            let mut current = self
                .net
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *current, net)
        };
        drop(previous);
    }

    pub fn new(
        app_handle: tauri::AppHandle,
        identity: SharedIdentity,
        storage: Arc<Storage>,
        http_client: reqwest::Client,
        net: NetworkStack,
    ) -> Self {
        Self {
            app_handle,
            identity,
            storage,
            http_client,
            net: std::sync::RwLock::new(net),
            exit_started: AtomicBool::new(false),
            replacing_identity: AtomicBool::new(false),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use iroh::{Endpoint, SecretKey, endpoint::PortmapperConfig, endpoint::presets};

    /// An endpoint bound to an ephemeral UDP port on 127.0.0.1 only, without relay,
    /// address lookup or port mapping, that accepts `alpn`.
    pub(crate) async fn loopback_endpoint(secret_key: SecretKey, alpn: &[u8]) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![alpn.to_vec()])
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .expect("valid bind address")
            .bind()
            .await
            .expect("bind loopback endpoint")
    }
}
