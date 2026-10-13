use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::keys::{generate_x25519_keypair, x25519_dh};

/// Largest accepted gap between the receiving chain position and the message number (or
/// previous chain length) in a header. One `skip_messages` call derives and stores at most
/// this many message keys.
pub(super) const MAX_SKIP: u32 = 100;

/// Maximum number of skipped message keys stored per session, across all receiving chains.
/// When the list is longer, the oldest keys are discarded and the messages they belong to
/// can no longer be decrypted.
pub(super) const MAX_SKIPPED_KEYS: usize = 1_000;

/// HKDF info strings for KDF chain derivation.
const KDF_RK_INFO: &[u8] = b"proscenium-dm-rk";
const KDF_CK_INFO_KEY: &[u8] = b"proscenium-dm-ck-msg";
const KDF_CK_INFO_CHAIN: &[u8] = b"proscenium-dm-ck-chain";

/// First bytes of the associated data of every message.
const AD_DOMAIN: &[u8] = b"proscenium-dm-msg-v2";

/// A 32-byte key that is overwritten with zeros when dropped.
type SecretBytes = Zeroizing<[u8; 32]>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The AEAD tag did not verify, or no receiving chain exists.
    DecryptionFailed,
    /// The header names a message more than `MAX_SKIP` positions ahead.
    TooManySkipped,
    /// The session has no sending chain: the responder has not received a message yet.
    NoSendingChain,
    /// A chain reached message number `u32::MAX`.
    ChainExhausted,
    /// The system random number generator returned an error.
    Rng,
    /// HKDF or AEAD encryption returned an error.
    Internal,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::DecryptionFailed => write!(f, "decryption failed"),
            CryptoError::TooManySkipped => write!(f, "too many skipped messages"),
            CryptoError::NoSendingChain => write!(f, "session has no sending chain"),
            CryptoError::ChainExhausted => write!(f, "message counter exhausted"),
            CryptoError::Rng => write!(f, "random number generator failed"),
            CryptoError::Internal => write!(f, "key derivation or encryption failed"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// A skipped message key for out-of-order delivery.
#[derive(Clone, Serialize, Deserialize)]
pub struct SkippedKey {
    pub ratchet_public: [u8; 32],
    pub message_number: u32,
    pub message_key: [u8; 32],
}

impl Zeroize for SkippedKey {
    fn zeroize(&mut self) {
        self.message_key.zeroize();
    }
}

impl Drop for SkippedKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for SkippedKey {}

impl std::fmt::Debug for SkippedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkippedKey")
            .field("message_number", &self.message_number)
            .finish_non_exhaustive()
    }
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
#[derive(Clone, Serialize, Deserialize)]
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

impl Zeroize for RatchetState {
    fn zeroize(&mut self) {
        self.dh_self_private.zeroize();
        self.root_key.zeroize();
        self.chain_key_send.zeroize();
        self.chain_key_recv.zeroize();
        self.skipped_keys.zeroize();
    }
}

impl Drop for RatchetState {
    fn drop(&mut self) {
        self.zeroize();
    }
}

impl ZeroizeOnDrop for RatchetState {}

impl std::fmt::Debug for RatchetState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RatchetState")
            .field("send_count", &self.send_count)
            .field("recv_count", &self.recv_count)
            .field("prev_send_count", &self.prev_send_count)
            .field("skipped_keys", &self.skipped_keys.len())
            .finish_non_exhaustive()
    }
}

/// KDF for root key ratchet: derives a new root key and chain key from
/// the current root key and a DH output. Returns `(root_key, chain_key)`.
fn kdf_rk(
    root_key: &[u8; 32],
    dh_output: &[u8; 32],
) -> Result<(SecretBytes, SecretBytes), CryptoError> {
    let hk = Hkdf::<sha2::Sha256>::new(Some(root_key), dh_output);
    let mut output = Zeroizing::new([0u8; 64]);
    hk.expand(KDF_RK_INFO, output.as_mut_slice())
        .map_err(|_| CryptoError::Internal)?;
    let mut new_root = Zeroizing::new([0u8; 32]);
    let mut chain_key = Zeroizing::new([0u8; 32]);
    let (first, second) = output.split_at(32);
    new_root.copy_from_slice(first);
    chain_key.copy_from_slice(second);
    Ok((new_root, chain_key))
}

/// KDF for chain key: derives the next chain key and a message key.
/// Returns `(chain_key, message_key)`.
fn kdf_ck(chain_key: &[u8; 32]) -> Result<(SecretBytes, SecretBytes), CryptoError> {
    let hk = Hkdf::<sha2::Sha256>::new(None, chain_key);
    let mut msg_key = Zeroizing::new([0u8; 32]);
    hk.expand(KDF_CK_INFO_KEY, msg_key.as_mut_slice())
        .map_err(|_| CryptoError::Internal)?;
    let mut new_chain = Zeroizing::new([0u8; 32]);
    hk.expand(KDF_CK_INFO_CHAIN, new_chain.as_mut_slice())
        .map_err(|_| CryptoError::Internal)?;
    Ok((new_chain, msg_key))
}

