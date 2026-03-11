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

/// Derive a DM key from the master key at a given index.
/// The output is a raw 32-byte secret that gets clamped to an X25519 private key.
/// This key is used exclusively for Diffie-Hellman (Noise IK + Double Ratchet).
pub fn derive_dm_key(master_secret: &[u8; 32], index: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&index.to_be_bytes()), master_secret);
    let mut dm_secret = [0u8; 32];
    hk.expand(b"iroh-social/dm-key", &mut dm_secret)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    dm_secret
}

/// Signed by the master key. Tells peers "these are my current signing and DM keys."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningKeyDelegation {
    /// The master public key (permanent identity).
    pub master_pubkey: String,
    /// The current signing public key (derived from master at this index).
    pub signing_pubkey: String,
    /// The signing key derivation index.
    pub key_index: u32,
    /// The current DM public key (hex-encoded X25519, for Noise IK + Double Ratchet).
    pub dm_pubkey: String,
    /// The DM key derivation index.
    pub dm_key_index: u32,
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
    dm_pubkey: &str,
    dm_key_index: u32,
    issued_at: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "master_pubkey": master_pubkey,
        "signing_pubkey": signing_pubkey,
        "key_index": key_index,
        "dm_pubkey": dm_pubkey,
        "dm_key_index": dm_key_index,
        "issued_at": issued_at,
    }))
    .expect("json serialization should not fail")
}

/// Create and sign a SigningKeyDelegation.
pub fn sign_delegation(
    master_secret: &SecretKey,
    signing_pubkey: &PublicKey,
    key_index: u32,
    dm_pubkey: &str,
    dm_key_index: u32,
    issued_at: u64,
) -> SigningKeyDelegation {
    let master_pubkey = master_secret.public().to_string();
    let signing_pubkey_str = signing_pubkey.to_string();
    let bytes = delegation_signing_bytes(
        &master_pubkey,
        &signing_pubkey_str,
        key_index,
        dm_pubkey,
        dm_key_index,
        issued_at,
    );
    let sig = master_secret.sign(&bytes);
    SigningKeyDelegation {
        master_pubkey,
        signing_pubkey: signing_pubkey_str,
        key_index,
        dm_pubkey: dm_pubkey.to_string(),
        dm_key_index,
        issued_at,
        signature: signature_to_hex(&sig),
    }
}

/// Signed by the master key. Announces that the old signing key has been replaced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningKeyRotation {
    /// The master public key (permanent identity -- unchanged).
    pub master_pubkey: String,
    /// The old signing pubkey (being revoked).
    pub old_signing_pubkey: String,
    /// The new signing pubkey (replacing it).
    pub new_signing_pubkey: String,
    /// The new key's derivation index.
    pub new_key_index: u32,
    /// When the rotation was issued (Unix timestamp ms).
    pub timestamp: u64,
    /// Signed by the MASTER key (proves the identity owner initiated rotation).
    pub signature: String,
    /// The new delegation (signed by master key), so peers can immediately cache it.
    pub new_delegation: SigningKeyDelegation,
}

/// Canonical bytes for signing a SigningKeyRotation.
fn rotation_signing_bytes(
    master_pubkey: &str,
    old_signing_pubkey: &str,
    new_signing_pubkey: &str,
    new_key_index: u32,
    timestamp: u64,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "signing_key_rotation",
        "master_pubkey": master_pubkey,
        "old_signing_pubkey": old_signing_pubkey,
        "new_signing_pubkey": new_signing_pubkey,
        "new_key_index": new_key_index,
        "timestamp": timestamp,
    }))
    .expect("json serialization should not fail")
}

/// Create and sign a SigningKeyRotation.
pub fn sign_rotation(
    master_secret: &SecretKey,
    old_signing_pubkey: &PublicKey,
    new_signing_pubkey: &PublicKey,
    new_key_index: u32,
    timestamp: u64,
    new_delegation: SigningKeyDelegation,
) -> SigningKeyRotation {
    let master_pubkey = master_secret.public().to_string();
    let old_str = old_signing_pubkey.to_string();
    let new_str = new_signing_pubkey.to_string();
    let bytes =
        rotation_signing_bytes(&master_pubkey, &old_str, &new_str, new_key_index, timestamp);
    let sig = master_secret.sign(&bytes);
    SigningKeyRotation {
        master_pubkey,
        old_signing_pubkey: old_str,
        new_signing_pubkey: new_str,
        new_key_index,
        timestamp,
        signature: signature_to_hex(&sig),
        new_delegation,
    }
}

