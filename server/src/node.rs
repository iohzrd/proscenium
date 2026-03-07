use iroh::{Endpoint, SecretKey};
use iroh_gossip::Gossip;
use iroh_social_types::PEER_ALPN;
use std::path::Path;

pub struct Node {
    pub endpoint: Endpoint,
    pub gossip: Gossip,
}

fn load_or_create_key(path: &Path) -> SecretKey {
    if path.exists() {
        let bytes = std::fs::read(path).expect("failed to read identity key");
        let bytes: [u8; 32] = bytes.try_into().expect("invalid key length");
        SecretKey::from_bytes(&bytes)
    } else {
        let mut key_bytes = [0u8; 32];
        getrandom::fill(&mut key_bytes).expect("failed to generate random key");
        let key = SecretKey::from_bytes(&key_bytes);
        std::fs::write(path, key.to_bytes()).expect("failed to write identity key");
        key
    }
}

impl Node {
    pub async fn start(data_dir: &Path) -> anyhow::Result<Self> {
        let secret_key = load_or_create_key(&data_dir.join("server.key"));

        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .alpns(vec![iroh_gossip::ALPN.to_vec(), PEER_ALPN.to_vec()])
            .bind()
            .await?;

        tracing::info!("server node ID: {}", endpoint.id());

        let gossip = Gossip::builder().spawn(endpoint.clone());

        // Register gossip protocol handler
        let _router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .spawn();

        Ok(Self { endpoint, gossip })
    }
}