/// Associated data authenticated with a message: `AD_DOMAIN`, the long-term DM public
/// keys of the sender and the receiver, and the three header fields. Every part has a
/// fixed length.
fn associated_data(sender: &[u8; 32], receiver: &[u8; 32], header: &RatchetHeader) -> Vec<u8> {
    let mut ad = Vec::with_capacity(AD_DOMAIN.len() + 3 * 32 + 2 * 4);
    ad.extend_from_slice(AD_DOMAIN);
    ad.extend_from_slice(sender);
    ad.extend_from_slice(receiver);
    ad.extend_from_slice(&header.dh_public);
    ad.extend_from_slice(&header.message_number.to_be_bytes());
    ad.extend_from_slice(&header.previous_chain_length.to_be_bytes());
    ad
}

/// Encrypt plaintext using a message key (ChaCha20Poly1305).
fn encrypt_message(key: &[u8; 32], plaintext: &[u8], ad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::Internal)?;
    // Use a zero nonce since each message key is unique (used exactly once).
    let nonce = Nonce::default();
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: ad,
            },
        )
        .map_err(|_| CryptoError::Internal)
}

/// Decrypt ciphertext using a message key (ChaCha20Poly1305).
fn decrypt_message(key: &[u8; 32], ciphertext: &[u8], ad: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher =
        ChaCha20Poly1305::new_from_slice(key).map_err(|_| CryptoError::DecryptionFailed)?;
    let nonce = Nonce::default();
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: ad,
            },
        )
        .map_err(|_| CryptoError::DecryptionFailed)
}

impl RatchetState {
    /// Initialize as the initiator (Alice) after Noise handshake.
    /// `shared_secret` is the handshake-derived seed (`noise_complete_initiator`).
    /// `bob_dh_public` is Bob's initial DH ratchet public key (his X25519 identity public).
    pub fn init_alice(
        shared_secret: &[u8; 32],
        bob_dh_public: &[u8; 32],
    ) -> Result<Self, CryptoError> {
        let (dh_self_private, dh_self_public) = generate_x25519_keypair()?;
        let dh_output = x25519_dh(&dh_self_private, bob_dh_public);
        let (root_key, chain_key_send) = kdf_rk(shared_secret, &dh_output)?;

        Ok(RatchetState {
            dh_self_private: *dh_self_private,
            dh_self_public,
            dh_remote_public: Some(*bob_dh_public),
            root_key: *root_key,
            chain_key_send: Some(*chain_key_send),
            chain_key_recv: None,
            send_count: 0,
            recv_count: 0,
            prev_send_count: 0,
            skipped_keys: Vec::new(),
        })
    }

    /// Initialize as the responder (Bob) after Noise handshake.
    /// `shared_secret` is the handshake-derived seed (`noise_complete_responder`).
    /// `bob_dh_keypair` is Bob's first ratchet key pair `(private, public)`. The caller
    /// passes Bob's long-term X25519 DM key pair, because that is the only key of Bob the
    /// initiator holds before Bob's first message.
    ///
    /// Consequence: the DH input of the initiator's first sending chain is
    /// DH(initiator's first ratchet key, Bob's long-term DM key). An attacker who recorded
    /// the handshake and the envelopes and later obtains Bob's DM private key can compute
    /// that DH output. The messages the initiator sent before Bob's first reply are then
    /// protected only by `shared_secret`. `shared_secret` depends on the
    /// ephemeral-ephemeral DH of the handshake, which neither long-term private key
    /// reveals, so the recording and the DM private key alone decrypt none of those
    /// messages. They are decrypted if the attacker also holds `shared_secret`: Bob's
    /// stored session contains it as `root_key` until his first received message replaces
    /// the root key, and that row is encrypted with a key derived from the same DM secret.
    /// Messages sent after Bob's first reply use ratchet keys generated per DH step and
    /// are not affected.
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
    ///
    /// `sender` and `receiver` are the long-term DM public keys of the local user and of
    /// the peer. They and the header are authenticated as associated data.
    /// Returns `NoSendingChain` if the session cannot send yet. The state changes only
    /// if encryption succeeds.
    pub fn encrypt(
        &mut self,
        plaintext: &[u8],
        sender: &[u8; 32],
        receiver: &[u8; 32],
    ) -> Result<(RatchetHeader, Vec<u8>), CryptoError> {
        let chain_key = Zeroizing::new(self.chain_key_send.ok_or(CryptoError::NoSendingChain)?);
        let next_send_count = self
            .send_count
            .checked_add(1)
            .ok_or(CryptoError::ChainExhausted)?;
        let (new_chain, msg_key) = kdf_ck(&chain_key)?;

        let header = RatchetHeader {
            dh_public: self.dh_self_public,
            message_number: self.send_count,
            previous_chain_length: self.prev_send_count,
        };
        let ad = associated_data(sender, receiver, &header);
        let ciphertext = encrypt_message(&msg_key, plaintext, &ad)?;

        self.chain_key_send = Some(*new_chain);
        self.send_count = next_send_count;
        Ok((header, ciphertext))
    }