/// Verify a SigningKeyRotation's signature against the master public key.
/// Also verifies the embedded new_delegation.
pub fn verify_rotation(rotation: &SigningKeyRotation) -> Result<(), String> {
    // Verify the rotation signature itself
    let sig = hex_to_signature(&rotation.signature)?;
    let master_pubkey: PublicKey = rotation
        .master_pubkey
        .parse()
        .map_err(|e| format!("invalid master pubkey: {e}"))?;
    let bytes = rotation_signing_bytes(
        &rotation.master_pubkey,
        &rotation.old_signing_pubkey,
        &rotation.new_signing_pubkey,
        rotation.new_key_index,
        rotation.timestamp,
    );
    master_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "rotation signature verification failed".to_string())?;

    // Verify the embedded delegation
    verify_delegation(&rotation.new_delegation)?;

    // Check consistency: delegation must match rotation fields
    if rotation.new_delegation.master_pubkey != rotation.master_pubkey {
        return Err("rotation delegation master_pubkey mismatch".to_string());
    }
    if rotation.new_delegation.signing_pubkey != rotation.new_signing_pubkey {
        return Err("rotation delegation signing_pubkey mismatch".to_string());
    }
    if rotation.new_delegation.key_index != rotation.new_key_index {
        return Err("rotation delegation key_index mismatch".to_string());
    }

    Ok(())
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
        &delegation.dm_pubkey,
        delegation.dm_key_index,
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

    fn test_dm_pubkey(master_bytes: &[u8; 32], index: u32) -> String {
        let dm_secret = derive_dm_key(master_bytes, index);
        let clamped = clamp_x25519(&dm_secret);
        let secret = x25519_dalek::StaticSecret::from(clamped);
        let public = x25519_dalek::PublicKey::from(&secret);
        hex::encode(public.as_bytes())
    }

    fn clamp_x25519(secret: &[u8; 32]) -> [u8; 32] {
        let mut k = *secret;
        k[0] &= 248;
        k[31] &= 127;
        k[31] |= 64;
        k
    }

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
    fn test_derive_dm_key_deterministic() {
        let master = [42u8; 32];
        let k1 = derive_dm_key(&master, 0);
        let k2 = derive_dm_key(&master, 0);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_derive_dm_key_different_indices() {
        let master = [42u8; 32];
        let k0 = derive_dm_key(&master, 0);
        let k1 = derive_dm_key(&master, 1);
        assert_ne!(k0, k1);
    }

    #[test]
    fn test_derive_dm_key_differs_from_signing_and_transport() {
        let master = [42u8; 32];
        let dm = derive_dm_key(&master, 0);
        let signing = derive_signing_key(&master, 0);
        let transport = derive_transport_key(&master, 0);
        assert_ne!(dm, signing);
        assert_ne!(dm, transport);
    }

    #[test]
    fn test_sign_and_verify_delegation() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let signing_bytes = derive_signing_key(&master_bytes, 0);
        let signing_secret = SecretKey::from_bytes(&signing_bytes);
        let signing_pubkey = signing_secret.public();

        let dm_pubkey = test_dm_pubkey(&master_bytes, 0);

        let delegation = sign_delegation(
            &master_secret,
            &signing_pubkey,
            0,
            &dm_pubkey,
            0,
            now_millis(),
        );
        assert!(verify_delegation(&delegation).is_ok());
    }

    #[test]
    fn test_sign_and_verify_rotation() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let old_signing = derive_signing_key(&master_bytes, 0);
        let old_signing_pub = SecretKey::from_bytes(&old_signing).public();

        let new_signing = derive_signing_key(&master_bytes, 1);
        let new_signing_pub = SecretKey::from_bytes(&new_signing).public();

        let dm_pubkey = test_dm_pubkey(&master_bytes, 0);

        let now = now_millis();
        let new_delegation =
            sign_delegation(&master_secret, &new_signing_pub, 1, &dm_pubkey, 0, now);
        let rotation = sign_rotation(
            &master_secret,
            &old_signing_pub,
            &new_signing_pub,
            1,
            now,
            new_delegation,
        );
        assert!(verify_rotation(&rotation).is_ok());
    }

    #[test]
    fn test_tampered_rotation_fails() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let old_signing = derive_signing_key(&master_bytes, 0);
        let old_signing_pub = SecretKey::from_bytes(&old_signing).public();

        let new_signing = derive_signing_key(&master_bytes, 1);
        let new_signing_pub = SecretKey::from_bytes(&new_signing).public();

        let dm_pubkey = test_dm_pubkey(&master_bytes, 0);

        let now = now_millis();
        let new_delegation =
            sign_delegation(&master_secret, &new_signing_pub, 1, &dm_pubkey, 0, now);
        let mut rotation = sign_rotation(
            &master_secret,
            &old_signing_pub,
            &new_signing_pub,
            1,
            now,
            new_delegation,
        );
        rotation.new_key_index = 2; // tamper
        assert!(verify_rotation(&rotation).is_err());
    }

    #[test]
    fn test_tampered_delegation_fails() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let signing_bytes = derive_signing_key(&master_bytes, 0);
        let signing_secret = SecretKey::from_bytes(&signing_bytes);
        let signing_pubkey = signing_secret.public();

        let dm_pubkey = test_dm_pubkey(&master_bytes, 0);

        let mut delegation = sign_delegation(
            &master_secret,
            &signing_pubkey,
            0,
            &dm_pubkey,
            0,
            now_millis(),
        );
        delegation.key_index = 1; // tamper
        assert!(verify_delegation(&delegation).is_err());
    }

    #[test]
    fn test_tampered_dm_pubkey_fails() {
        let mut master_bytes = [0u8; 32];
        getrandom::fill(&mut master_bytes).unwrap();
        let master_secret = SecretKey::from_bytes(&master_bytes);

        let signing_bytes = derive_signing_key(&master_bytes, 0);
        let signing_secret = SecretKey::from_bytes(&signing_bytes);
        let signing_pubkey = signing_secret.public();

        let dm_pubkey = test_dm_pubkey(&master_bytes, 0);

        let mut delegation = sign_delegation(
            &master_secret,
            &signing_pubkey,
            0,
            &dm_pubkey,
            0,
            now_millis(),
        );
        delegation.dm_pubkey = "ff".repeat(32); // tamper
        assert!(verify_delegation(&delegation).is_err());
    }
}
