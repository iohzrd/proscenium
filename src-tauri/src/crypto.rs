use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, aead::Aead};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
use zeroize::Zeroize;

// -- Key Conversion --

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
fn x25519_dh(my_private: &[u8; 32], their_public: &[u8; 32]) -> [u8; 32] {
    let secret = StaticSecret::from(*my_private);
    let public = X25519Public::from(*their_public);
    let shared = secret.diffie_hellman(&public);
    shared.to_bytes()
}

/// Generate a new X25519 keypair for ratchet steps.
fn generate_x25519_keypair() -> ([u8; 32], [u8; 32]) {
    let mut private_bytes = [0u8; 32];
    getrandom::fill(&mut private_bytes).expect("failed to generate random key");
    let public_bytes = x25519_public_from_private(&private_bytes);
    (private_bytes, public_bytes)
}

// -- Noise IK Session Establishment --

const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const NOISE_PSK_PATTERN: &str = "Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s";

/// Perform the initiator side of Noise IK handshake.
/// Returns the handshake state and the first message to send.
pub fn noise_initiate(
    my_x25519_private: &[u8; 32],
    peer_x25519_public: &[u8; 32],
) -> Result<(snow::HandshakeState, Vec<u8>), snow::Error> {
    let mut initiator = snow::Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(my_x25519_private)?
        .remote_public_key(peer_x25519_public)?
        .build_initiator()?;

    let mut buf = vec![0u8; 65535];
    let len = initiator.write_message(&[], &mut buf)?;
    buf.truncate(len);
    Ok((initiator, buf))
}

/// Perform the responder side of Noise IK handshake.
/// Returns the handshake state and the response message to send.
pub fn noise_respond(
    my_x25519_private: &[u8; 32],
    message: &[u8],
) -> Result<(snow::HandshakeState, Vec<u8>), snow::Error> {
    let mut responder = snow::Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(my_x25519_private)?
        .build_responder()?;

    let mut payload = vec![0u8; 65535];
    let _len = responder.read_message(message, &mut payload)?;

    let mut buf = vec![0u8; 65535];
    let len = responder.write_message(&[], &mut buf)?;
    buf.truncate(len);
    Ok((responder, buf))
}

/// Complete the handshake on the initiator side.
/// Returns the handshake hash (shared secret for ratchet seeding).
pub fn noise_complete_initiator(
    mut initiator: snow::HandshakeState,
    response: &[u8],
) -> Result<[u8; 32], snow::Error> {
    let mut payload = vec![0u8; 65535];
    initiator.read_message(response, &mut payload)?;
    let hash = extract_handshake_hash(&initiator);
    // Transition to transport mode to complete the handshake properly
    let _transport = initiator.into_transport_mode()?;
    Ok(hash)
}

/// Complete the handshake on the responder side.
/// Returns `(handshake_hash, initiator_dm_pubkey)`.
/// The initiator's long-term X25519 key, authenticated by the Noise IK handshake.
pub fn noise_complete_responder(
    responder: snow::HandshakeState,
) -> Result<([u8; 32], Option<[u8; 32]>), snow::Error> {
    let hash = extract_handshake_hash(&responder);
    let initiator_dm_pubkey = responder.get_remote_static().and_then(|s| {
        let arr: [u8; 32] = s.try_into().ok()?;
        Some(arr)
    });
    let _transport = responder.into_transport_mode()?;
    Ok((hash, initiator_dm_pubkey))
}

/// Extract the handshake hash from a Noise handshake state.
fn extract_handshake_hash(hs: &snow::HandshakeState) -> [u8; 32] {
    let hash = hs.get_handshake_hash();
    let mut result = [0u8; 32];
    let len = hash.len().min(32);
    result[..len].copy_from_slice(&hash[..len]);
    result
}

// -- Noise IK+PSK for Device Pairing --

