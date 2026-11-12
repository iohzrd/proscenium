use anyhow::Context;
use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use iroh_gossip::Gossip;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// Maximum size of one gossip frame. Must equal the client's `GOSSIP_MAX_MESSAGE_SIZE`:
/// a peer closes the gossip connection when it receives a larger frame.
pub const GOSSIP_MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Time allowed for `Router::shutdown` before the process exits anyway.
const ROUTER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Node {
    pub endpoint: Endpoint,
    pub gossip: Gossip,
    /// Owns the accept loop. The loop is aborted when the last `Router` clone is dropped.
    router: Router,
}

/// Read the 32-byte secret key at `path`, or create the file with a new random key.
///
/// A new key is written to a temporary file with mode 0600, synced, and renamed to
/// `path`, so `path` never holds a partial key.
fn load_or_create_key(path: &Path) -> anyhow::Result<SecretKey> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                anyhow::anyhow!(
                    "{} holds {} bytes, expected 32",
                    path.display(),
                    bytes.len()
                )
            })?;
            restrict_permissions(path)?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut key_bytes = [0u8; 32];
            getrandom::fill(&mut key_bytes)
                .map_err(|e| anyhow::anyhow!("failed to generate random key: {e}"))?;
            write_key_file(path, &key_bytes)
                .with_context(|| format!("failed to write {}", path.display()))?;
            Ok(SecretKey::from_bytes(&key_bytes))
        }
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_key_file(path: &Path, key_bytes: &[u8; 32]) -> std::io::Result<()> {
    let tmp_path = path.with_extension("key.tmp");
    // A temporary file left by an interrupted earlier run is replaced.
    match std::fs::remove_file(&tmp_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp_path)?;
    file.write_all(key_bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp_path, path)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Remove group and other access from an existing key file.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        tracing::warn!(
            "{} had mode {:o}; changed to 600",
            path.display(),
            mode & 0o777
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

impl Node {
    pub async fn start(data_dir: &Path) -> anyhow::Result<Self> {
        let secret_key = load_or_create_key(&data_dir.join("server.key"))?;

        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret_key)
            .transport_config(proscenium_types::quic_transport_config())
            .bind()
            .await?;

        tracing::info!("server node ID: {}", endpoint.id());
        Ok(Self::spawn(endpoint))
    }

    /// Start gossip and the accept loop on a bound endpoint.
    ///
    /// Only the gossip ALPN is accepted: `Router::spawn` sets the endpoint's ALPNs to
    /// those with a registered handler. The peer ALPN is used for outgoing sync
    /// connections only.
    pub(crate) fn spawn(endpoint: Endpoint) -> Self {
        let gossip = Gossip::builder()
            .max_message_size(GOSSIP_MAX_MESSAGE_SIZE)
            .spawn(endpoint.clone());

        let router = Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .spawn();

        Self {
            endpoint,
            gossip,
            router,
        }
    }

    /// Stop the accept loop, shut down the gossip handler and close the endpoint.
    pub async fn shutdown(&self) {
        match tokio::time::timeout(ROUTER_SHUTDOWN_TIMEOUT, self.router.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("iroh router task failed: {e}"),
            Err(_) => tracing::warn!(
                "iroh router shutdown did not finish within {}s",
                ROUTER_SHUTDOWN_TIMEOUT.as_secs()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "proscenium-server-test-{name}-{:016x}",
            u64::from_le_bytes(suffix)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn key_file_is_created_once_with_owner_only_mode() {
        let dir = temp_dir("key");
        let path = dir.join("server.key");
        let created = load_or_create_key(&path).unwrap();
        let loaded = load_or_create_key(&path).unwrap();
        assert_eq!(created.to_bytes(), loaded.to_bytes());
        assert!(!dir.join("server.key.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn malformed_key_file_is_an_error() {
        let dir = temp_dir("badkey");
        let path = dir.join("server.key");
        std::fs::write(&path, [1u8; 7]).unwrap();
        assert!(load_or_create_key(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
