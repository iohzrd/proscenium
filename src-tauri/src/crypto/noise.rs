use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const NOISE_PSK_PATTERN: &str = "Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s";

/// HKDF info string for the Double Ratchet seed derived from a DM handshake.
const RATCHET_SEED_INFO: &[u8] = b"proscenium-dm-root-v2";

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
/// Returns the seed for the Double Ratchet root key (`derive_ratchet_seed`).
pub fn noise_complete_initiator(
    mut initiator: snow::HandshakeState,
    response: &[u8],
) -> Result<Zeroizing<[u8; 32]>, snow::Error> {
    let mut payload = vec![0u8; 65535];
    initiator.read_message(response, &mut payload)?;
    derive_ratchet_seed(&mut initiator)
}

/// Complete the handshake on the responder side.
/// Returns `(ratchet_seed, initiator_dm_pubkey)`: the seed for the Double Ratchet root key
/// (`derive_ratchet_seed`) and the initiator's long-term X25519 key, which the handshake
/// authenticated.
pub fn noise_complete_responder(
    mut responder: snow::HandshakeState,
) -> Result<(Zeroizing<[u8; 32]>, [u8; 32]), snow::Error> {
    let initiator_dm_pubkey: [u8; 32] = responder
        .get_remote_static()
        .and_then(|s| s.try_into().ok())
        .ok_or(snow::Error::Prereq(
            snow::error::Prerequisite::RemotePublicKey,
        ))?;
    let seed = derive_ratchet_seed(&mut responder)?;
    Ok((seed, initiator_dm_pubkey))
}

/// Derive the Double Ratchet seed from a finished handshake:
/// `HKDF-SHA256(ikm = k1 || k2, salt = handshake hash, info = RATCHET_SEED_INFO)`.
///
/// `k1` and `k2` are the two transport cipher keys of the Noise `Split()` step. They are
/// derived from the final chaining key, which mixes the four DH outputs of the IK pattern
/// (es, ss, ee, se), so they cannot be computed from the handshake messages. The handshake
/// hash covers only transmitted bytes and is used as a public salt.
///
/// Returns `HandshakeNotFinished` before the last handshake message was processed, because
/// the chaining key does not include every DH output until then.
fn derive_ratchet_seed(hs: &mut snow::HandshakeState) -> Result<Zeroizing<[u8; 32]>, snow::Error> {
    if !hs.is_handshake_finished() {
        return Err(snow::Error::State(
            snow::error::StateProblem::HandshakeNotFinished,
        ));
    }
    let (k1, k2) = hs.dangerously_get_raw_split();
    let (k1, k2) = (Zeroizing::new(k1), Zeroizing::new(k2));
    let mut ikm = Zeroizing::new([0u8; 64]);
    let (first, second) = ikm.split_at_mut(32);
    first.copy_from_slice(k1.as_slice());
    second.copy_from_slice(k2.as_slice());

    let hk = Hkdf::<Sha256>::new(Some(hs.get_handshake_hash()), ikm.as_slice());
    let mut seed = Zeroizing::new([0u8; 32]);
    hk.expand(RATCHET_SEED_INFO, seed.as_mut_slice())
        .map_err(|_| snow::Error::Input)?;
    Ok(seed)
}

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