/// Initiator side of Noise IK+PSK handshake for device pairing.
/// The PSK is the one-time secret from the QR code.
/// `peer_x25519_public` is the existing device's X25519 public key.
pub fn noise_psk_initiate(
    my_x25519_private: &[u8; 32],
    peer_x25519_public: &[u8; 32],
    psk: &[u8; 32],
) -> Result<(snow::HandshakeState, Vec<u8>), snow::Error> {
    let mut initiator = snow::Builder::new(NOISE_PSK_PATTERN.parse()?)
        .local_private_key(my_x25519_private)?
        .remote_public_key(peer_x25519_public)?
        .psk(2, psk)?
        .build_initiator()?;

    let mut buf = vec![0u8; 65535];
    let len = initiator.write_message(&[], &mut buf)?;
    buf.truncate(len);
    Ok((initiator, buf))
}

/// Responder side of Noise IK+PSK handshake for device pairing.
/// The PSK is the one-time secret from the QR code.
/// Returns the handshake state, the response message, and the transport state.
pub fn noise_psk_respond(
    my_x25519_private: &[u8; 32],
    psk: &[u8; 32],
    message: &[u8],
) -> Result<(snow::TransportState, Vec<u8>), snow::Error> {
    let mut responder = snow::Builder::new(NOISE_PSK_PATTERN.parse()?)
        .local_private_key(my_x25519_private)?
        .psk(2, psk)?
        .build_responder()?;

    let mut payload = vec![0u8; 65535];
    let _len = responder.read_message(message, &mut payload)?;

    let mut buf = vec![0u8; 65535];
    let len = responder.write_message(&[], &mut buf)?;
    buf.truncate(len);

    let transport = responder.into_transport_mode()?;
    Ok((transport, buf))
}

/// Complete the PSK handshake on the initiator side and get transport state.
pub fn noise_psk_complete_initiator(
    mut initiator: snow::HandshakeState,
    response: &[u8],
) -> Result<snow::TransportState, snow::Error> {
    let mut payload = vec![0u8; 65535];
    initiator.read_message(response, &mut payload)?;
    initiator.into_transport_mode()
}

/// Encrypt data using a Noise transport state.
pub fn noise_transport_encrypt(
    transport: &mut snow::TransportState,
    plaintext: &[u8],
) -> Result<Vec<u8>, snow::Error> {
    let mut buf = vec![0u8; plaintext.len() + 65535];
    let len = transport.write_message(plaintext, &mut buf)?;
    buf.truncate(len);
    Ok(buf)
}

/// Decrypt data using a Noise transport state.
pub fn noise_transport_decrypt(
    transport: &mut snow::TransportState,
    ciphertext: &[u8],
) -> Result<Vec<u8>, snow::Error> {
    let mut buf = vec![0u8; ciphertext.len() + 65535];
    let len = transport.read_message(ciphertext, &mut buf)?;
    buf.truncate(len);
    Ok(buf)
}

// -- Double Ratchet --

/// Maximum number of skipped message keys to store.
const MAX_SKIP: u32 = 100;

/// HKDF info strings for KDF chain derivation.
const KDF_RK_INFO: &[u8] = b"iroh-social-dm-rk";
const KDF_CK_INFO_KEY: &[u8] = b"iroh-social-dm-ck-msg";
const KDF_CK_INFO_CHAIN: &[u8] = b"iroh-social-dm-ck-chain";

#[derive(Debug, Clone)]
pub enum CryptoError {
    DecryptionFailed,
    TooManySkipped,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::DecryptionFailed => write!(f, "decryption failed"),
            CryptoError::TooManySkipped => write!(f, "too many skipped messages"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// A skipped message key for out-of-order delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedKey {
    pub ratchet_public: [u8; 32],
    pub message_number: u32,
    pub message_key: [u8; 32],
}

/// The ratchet header sent with each encrypted message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetHeader {
    pub dh_public: [u8; 32],
    pub message_number: u32,
    pub previous_chain_length: u32,
}

/// Persistent ratchet state for a conversation.
/// Serialized to JSON and stored in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetState {
    pub dh_self_private: [u8; 32],
    pub dh_self_public: [u8; 32],
    pub dh_remote_public: Option<[u8; 32]>,
    pub root_key: [u8; 32],
    pub chain_key_send: Option<[u8; 32]>,
    pub chain_key_recv: Option<[u8; 32]>,
    pub send_count: u32,
    pub recv_count: u32,
    pub prev_send_count: u32,
    pub skipped_keys: Vec<SkippedKey>,
}

