use crate::call::CallHandler;
use crate::dm::DmHandler;
use crate::gossip::GossipService;
use crate::peer::PeerHandler;
use crate::stage::StageHandler;
use crate::storage::Storage;
use iroh::{Endpoint, SecretKey, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::fs::FsStore};
use proscenium_types::{
    DeviceEntry, MAX_LINKED_DEVICES, derive_dm_key, derive_signing_key, derive_transport_key,
    now_millis, sign_delegation,
};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Commands sent to the peer sync task to trigger on-demand syncs.
pub enum SyncCommand {
    /// Sync a specific peer immediately (e.g. after following).
    SyncPeer(String),
    /// Sync all followed peers immediately (e.g. app foreground, manual refresh).
    SyncAll,
}

/// Cryptographic identity and key material for this node.
/// Wrapped in `SharedIdentity` (Arc<RwLock<Identity>>) so key material can be
/// updated in place when signing keys or DM keys are rotated at runtime.
pub struct Identity {
    /// Master key secret bytes (permanent identity, cold storage).
    pub master_secret_key_bytes: [u8; 32],
    /// Master public key string (the permanent, unforgeable identity).
    pub master_pubkey: String,
    /// Signing key secret bytes (derived from master, signs content).
    pub signing_secret_key_bytes: [u8; 32],
    /// Pre-constructed signing SecretKey (avoids repeated from_bytes).
    pub signing_key: SecretKey,
    /// Signing key derivation index (0 for initial, incremented on rotation).
    pub signing_key_index: u32,
    /// DM key secret bytes (derived from master, used for Noise IK + Double Ratchet).
    /// No code outside tests reads this field at present; `dm_x25519_private` is the
    /// clamped form in use.
    #[allow(dead_code)]
    pub dm_secret_key_bytes: [u8; 32],
    /// DM public key string (hex-encoded X25519).
    pub dm_pubkey: String,
    /// DM key derivation index.
    pub dm_key_index: u32,
    /// X25519 private key bytes (derived from dm_secret_key_bytes).
    pub dm_x25519_private: [u8; 32],
    /// X25519 public key bytes (derived from dm_secret_key_bytes).
    pub dm_x25519_public: [u8; 32],
    /// Key for encrypting ratchet state at rest (derived from dm_secret_key_bytes).
    pub ratchet_storage_key: [u8; 32],
    /// Transport key derivation index of this device (0 for the primary device).
    pub device_index: u32,
    /// Transport NodeId string (iroh's own key, for QUIC networking).
    pub transport_node_id: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: proscenium_types::SigningKeyDelegation,
}

impl Identity {
    /// Derive every key from the master secret key and the three derivation indexes,
    /// and sign a delegation for the signing and DM keys with the current time.
    pub fn derive(
        master_secret_key_bytes: [u8; 32],
        device_index: u32,
        signing_key_index: u32,
        dm_key_index: u32,
    ) -> Self {
        let master_secret = SecretKey::from_bytes(&master_secret_key_bytes);
        let signing_secret_key_bytes =
            derive_signing_key(&master_secret_key_bytes, signing_key_index);
        let signing_key = SecretKey::from_bytes(&signing_secret_key_bytes);
        let dm_secret_key_bytes = derive_dm_key(&master_secret_key_bytes, dm_key_index);
        let (dm_x25519_private, dm_x25519_public) =
            crate::crypto::x25519_keypair_from_raw(&dm_secret_key_bytes);
        let dm_pubkey = hex::encode(dm_x25519_public);
        let delegation = sign_delegation(
            &master_secret,
            &signing_key.public(),
            signing_key_index,
            &dm_pubkey,
            dm_key_index,
            now_millis(),
        );

        Self {
            master_secret_key_bytes,
            master_pubkey: master_secret.public().to_string(),
            signing_secret_key_bytes,
            signing_key,
            signing_key_index,
            dm_secret_key_bytes,
            dm_pubkey,
            dm_key_index,
            dm_x25519_private,
            dm_x25519_public,
            ratchet_storage_key: crate::crypto::derive_ratchet_storage_key(&dm_secret_key_bytes),
            device_index,
            transport_node_id: device_node_id(&master_secret_key_bytes, device_index),
            delegation,
        }
    }

    /// Secret key of this device's iroh endpoint.
    pub fn transport_secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&derive_transport_key(
            &self.master_secret_key_bytes,
            self.device_index,
        ))
    }

    /// Transport key derivation index of `node_id`, or `None` if `node_id` is not the
    /// NodeId of a device of this identity.
    pub fn device_index_of(&self, node_id: &str) -> Option<u32> {
        device_index_of(&self.master_secret_key_bytes, node_id)
    }

    /// Keep the entries whose NodeId is a device of this identity, and set `is_primary`
    /// from the derivation index (index 0 is the primary device).
    pub fn own_devices(&self, devices: &[DeviceEntry]) -> Vec<DeviceEntry> {
        own_devices(&self.master_secret_key_bytes, devices)
    }
}

