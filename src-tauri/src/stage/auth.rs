use crate::error::AppError;
use iroh::{PublicKey, SecretKey, Signature};
use sha2::{Digest, Sha256};

/// Sign the chain hash every N frames (~1 second at 50fps Opus).
pub const CHECKPOINT_INTERVAL: u64 = 50;

/// Byte size of the checkpoint auth extension prepended to the Opus payload.
/// chain_hash(32) + signature(64) = 96 bytes.
pub const CHECKPOINT_EXTRA: usize = 96;

// ---- HostAuthState ------------------------------------------------------

/// Maintained by the HostMixer to authenticate each outgoing mixed frame.
///
/// chain_hash ← SHA256(chain_hash ‖ opus_payload)
/// Every CHECKPOINT_INTERVAL frames: sign chain_hash with host signing key.
pub struct HostAuthState {
    chain_hash: [u8; 32],
    sequence: u64,
    signing_key: SecretKey,
}

impl HostAuthState {
    pub fn new(signing_key: SecretKey) -> Self {
        Self {
            chain_hash: [0u8; 32],
            sequence: 0,
            signing_key,
        }
    }

    /// Process one Opus frame. Returns `(tag_byte, wire_payload)`:
    ///   Normal:     (TAG_NORMAL,      opus_bytes)
    ///   Checkpoint: (TAG_CHECKPOINT,  hash(32) + sig(64) + opus_bytes)
    pub fn process(&mut self, opus: &[u8]) -> (u8, Vec<u8>) {
        let mut hasher = Sha256::new();
        hasher.update(self.chain_hash);
        hasher.update(opus);
        self.chain_hash = hasher.finalize().into();
        self.sequence += 1;

        if self.sequence.is_multiple_of(CHECKPOINT_INTERVAL) {
            let sig: Signature = self.signing_key.sign(&self.chain_hash);
            let mut buf = Vec::with_capacity(CHECKPOINT_EXTRA + opus.len());
            buf.extend_from_slice(&self.chain_hash);
            buf.extend_from_slice(&sig.to_bytes());
            buf.extend_from_slice(opus);
            (crate::audio::transport::TAG_CHECKPOINT, buf)
        } else {
            (crate::audio::TAG_NORMAL, opus.to_vec())
        }
    }
}

// ---- ListenerAuthState --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthResult {
    /// Accepted, no checkpoint in this frame.
    Ok,
    /// No checkpoint verified yet; playing unverified audio (late joiner).
    Unverified,
    /// Checkpoint valid — all frames since last checkpoint are authentic.
    Verified,
    /// Chain hash mismatch — relay tampered with frames.
    TamperDetected,
    /// Signature invalid — not signed by the expected host key.
    InvalidSignature,
}

/// Maintained by the listener to verify the incoming mixed stream.
pub struct ListenerAuthState {
    chain_hash: [u8; 32],
    verified: bool,
    /// Set permanently on the first `TamperDetected` result. Once compromised,
    /// every subsequent frame returns `TamperDetected` — the chain cannot recover
    /// because we cannot know which frames were tampered with. The caller should
    /// disconnect and reconnect to get a fresh auth state.
    compromised: bool,
    host_pubkey: PublicKey,
}

impl ListenerAuthState {
    pub fn new(host_pubkey: PublicKey) -> Self {
        Self {
            chain_hash: [0u8; 32],
            verified: false,
            compromised: false,
            host_pubkey,
        }
    }

    /// Verify one incoming frame.
    ///
    /// `opus`       — raw Opus bytes (checkpoint prefix already stripped).
    /// `tag_byte`   — TAG_NORMAL or TAG_CHECKPOINT.
    /// `checkpoint` — `Some((wire_hash, wire_sig))` when tag == TAG_CHECKPOINT.
    pub fn verify_frame(
        &mut self,
        opus: &[u8],
        tag_byte: u8,
        checkpoint: Option<([u8; 32], [u8; 64])>,
    ) -> AuthResult {
        // Once compromised, every frame is rejected until the caller reconnects.
        if self.compromised {
            return AuthResult::TamperDetected;
        }

        if !self.verified {
            // Late joiner: we don't have the prior chain state, so we cannot
            // verify the hash chain until we receive and signature-verify a
            // checkpoint. Normal frames before the first checkpoint are played
            // unverified. On the first checkpoint we trust the signature alone
            // (not the chain hash, which we can't reconstruct) and adopt
            // `wire_hash` as the new chain baseline going forward.
            let Some((wire_hash, wire_sig)) = checkpoint else {
                return AuthResult::Unverified;
            };
            let sig = Signature::from_bytes(&wire_sig);
            return match self.host_pubkey.verify(&wire_hash, &sig) {
                Ok(()) => {
                    self.chain_hash = wire_hash;
                    self.verified = true;
                    AuthResult::Verified
                }
                Err(_) => {
                    self.compromised = true;
                    AuthResult::InvalidSignature
                }
            };
        }

        // Verified path: extend local chain identically to the host.
        let mut hasher = Sha256::new();
        hasher.update(self.chain_hash);
        hasher.update(opus);
        self.chain_hash = hasher.finalize().into();

        if tag_byte == crate::audio::TAG_NORMAL {
            return AuthResult::Ok;
        }

        // Checkpoint: verify chain hash matches and signature is valid.
        let Some((wire_hash, wire_sig)) = checkpoint else {
            return AuthResult::Ok;
        };

        if self.chain_hash != wire_hash {
            self.compromised = true;
            return AuthResult::TamperDetected;
        }

        let sig = Signature::from_bytes(&wire_sig);
        match self.host_pubkey.verify(&wire_hash, &sig) {
            Ok(()) => AuthResult::Verified,
            Err(_) => {
                self.compromised = true;
                AuthResult::InvalidSignature
            }
        }
    }
}

// ---- Wire format helper -------------------------------------------------

/// Decode a checkpoint payload: `[chain_hash:32][signature:64][opus...]`
///
/// Returns `(hash, sig, opus_slice)`.
#[allow(clippy::type_complexity)]
pub fn decode_checkpoint_payload(payload: &[u8]) -> Result<([u8; 32], [u8; 64], &[u8]), AppError> {
    if payload.len() < CHECKPOINT_EXTRA {
        return Err(AppError::Other(format!(
            "checkpoint payload too short: {} bytes",
            payload.len()
        )));
    }
    let hash: [u8; 32] = payload[0..32].try_into().unwrap();
    let sig: [u8; 64] = payload[32..96].try_into().unwrap();
    Ok((hash, sig, &payload[96..]))
}