/// KDF for root key ratchet: derives a new root key and chain key from
/// the current root key and a DH output.
fn kdf_rk(root_key: &[u8; 32], dh_output: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<sha2::Sha256>::new(Some(root_key), dh_output);
    let mut output = [0u8; 64];
    hk.expand(KDF_RK_INFO, &mut output)
        .expect("HKDF output length valid");
    let mut new_root = [0u8; 32];
    let mut chain_key = [0u8; 32];
    new_root.copy_from_slice(&output[..32]);
    chain_key.copy_from_slice(&output[32..]);
    output.zeroize();
    (new_root, chain_key)
}

/// KDF for chain key: derives the next chain key and a message key.
fn kdf_ck(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<sha2::Sha256>::new(None, chain_key);
    let mut msg_key = [0u8; 32];
    hk.expand(KDF_CK_INFO_KEY, &mut msg_key)
        .expect("HKDF output length valid");
    let mut new_chain = [0u8; 32];
    hk.expand(KDF_CK_INFO_CHAIN, &mut new_chain)
        .expect("HKDF output length valid");
    (new_chain, msg_key)
}

/// Encrypt plaintext using a message key (ChaCha20Poly1305).
fn encrypt_message(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    // Use a zero nonce since each message key is unique (used exactly once).
    let nonce = Nonce::default();
    cipher
        .encrypt(&nonce, plaintext)
        .expect("encryption should not fail")
}

/// Decrypt ciphertext using a message key (ChaCha20Poly1305).
fn decrypt_message(key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = Nonce::default();
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| CryptoError::DecryptionFailed)
}

impl RatchetState {
    /// Initialize as the initiator (Alice) after Noise handshake.
    /// `shared_secret` is the Noise handshake hash.
    /// `bob_dh_public` is Bob's initial DH ratchet public key (his X25519 identity public).
    pub fn init_alice(shared_secret: &[u8; 32], bob_dh_public: &[u8; 32]) -> Self {
        let (dh_self_private, dh_self_public) = generate_x25519_keypair();
        let dh_output = x25519_dh(&dh_self_private, bob_dh_public);
        let (root_key, chain_key_send) = kdf_rk(shared_secret, &dh_output);

        RatchetState {
            dh_self_private,
            dh_self_public,
            dh_remote_public: Some(*bob_dh_public),
            root_key,
            chain_key_send: Some(chain_key_send),
            chain_key_recv: None,
            send_count: 0,
            recv_count: 0,
            prev_send_count: 0,
            skipped_keys: Vec::new(),
        }
    }

    /// Initialize as the responder (Bob) after Noise handshake.
    /// `shared_secret` is the Noise handshake hash.
    /// `bob_dh_keypair` is Bob's initial DH ratchet keypair (typically his X25519 identity key).
    pub fn init_bob(shared_secret: &[u8; 32], bob_dh_keypair: ([u8; 32], [u8; 32])) -> Self {
        RatchetState {
            dh_self_private: bob_dh_keypair.0,
            dh_self_public: bob_dh_keypair.1,
            dh_remote_public: None,
            root_key: *shared_secret,
            chain_key_send: None,
            chain_key_recv: None,
            send_count: 0,
            recv_count: 0,
            prev_send_count: 0,
            skipped_keys: Vec::new(),
        }
    }

    /// Encrypt a plaintext message. Returns the header and ciphertext.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> (RatchetHeader, Vec<u8>) {
        let chain_key = self
            .chain_key_send
            .expect("cannot encrypt without a sending chain key");
        let (new_chain, msg_key) = kdf_ck(&chain_key);
        self.chain_key_send = Some(new_chain);

        let header = RatchetHeader {
            dh_public: self.dh_self_public,
            message_number: self.send_count,
            previous_chain_length: self.prev_send_count,
        };
        self.send_count += 1;

