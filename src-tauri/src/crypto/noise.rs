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