    /// Decrypt a received message.
    ///
    /// `sender` and `receiver` are the long-term DM public keys of the peer and of the
    /// local user. The state changes only if the message authenticates.
    pub fn decrypt(
        &mut self,
        header: &RatchetHeader,
        ciphertext: &[u8],
        sender: &[u8; 32],
        receiver: &[u8; 32],
    ) -> Result<Vec<u8>, CryptoError> {
        let ad = associated_data(sender, receiver, header);
        // The header selects the ratchet steps before it is authenticated. The steps run
        // on a copy that replaces the state only after the AEAD tag verified.
        let mut next = self.clone();
        let plaintext = next.decrypt_step(header, ciphertext, &ad)?;
        *self = next;
        Ok(plaintext)
    }

    fn decrypt_step(
        &mut self,
        header: &RatchetHeader,
        ciphertext: &[u8],
        ad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        // A stored skipped key for this header decrypts the message without a ratchet step.
        if let Some(plaintext) = self.try_skipped_keys(header, ciphertext, ad)? {
            return Ok(plaintext);
        }

        // A header with a new ratchet public key requires a DH ratchet step.
        let need_dh_ratchet = self.dh_remote_public.as_ref() != Some(&header.dh_public);

        if need_dh_ratchet {
            self.skip_messages(header.previous_chain_length)?;
            self.dh_ratchet(&header.dh_public)?;
        }

        self.skip_messages(header.message_number)?;

        let chain_key = Zeroizing::new(self.chain_key_recv.ok_or(CryptoError::DecryptionFailed)?);
        let next_recv_count = self
            .recv_count
            .checked_add(1)
            .ok_or(CryptoError::ChainExhausted)?;
        let (new_chain, msg_key) = kdf_ck(&chain_key)?;
        let plaintext = decrypt_message(&msg_key, ciphertext, ad)?;

        self.chain_key_recv = Some(*new_chain);
        self.recv_count = next_recv_count;
        Ok(plaintext)
    }

    /// Decrypt with the stored skipped key for `header`, removing the key.
    /// Returns `Ok(None)` if no key is stored for that header.
    fn try_skipped_keys(
        &mut self,
        header: &RatchetHeader,
        ciphertext: &[u8],
        ad: &[u8],
    ) -> Result<Option<Vec<u8>>, CryptoError> {
        let idx = self.skipped_keys.iter().position(|sk| {
            sk.ratchet_public == header.dh_public && sk.message_number == header.message_number
        });

        if let Some(idx) = idx {
            let sk = self.skipped_keys.remove(idx);
            let plaintext = decrypt_message(&sk.message_key, ciphertext, ad)?;
            Ok(Some(plaintext))
        } else {
            Ok(None)
        }
    }

    /// Skip messages up to `until`, storing their message keys for later.
    /// Keeps at most `MAX_SKIPPED_KEYS` keys; the oldest are discarded first.
    fn skip_messages(&mut self, until: u32) -> Result<(), CryptoError> {
        if until.saturating_sub(self.recv_count) > MAX_SKIP {
            return Err(CryptoError::TooManySkipped);
        }

        let (Some(chain_key), Some(ratchet_public)) = (self.chain_key_recv, self.dh_remote_public)
        else {
            return Ok(());
        };
        let mut chain_key = Zeroizing::new(chain_key);
        while self.recv_count < until {
            let (new_chain, msg_key) = kdf_ck(&chain_key)?;
            self.skipped_keys.push(SkippedKey {
                ratchet_public,
                message_number: self.recv_count,
                message_key: *msg_key,
            });
            chain_key = new_chain;
            self.recv_count += 1;
        }
        self.chain_key_recv = Some(*chain_key);

        if self.skipped_keys.len() > MAX_SKIPPED_KEYS {
            let excess = self.skipped_keys.len() - MAX_SKIPPED_KEYS;
            self.skipped_keys.drain(..excess);
        }

        Ok(())
    }

    /// Perform a DH ratchet step when receiving a new DH public key.
    fn dh_ratchet(&mut self, new_remote_public: &[u8; 32]) -> Result<(), CryptoError> {
        // Derive receiving chain
        let dh_recv = x25519_dh(&self.dh_self_private, new_remote_public);
        let (root_key, chain_key_recv) = kdf_rk(&self.root_key, &dh_recv)?;

        // Generate new DH keypair and derive sending chain
        let (new_private, new_public) = generate_x25519_keypair()?;
        let dh_send = x25519_dh(&new_private, new_remote_public);
        let (root_key, chain_key_send) = kdf_rk(&root_key, &dh_send)?;

        self.prev_send_count = self.send_count;
        self.send_count = 0;
        self.recv_count = 0;
        self.dh_remote_public = Some(*new_remote_public);
        self.dh_self_private = *new_private;
        self.dh_self_public = new_public;
        self.root_key = *root_key;
        self.chain_key_recv = Some(*chain_key_recv);
        self.chain_key_send = Some(*chain_key_send);
        Ok(())
    }
}
