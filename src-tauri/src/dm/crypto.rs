use crate::crypto::{RatchetHeader, RatchetState};
use crate::error::AppError;
use base64::Engine as _;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce, aead::Aead};
use proscenium_types::{EncryptedEnvelope, RatchetHeaderWire, now_millis, short_id};
use zeroize::Zeroizing;

use super::{DmHandler, DmKeys, PeerRoute};

/// Serialize a ratchet state to JSON and encrypt it with ChaCha20Poly1305.
/// Returns base64(nonce || ciphertext).
pub(super) fn seal_session(key: &[u8; 32], ratchet: &RatchetState) -> Result<String, AppError> {
    let plaintext = Zeroizing::new(serde_json::to_vec(ratchet)?);
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| AppError::Other("ratchet state key has invalid length".into()))?;
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes)?;
    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_slice())
        .map_err(|_| AppError::Other("ratchet state encryption failed".into()))?;
    let mut blob = nonce_bytes.to_vec();
    blob.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(&blob))
}

/// Decrypt and parse a ratchet state stored by `seal_session`.
pub(super) fn open_session(key: &[u8; 32], stored: &str) -> Result<RatchetState, AppError> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(stored)
        .map_err(|_| AppError::Other("invalid base64 in ratchet state".into()))?;
    let Some((nonce_bytes, ciphertext)) = blob.split_at_checked(12) else {
        return Err(AppError::Other("ratchet state too short".into()));
    };
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| AppError::Other("ratchet state key has invalid length".into()))?;
    let nonce = Nonce::try_from(nonce_bytes)
        .map_err(|_| AppError::Other("ratchet state nonce has invalid length".into()))?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| AppError::Other("ratchet state decryption failed".into()))?,
    );
    Ok(serde_json::from_slice(&plaintext)?)
}

/// Decode a hex-encoded DM pubkey (X25519) to 32 bytes.
pub(super) fn dm_pubkey_to_x25519(hex_pubkey: &str) -> Result<[u8; 32], AppError> {
    let bytes = hex::decode(hex_pubkey)?;
    bytes
        .try_into()
        .map_err(|_| AppError::Other("DM pubkey wrong length".into()))
}

pub(super) fn ratchet_header_to_wire(header: &RatchetHeader) -> RatchetHeaderWire {
    RatchetHeaderWire {
        dh_public: hex::encode(header.dh_public),
        message_number: header.message_number,
        previous_chain_length: header.previous_chain_length,
    }
}

pub(super) fn wire_to_ratchet_header(wire: &RatchetHeaderWire) -> Result<RatchetHeader, AppError> {
    let bytes = hex::decode(&wire.dh_public)?;
    let dh_public: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AppError::Other("invalid dh_public length".into()))?;
    Ok(RatchetHeader {
        dh_public,
        message_number: wire.message_number,
        previous_chain_length: wire.previous_chain_length,
    })
}

impl DmHandler {
    /// Load the stored session with `peer_dm_pubkey`. A row that cannot be decrypted or
    /// parsed (for example one sealed under a previous local DM key) is reported as absent.
    /// The caller holds the session lock of the peer.
    pub(super) async fn load_session(
        &self,
        keys: &DmKeys,
        peer_dm_pubkey: &str,
    ) -> Result<Option<RatchetState>, AppError> {
        let Some(stored) = self.storage.get_ratchet_session(peer_dm_pubkey).await? else {
            return Ok(None);
        };
        match open_session(&keys.ratchet_storage_key, &stored) {
            Ok(ratchet) => Ok(Some(ratchet)),
            Err(e) => {
                log::warn!(
                    "[dm] stored session with {} is unusable: {e}",
                    short_id(peer_dm_pubkey)
                );
                Ok(None)
            }
        }
    }

    /// Seal `ratchet` and store it as the session with `peer_dm_pubkey`.
    /// The caller holds the session lock of the peer.
    pub(super) async fn save_session(
        &self,
        keys: &DmKeys,
        peer_dm_pubkey: &str,
        ratchet: &RatchetState,
    ) -> Result<(), AppError> {
        let sealed = seal_session(&keys.ratchet_storage_key, ratchet)?;
        self.storage
            .save_ratchet_session(peer_dm_pubkey, &sealed, now_millis())
            .await
    }

