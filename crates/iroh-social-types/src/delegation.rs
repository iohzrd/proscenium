use crate::signing::{hex_to_signature, signature_to_hex};
use hkdf::Hkdf;
use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Derive a signing key from the master key at a given index.
/// Hardened derivation: compromising the signing key does NOT reveal the master key.
pub fn derive_signing_key(master_secret: &[u8; 32], index: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&index.to_be_bytes()), master_secret);
    let mut signing_secret = [0u8; 32];
    hk.expand(b"iroh-social/signing-key", &mut signing_secret)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    signing_secret
}

/// Derive a stable transport key from the master key for a given device index.
/// Each device uses a unique index so linked devices get distinct NodeIds.
/// The primary device uses index 0.
pub fn derive_transport_key(master_secret: &[u8; 32], device_index: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&device_index.to_be_bytes()), master_secret);
    let mut transport_secret = [0u8; 32];
    hk.expand(b"iroh-social/transport-key", &mut transport_secret)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    transport_secret
}

/// Signed by the master key. Tells peers "this is my current signing key."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningKeyDelegation {
    /// The master public key (permanent identity).
    pub master_pubkey: String,
    /// The current signing public key (derived from master at this index).
    pub signing_pubkey: String,
    /// The derivation index.
    pub key_index: u32,
    /// When this delegation was issued (Unix timestamp ms).
    pub issued_at: u64,
    /// Ed25519 signature from the master key over the canonical bytes.
    pub signature: String,
}

/// Canonical bytes for signing a SigningKeyDelegation.
fn delegation_signing_bytes(
    master_pubkey: &str,
    signing_pubkey: &str,
    key_index: u32,
    issued_at: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "master_pubkey": master_pubkey,
        "signing_pubkey": signing_pubkey,
        "key_index": key_index,
        "issued_at": issued_at,
    }))
    .expect("json serialization should not fail")
}

/// Create and sign a SigningKeyDelegation.
pub fn sign_delegation(
    master_secret: &SecretKey,
    signing_pubkey: &PublicKey,
    key_index: u32,
    issued_at: u64,
) -> SigningKeyDelegation {
    let master_pubkey = master_secret.public().to_string();
    let signing_pubkey_str = signing_pubkey.to_string();
    let bytes =
        delegation_signing_bytes(&master_pubkey, &signing_pubkey_str, key_index, issued_at);
    let sig = master_secret.sign(&bytes);
    SigningKeyDelegation {
        master_pubkey,
        signing_pubkey: signing_pubkey_str,
        key_index,
        issued_at,
        signature: signature_to_hex(&sig),
    }
}

/// Verify a SigningKeyDelegation's signature against the master public key.
pub fn verify_delegation(delegation: &SigningKeyDelegation) -> Result<(), String> {
    let sig = hex_to_signature(&delegation.signature)?;
    let master_pubkey: PublicKey = delegation
        .master_pubkey
        .parse()
        .map_err(|e| format!("invalid master pubkey: {e}"))?;
    let _signing_pubkey: PublicKey = delegation
        .signing_pubkey
        .parse()
        .map_err(|e| format!("invalid signing pubkey: {e}"))?;
    let bytes = delegation_signing_bytes(
        &delegation.master_pubkey,
        &delegation.signing_pubkey,
        delegation.key_index,
        delegation.issued_at,
    );
    master_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "delegation signature verification failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::now_millis;

    #[test]
    fn test_derive_signing_key_deterministic() {
        let master = [42u8; 32];
        let k1 = derive_signing_key(&master, 0);
        let k2 = derive_signing_key(&master, 0);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_derive_signing_key_different_indices() {
        let master = [42u8; 32];
        let k0 = derive_signing_key(&master, 0);
        let k1 = derive_signing_key(&master, 1);
        assert_ne!(k0, k1);
    }

    #[test]
    fn test_derive_signing_key_different_masters() {
        let m1 = [1u8; 32];
        let m2 = [2u8; 32];
        let k1 = derive_signing_key(&m1, 0);
        let k2 = derive_signing_key(&m2, 0);
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_derive_transport_key_deterministic() {
        let master = [42u8; 32];
        let t1 = derive_transport_key(&master, 0);
        let t2 = derive_transport_key(&master, 0);
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_derive_transport_key_differs_from_signing_key() {
        let master = [42u8; 32];
        let transport = derive_transport_key(&master, 0);
        let signing = derive_signing_key(&master, 0);
        assert_ne!(transport, signing);
    }

    #[test]
    fn test_derive_transport_key_different_devices() {
        let master = [42u8; 32];
        let t0 = derive_transport_key(&master, 0);
        let t1 = derive_transport_key(&master, 1);
        assert_ne!(t0, t1);
    }

    #[test]
    fn test_sign_and_verify_delegation() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let signing_bytes = derive_signing_key(&master_bytes, 0);
        let signing_secret = SecretKey::from_bytes(&signing_bytes);
        let signing_pubkey = signing_secret.public();

        let delegation = sign_delegation(&master_secret, &signing_pubkey, 0, now_millis());
        assert!(verify_delegation(&delegation).is_ok());
    }

    #[test]
    fn test_tampered_delegation_fails() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let signing_bytes = derive_signing_key(&master_bytes, 0);
        let signing_secret = SecretKey::from_bytes(&signing_bytes);
        let signing_pubkey = signing_secret.public();

        let mut delegation = sign_delegation(&master_secret, &signing_pubkey, 0, now_millis());
        delegation.key_index = 1; // tamper
        assert!(verify_delegation(&delegation).is_err());
    }
}