        let ciphertext = encrypt_message(&msg_key, plaintext);
        (header, ciphertext)
    }

    /// Decrypt a received message.
    pub fn decrypt(
        &mut self,
        header: &RatchetHeader,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        // Try skipped message keys first
        if let Some(plaintext) = self.try_skipped_keys(header, ciphertext)? {
            return Ok(plaintext);
        }

        // Check if we need a DH ratchet step
        let need_dh_ratchet = self.dh_remote_public.as_ref() != Some(&header.dh_public);

        if need_dh_ratchet {
            self.skip_messages(header.previous_chain_length)?;
            self.dh_ratchet(&header.dh_public);
        }

        self.skip_messages(header.message_number)?;

        let chain_key = self.chain_key_recv.ok_or(CryptoError::DecryptionFailed)?;
        let (new_chain, msg_key) = kdf_ck(&chain_key);
        self.chain_key_recv = Some(new_chain);
        self.recv_count += 1;

        decrypt_message(&msg_key, ciphertext)
    }

    /// Try to decrypt using a skipped message key.
    fn try_skipped_keys(
        &mut self,
        header: &RatchetHeader,
        ciphertext: &[u8],
    ) -> Result<Option<Vec<u8>>, CryptoError> {
        let idx = self.skipped_keys.iter().position(|sk| {
            sk.ratchet_public == header.dh_public && sk.message_number == header.message_number
        });

        if let Some(idx) = idx {
            let sk = self.skipped_keys.remove(idx);
            let plaintext = decrypt_message(&sk.message_key, ciphertext)?;
            Ok(Some(plaintext))
        } else {
            Ok(None)
        }
    }

    /// Skip messages up to `until`, storing their message keys for later.
    fn skip_messages(&mut self, until: u32) -> Result<(), CryptoError> {
        if self.recv_count + MAX_SKIP < until {
            return Err(CryptoError::TooManySkipped);
        }

        if let Some(mut chain_key) = self.chain_key_recv {
            while self.recv_count < until {
                let (new_chain, msg_key) = kdf_ck(&chain_key);
                self.skipped_keys.push(SkippedKey {
                    ratchet_public: self.dh_remote_public.unwrap_or([0u8; 32]),
                    message_number: self.recv_count,
                    message_key: msg_key,
                });
                chain_key = new_chain;
                self.recv_count += 1;
            }
            self.chain_key_recv = Some(chain_key);
        }

        Ok(())
    }

    /// Perform a DH ratchet step when receiving a new DH public key.
    fn dh_ratchet(&mut self, new_remote_public: &[u8; 32]) {
        self.prev_send_count = self.send_count;
        self.send_count = 0;
        self.recv_count = 0;
        self.dh_remote_public = Some(*new_remote_public);

        // Derive receiving chain
        let dh_recv = x25519_dh(&self.dh_self_private, new_remote_public);
        let (root_key, chain_key_recv) = kdf_rk(&self.root_key, &dh_recv);
        self.root_key = root_key;
        self.chain_key_recv = Some(chain_key_recv);

        // Generate new DH keypair and derive sending chain
        let (new_private, new_public) = generate_x25519_keypair();
        self.dh_self_private = new_private;
        self.dh_self_public = new_public;

        let dh_send = x25519_dh(&self.dh_self_private, new_remote_public);
        let (root_key, chain_key_send) = kdf_rk(&self.root_key, &dh_send);
        self.root_key = root_key;
        self.chain_key_send = Some(chain_key_send);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_conversion_produces_valid_x25519() {
        let mut ed_secret = [0u8; 32];
        getrandom::fill(&mut ed_secret).unwrap();

        let x_private = ed25519_secret_to_x25519(&ed_secret);
        let x_public = x25519_public_from_private(&x_private);

        // Verify the key is clamped correctly
        assert_eq!(x_private[0] & 7, 0);
        assert_eq!(x_private[31] & 128, 0);
        assert_eq!(x_private[31] & 64, 64);

        // Verify public key is non-zero
        assert_ne!(x_public, [0u8; 32]);
    }

    #[test]
    fn test_ed25519_public_to_x25519() {
        let mut ed_secret = [0u8; 32];
        getrandom::fill(&mut ed_secret).unwrap();

        // Derive ed25519 public key using ed25519-dalek-compatible method:
        // The public key is the compressed Edwards Y coordinate of the scalar * basepoint.
        // We use curve25519_dalek directly since we have it as a dependency.
        let hash = Sha512::digest(&ed_secret);
        let mut scalar_bytes = [0u8; 32];
        scalar_bytes.copy_from_slice(&hash[..32]);
        scalar_bytes[0] &= 248;
        scalar_bytes[31] &= 127;
        scalar_bytes[31] |= 64;

        use curve25519_dalek::edwards::EdwardsPoint;
        use curve25519_dalek::scalar::Scalar;
        let scalar = Scalar::from_bytes_mod_order(scalar_bytes);
        let point = EdwardsPoint::mul_base(&scalar);
        let ed_public = point.compress().to_bytes();

        let x_public = ed25519_public_to_x25519(&ed_public);
        assert!(x_public.is_some());

        // The X25519 public derived from the ed25519 public should match
        // the X25519 public derived from the ed25519 secret
        let x_private = ed25519_secret_to_x25519(&ed_secret);
        let x_public_from_private = x25519_public_from_private(&x_private);
        assert_eq!(x_public.unwrap(), x_public_from_private);
    }

    #[test]
    fn test_noise_handshake() {
        let mut alice_ed = [0u8; 32];
        let mut bob_ed = [0u8; 32];
        getrandom::fill(&mut alice_ed).unwrap();
        getrandom::fill(&mut bob_ed).unwrap();

        let alice_x = ed25519_secret_to_x25519(&alice_ed);
        let bob_x = ed25519_secret_to_x25519(&bob_ed);
        let bob_x_pub = x25519_public_from_private(&bob_x);

        // Alice initiates (knows Bob's public key)
        let (alice_hs, msg1) = noise_initiate(&alice_x, &bob_x_pub).unwrap();

        // Bob responds
        let (bob_hs, msg2) = noise_respond(&bob_x, &msg1).unwrap();

        // Both complete and get the same handshake hash
        let alice_hash = noise_complete_initiator(alice_hs, &msg2).unwrap();
        let (bob_hash, _) = noise_complete_responder(bob_hs).unwrap();
        assert_eq!(alice_hash, bob_hash);
    }

    #[test]
    fn test_ratchet_basic_encrypt_decrypt() {
        let shared_secret = [42u8; 32];
        let bob_x_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let bob_x_public = x25519_public_from_private(&bob_x_private);

        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_public);
        let mut bob = RatchetState::init_bob(&shared_secret, (bob_x_private, bob_x_public));

        // Alice sends a message to Bob
        let plaintext = b"Hello Bob!";
        let (header, ciphertext) = alice.encrypt(plaintext);
        let decrypted = bob.decrypt(&header, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_ratchet_multiple_messages_same_direction() {
        let shared_secret = [42u8; 32];
        let bob_x_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let bob_x_public = x25519_public_from_private(&bob_x_private);

        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_public);
        let mut bob = RatchetState::init_bob(&shared_secret, (bob_x_private, bob_x_public));

        for i in 0..5 {
            let msg = format!("Message {i}");
            let (header, ciphertext) = alice.encrypt(msg.as_bytes());
            let decrypted = bob.decrypt(&header, &ciphertext).unwrap();
            assert_eq!(decrypted, msg.as_bytes());
        }
    }

    #[test]
    fn test_ratchet_alternating_directions() {
        let shared_secret = [42u8; 32];
        let bob_x_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let bob_x_public = x25519_public_from_private(&bob_x_private);

        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_public);
        let mut bob = RatchetState::init_bob(&shared_secret, (bob_x_private, bob_x_public));

        // Alice -> Bob
        let (h, c) = alice.encrypt(b"Hi Bob");
        assert_eq!(bob.decrypt(&h, &c).unwrap(), b"Hi Bob");

        // Bob -> Alice
        let (h, c) = bob.encrypt(b"Hi Alice");
        assert_eq!(alice.decrypt(&h, &c).unwrap(), b"Hi Alice");

        // Alice -> Bob again
        let (h, c) = alice.encrypt(b"How are you?");
        assert_eq!(bob.decrypt(&h, &c).unwrap(), b"How are you?");

        // Bob -> Alice again
        let (h, c) = bob.encrypt(b"Good!");
        assert_eq!(alice.decrypt(&h, &c).unwrap(), b"Good!");
    }

    #[test]
    fn test_ratchet_out_of_order_messages() {
        let shared_secret = [42u8; 32];
        let bob_x_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let bob_x_public = x25519_public_from_private(&bob_x_private);

        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_public);
        let mut bob = RatchetState::init_bob(&shared_secret, (bob_x_private, bob_x_public));

        // Alice sends 3 messages
        let (h1, c1) = alice.encrypt(b"msg1");
        let (h2, c2) = alice.encrypt(b"msg2");
        let (h3, c3) = alice.encrypt(b"msg3");

        // Bob receives them out of order
        assert_eq!(bob.decrypt(&h3, &c3).unwrap(), b"msg3");
        assert_eq!(bob.decrypt(&h1, &c1).unwrap(), b"msg1");
        assert_eq!(bob.decrypt(&h2, &c2).unwrap(), b"msg2");
    }

    #[test]
    fn test_ratchet_serialization() {
        let shared_secret = [42u8; 32];
        let bob_x_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let bob_x_public = x25519_public_from_private(&bob_x_private);

        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_public);

        // Send a message to advance state
        let _ = alice.encrypt(b"test");

        // Serialize and deserialize
        let json = serde_json::to_string(&alice).unwrap();
        let restored: RatchetState = serde_json::from_str(&json).unwrap();

        assert_eq!(alice.dh_self_public, restored.dh_self_public);
        assert_eq!(alice.root_key, restored.root_key);
        assert_eq!(alice.send_count, restored.send_count);
    }

    #[test]
    fn test_ratchet_wrong_key_fails() {
        let shared_secret = [42u8; 32];
        let bob_x_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let bob_x_public = x25519_public_from_private(&bob_x_private);

        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_public);

        // Eve has a different shared secret
        let eve_private = {
            let mut k = [0u8; 32];
            getrandom::fill(&mut k).unwrap();
            k
        };
        let eve_public = x25519_public_from_private(&eve_private);
        let mut eve = RatchetState::init_bob(&[99u8; 32], (eve_private, eve_public));

        let (header, ciphertext) = alice.encrypt(b"secret message");
        assert!(eve.decrypt(&header, &ciphertext).is_err());
    }

    #[test]
    fn test_full_noise_then_ratchet() {
        // Full integration: Noise handshake -> Double Ratchet conversation
        let mut alice_ed = [0u8; 32];
        let mut bob_ed = [0u8; 32];
        getrandom::fill(&mut alice_ed).unwrap();
        getrandom::fill(&mut bob_ed).unwrap();

        let alice_x = ed25519_secret_to_x25519(&alice_ed);
        let bob_x = ed25519_secret_to_x25519(&bob_ed);
        let bob_x_pub = x25519_public_from_private(&bob_x);

        // Noise handshake
        let (alice_hs, msg1) = noise_initiate(&alice_x, &bob_x_pub).unwrap();
        let (bob_hs, msg2) = noise_respond(&bob_x, &msg1).unwrap();
        let shared_secret = noise_complete_initiator(alice_hs, &msg2).unwrap();
        let (bob_shared, _) = noise_complete_responder(bob_hs).unwrap();
        assert_eq!(shared_secret, bob_shared);

        // Initialize ratchets
        // Bob uses his X25519 identity key as the initial ratchet key
        let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_pub);
        let mut bob = RatchetState::init_bob(&shared_secret, (bob_x, bob_x_pub));

        // Conversation
        let (h, c) = alice.encrypt(b"Hello from Alice");
        assert_eq!(bob.decrypt(&h, &c).unwrap(), b"Hello from Alice");

        let (h, c) = bob.encrypt(b"Hello from Bob");
        assert_eq!(alice.decrypt(&h, &c).unwrap(), b"Hello from Bob");

        let (h, c) = alice.encrypt(b"This is E2E encrypted!");
        assert_eq!(bob.decrypt(&h, &c).unwrap(), b"This is E2E encrypted!");
    }
}