/// Prints the public fields only.
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("master_pubkey", &self.master_pubkey)
            .field("signing_pubkey", &self.signing_key.public())
            .field("signing_key_index", &self.signing_key_index)
            .field("dm_pubkey", &self.dm_pubkey)
            .field("dm_key_index", &self.dm_key_index)
            .field("device_index", &self.device_index)
            .field("transport_node_id", &self.transport_node_id)
            .finish_non_exhaustive()
    }
}

/// Transport NodeId of the device with derivation index `device_index`.
pub fn device_node_id(master_secret_key_bytes: &[u8; 32], device_index: u32) -> String {
    SecretKey::from_bytes(&derive_transport_key(master_secret_key_bytes, device_index))
        .public()
        .to_string()
}

/// Transport key derivation index (`0..MAX_LINKED_DEVICES`) whose NodeId equals `node_id`.
///
/// A device of an identity holds the master key and derives its transport key from it,
/// so a QUIC peer with such a NodeId is a device of that identity.
pub fn device_index_of(master_secret_key_bytes: &[u8; 32], node_id: &str) -> Option<u32> {
    (0..MAX_LINKED_DEVICES as u32)
        .find(|index| device_node_id(master_secret_key_bytes, *index) == node_id)
}

/// Keep the entries whose NodeId derives from the master key, and set `is_primary`
/// from the derivation index (index 0 is the primary device).
pub fn own_devices(
    master_secret_key_bytes: &[u8; 32],
    devices: &[DeviceEntry],
) -> Vec<DeviceEntry> {
    let node_ids: Vec<String> = (0..MAX_LINKED_DEVICES as u32)
        .map(|index| device_node_id(master_secret_key_bytes, index))
        .collect();
    devices
        .iter()
        .filter_map(|device| {
            let index = node_ids.iter().position(|id| *id == device.node_id)?;
            Some(DeviceEntry {
                is_primary: index == 0,
                ..device.clone()
            })
        })
        .collect()
}

/// Shared, mutable identity -- the canonical source for all key material.
pub type SharedIdentity = Arc<tokio::sync::RwLock<Identity>>;

/// Networking and protocol state that gets rebuilt during identity recovery.
pub struct NetworkStack {
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
    pub(crate) blob_store: FsStore,
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
        blob_store: FsStore,
        http_client: reqwest::Client,
        net: NetworkStack,
    ) -> Self {
        Self {
            app_handle,
            identity,
            storage,
            blob_store,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_index_is_found_only_for_derived_node_ids() {
        let master = [1u8; 32];
        let identity = Identity::derive(master, 3, 0, 0);
        assert_eq!(identity.transport_node_id, device_node_id(&master, 3));
        assert_eq!(
            identity.device_index_of(&identity.transport_node_id),
            Some(3)
        );
        assert_eq!(
            identity.device_index_of(&device_node_id(&master, 0)),
            Some(0)
        );
        let last = MAX_LINKED_DEVICES as u32 - 1;
        assert_eq!(
            identity.device_index_of(&device_node_id(&master, last)),
            Some(last)
        );
        // An index outside the range, another master key, the master pubkey itself, and
        // a string that is not a key.
        for node_id in [
            device_node_id(&master, MAX_LINKED_DEVICES as u32),
            device_node_id(&[2u8; 32], 0),
            identity.master_pubkey.clone(),
            "aaaaaaa\u{20ac}".to_string(),
        ] {
            assert_eq!(identity.device_index_of(&node_id), None);
        }
    }

    #[test]
    fn own_devices_drops_foreign_entries_and_sets_primary_from_index() {
        let master = [1u8; 32];
        let entry = |node_id: String, is_primary: bool| DeviceEntry {
            node_id,
            device_name: "d".to_string(),
            is_primary,
            added_at: 1,
        };
        let filtered = own_devices(
            &master,
            &[
                entry(device_node_id(&master, 0), false),
                entry(device_node_id(&[2u8; 32], 0), true),
                entry(device_node_id(&master, 2), true),
            ],
        );
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].node_id, device_node_id(&master, 0));
        assert!(filtered[0].is_primary);
        assert_eq!(filtered[1].node_id, device_node_id(&master, 2));
        assert!(!filtered[1].is_primary);
    }
}
