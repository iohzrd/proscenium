use hkdf::Hkdf;
use sha2::{Digest, Sha256, Sha512};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
use zeroize::Zeroize;

/// Derive the ratchet state storage encryption key from a DM secret key.
/// Used to encrypt ratchet sessions at rest.
pub fn derive_ratchet_storage_key(dm_secret_key_bytes: &[u8; 32]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, dm_secret_key_bytes);
    let mut key = [0u8; 32];
    hk.expand(b"proscenium-ratchet-storage-v1", &mut key)
        .expect("HKDF expand valid length");
    key
}

/// Convert an Ed25519 secret key (32-byte seed) to an X25519 private key.
/// Follows the standard conversion: SHA-512 the seed, take first 32 bytes, clamp per RFC 7748.
pub fn ed25519_secret_to_x25519(ed25519_secret: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha512::digest(ed25519_secret);
    let mut x25519_key = [0u8; 32];
    x25519_key.copy_from_slice(&hash[..32]);
    // Clamp per RFC 7748
    x25519_key[0] &= 248;
    x25519_key[31] &= 127;
    x25519_key[31] |= 64;
    hash.as_mut_slice().zeroize();
    x25519_key
}

/// Convert an Ed25519 public key (32 bytes) to an X25519 public key (32 bytes).
/// Uses the birational map from Edwards curve to Montgomery curve.
pub fn ed25519_public_to_x25519(ed25519_public: &[u8; 32]) -> Option<[u8; 32]> {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    let compressed = CompressedEdwardsY::from_slice(ed25519_public).ok()?;
    let point = compressed.decompress()?;
    let montgomery = point.to_montgomery();
    Some(montgomery.to_bytes())
}

/// Derive X25519 public key from X25519 private key.
pub fn x25519_public_from_private(private: &[u8; 32]) -> [u8; 32] {
    let secret = StaticSecret::from(*private);
    let public = X25519Public::from(&secret);
    public.to_bytes()
}

/// Clamp a raw 32-byte secret to an X25519 private key per RFC 7748 and derive the public key.
/// Used for DM keys that are already raw bytes (not Ed25519-derived).
pub fn x25519_keypair_from_raw(secret: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut private = *secret;
    private[0] &= 248;
    private[31] &= 127;
    private[31] |= 64;
    let public = x25519_public_from_private(&private);
    (private, public)
}

/// Perform X25519 Diffie-Hellman key agreement.
pub(super) fn x25519_dh(my_private: &[u8; 32], their_public: &[u8; 32]) -> [u8; 32] {
    let secret = StaticSecret::from(*my_private);
    let public = X25519Public::from(*their_public);
    let shared = secret.diffie_hellman(&public);
    shared.to_bytes()
}

/// Generate a new X25519 keypair for ratchet steps.
pub(super) fn generate_x25519_keypair() -> ([u8; 32], [u8; 32]) {
    let mut private_bytes = [0u8; 32];
    getrandom::fill(&mut private_bytes).expect("failed to generate random key");
    let public_bytes = x25519_public_from_private(&private_bytes);
    (private_bytes, public_bytes)
}