    /// Encrypt `plaintext` (a serialized `DmPayload`) for `peer` and store the advanced
    /// session. The session lock of the peer is held from loading the session until it
    /// is stored, including a handshake if one runs.
    ///
    /// `refused_ratchet_key` is the `dh_public` of an envelope the peer answered with
    /// `NoSession` or `DecryptFailed`. If the stored session still has that ratchet key,
    /// it is the session the peer could not use and is deleted. A stored session with
    /// another ratchet key was written after that envelope and is kept.
    ///
    /// A handshake runs if no session is stored, if the stored session was deleted as
    /// described, or if it has no sending chain. With `allow_handshake` false an error is
    /// returned instead.
    pub(super) async fn encrypt_for_peer(
        &self,
        keys: &DmKeys,
        peer: &mut PeerRoute,
        plaintext: &[u8],
        refused_ratchet_key: Option<&str>,
        allow_handshake: bool,
    ) -> Result<EncryptedEnvelope, AppError> {
        let _session_guard = self.session_locks.lock(&peer.dm_pubkey).await;

        let mut stored = self.load_session(keys, &peer.dm_pubkey).await?;
        if let (Some(refused), Some(ratchet)) = (refused_ratchet_key, &stored)
            && hex::encode(ratchet.dh_self_public) == refused
        {
            self.storage.delete_ratchet_session(&peer.dm_pubkey).await?;
            stored = None;
        }
        // A session created by `init_bob` cannot encrypt before the first message of the
        // peer arrived. A handshake as initiator replaces it.
        let usable = stored.filter(|ratchet| ratchet.chain_key_send.is_some());

        let mut ratchet = match usable {
            Some(ratchet) => ratchet,
            None if allow_handshake => {
                super::handshake::establish_session(self, keys, peer).await?
            }
            None => {
                return Err(AppError::Other(format!(
                    "no session with {} to encrypt with",
                    short_id(&peer.master_pubkey)
                )));
            }
        };

        let (header, ciphertext) =
            ratchet.encrypt(plaintext, &keys.x25519_public, &peer.x25519_public)?;
        self.save_session(keys, &peer.dm_pubkey, &ratchet).await?;

        log::debug!(
            "[dm] encrypted envelope for {}: msg_n={} prev_chain={} ciphertext_len={}",
            short_id(&peer.master_pubkey),
            header.message_number,
            header.previous_chain_length,
            ciphertext.len(),
        );

        Ok(EncryptedEnvelope {
            sender: keys.dm_pubkey.clone(),
            ratchet_header: ratchet_header_to_wire(&header),
            ciphertext,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_session_opens_only_with_the_same_key() {
        let key = [7u8; 32];
        let bob_public = crate::crypto::x25519_public_from_private(&[9u8; 32]);
        let ratchet = RatchetState::init_alice(&[1u8; 32], &bob_public).unwrap();

        let sealed = seal_session(&key, &ratchet).unwrap();
        let opened = open_session(&key, &sealed).unwrap();
        assert_eq!(opened.root_key, ratchet.root_key);
        assert_eq!(opened.dh_self_public, ratchet.dh_self_public);

        assert!(open_session(&[8u8; 32], &sealed).is_err());
        assert!(open_session(&key, "AAAA").is_err());
        assert!(open_session(&key, "not base64").is_err());
    }

    #[test]
    fn wire_header_with_wrong_key_length_is_rejected() {
        let wire = RatchetHeaderWire {
            dh_public: "ab".repeat(31),
            message_number: 0,
            previous_chain_length: 0,
        };
        assert!(wire_to_ratchet_header(&wire).is_err());
        let wire = RatchetHeaderWire {
            dh_public: "\u{20ac}".repeat(8),
            message_number: 0,
            previous_chain_length: 0,
        };
        assert!(wire_to_ratchet_header(&wire).is_err());
    }
}
