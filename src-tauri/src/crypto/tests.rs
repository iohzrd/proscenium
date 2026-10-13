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
    use sha2::{Digest, Sha512};

    let mut ed_secret = [0u8; 32];
    getrandom::fill(&mut ed_secret).unwrap();

    // Derive ed25519 public key using ed25519-dalek-compatible method:
    // The public key is the compressed Edwards Y coordinate of the scalar * basepoint.
    // We use curve25519_dalek directly since we have it as a dependency.
    let hash = Sha512::digest(ed_secret);
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

/// Long-term DM public keys used as associated data in the ratchet tests.
const ALICE_ID: [u8; 32] = [0xa1; 32];
const BOB_ID: [u8; 32] = [0xb0; 32];

/// A pair of ratchet states after a handshake with seed `[42; 32]`: `(alice, bob)`.
fn ratchet_pair() -> (RatchetState, RatchetState) {
    let shared_secret = [42u8; 32];
    let mut bob_x_private = [0u8; 32];
    getrandom::fill(&mut bob_x_private).unwrap();
    let bob_x_public = x25519_public_from_private(&bob_x_private);

    let alice = RatchetState::init_alice(&shared_secret, &bob_x_public).unwrap();
    let bob = RatchetState::init_bob(&shared_secret, (bob_x_private, bob_x_public));
    (alice, bob)
}

/// Alice encrypts for Bob.
fn a2b(alice: &mut RatchetState, plaintext: &[u8]) -> (RatchetHeader, Vec<u8>) {
    alice.encrypt(plaintext, &ALICE_ID, &BOB_ID).unwrap()
}

/// Bob decrypts a message from Alice.
fn b_recv(
    bob: &mut RatchetState,
    header: &RatchetHeader,
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    bob.decrypt(header, ciphertext, &ALICE_ID, &BOB_ID)
}

/// Bob encrypts for Alice.
fn b2a(bob: &mut RatchetState, plaintext: &[u8]) -> (RatchetHeader, Vec<u8>) {
    bob.encrypt(plaintext, &BOB_ID, &ALICE_ID).unwrap()
}

/// Alice decrypts a message from Bob.
fn a_recv(
    alice: &mut RatchetState,
    header: &RatchetHeader,
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    alice.decrypt(header, ciphertext, &BOB_ID, &ALICE_ID)
}

#[test]
fn test_noise_handshake() {
    let mut alice_ed = [0u8; 32];
    let mut bob_ed = [0u8; 32];
    getrandom::fill(&mut alice_ed).unwrap();
    getrandom::fill(&mut bob_ed).unwrap();

    let alice_x = ed25519_secret_to_x25519(&alice_ed);
    let alice_x_pub = x25519_public_from_private(&alice_x);
    let bob_x = ed25519_secret_to_x25519(&bob_ed);
    let bob_x_pub = x25519_public_from_private(&bob_x);

    // Alice initiates with Bob's public key
    let (alice_hs, msg1) = noise_initiate(&alice_x, &bob_x_pub).unwrap();
    assert_eq!(msg1.len(), 96);

    // Bob responds
    let (bob_hs, msg2) = noise_respond(&bob_x, &msg1).unwrap();

    // Both complete and derive the same ratchet seed
    let alice_seed = noise_complete_initiator(alice_hs, &msg2).unwrap();
    let (bob_seed, initiator_key) = noise_complete_responder(bob_hs).unwrap();
    assert_eq!(alice_seed, bob_seed);
    assert_eq!(initiator_key, alice_x_pub);
}

#[test]
fn test_ratchet_seed_differs_from_handshake_hash() {
    let mut alice_x = [0u8; 32];
    let mut bob_x = [0u8; 32];
    getrandom::fill(&mut alice_x).unwrap();
    getrandom::fill(&mut bob_x).unwrap();
    let bob_x_pub = x25519_public_from_private(&bob_x);

    let (alice_hs, msg1) = noise_initiate(&alice_x, &bob_x_pub).unwrap();
    let (bob_hs, msg2) = noise_respond(&bob_x, &msg1).unwrap();
    // After Bob wrote message 2 his handshake is finished and the hash is final.
    let handshake_hash = bob_hs.get_handshake_hash().to_vec();

    let alice_seed = noise_complete_initiator(alice_hs, &msg2).unwrap();
    let (bob_seed, _) = noise_complete_responder(bob_hs).unwrap();
    assert_eq!(alice_seed, bob_seed);
    assert_ne!(alice_seed.as_slice(), handshake_hash.as_slice());
    assert_ne!(*alice_seed, [0u8; 32]);
}

#[test]
fn test_ratchet_seed_requires_finished_handshake() {
    let mut alice_x = [0u8; 32];
    let mut bob_x = [0u8; 32];
    getrandom::fill(&mut alice_x).unwrap();
    getrandom::fill(&mut bob_x).unwrap();
    let bob_x_pub = x25519_public_from_private(&bob_x);

    // A responder state that has not read message 1 is not finished.
    let unfinished = snow::Builder::new("Noise_IK_25519_ChaChaPoly_BLAKE2s".parse().unwrap())
        .local_private_key(&bob_x)
        .unwrap()
        .build_responder()
        .unwrap();
    assert!(noise_complete_responder(unfinished).is_err());

    // A truncated or altered message 2 fails on the initiator side.
    let (alice_hs, msg1) = noise_initiate(&alice_x, &bob_x_pub).unwrap();
    let (_bob_hs, mut msg2) = noise_respond(&bob_x, &msg1).unwrap();
    msg2[40] ^= 1;
    assert!(noise_complete_initiator(alice_hs, &msg2).is_err());
}

#[test]
fn test_ratchet_basic_encrypt_decrypt() {
    let (mut alice, mut bob) = ratchet_pair();

    // Alice sends a message to Bob
    let plaintext = b"Hello Bob!";
    let (header, ciphertext) = a2b(&mut alice, plaintext);
    let decrypted = b_recv(&mut bob, &header, &ciphertext).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_ratchet_multiple_messages_same_direction() {
    let (mut alice, mut bob) = ratchet_pair();

    for i in 0..5 {
        let msg = format!("Message {i}");
        let (header, ciphertext) = a2b(&mut alice, msg.as_bytes());
        let decrypted = b_recv(&mut bob, &header, &ciphertext).unwrap();
        assert_eq!(decrypted, msg.as_bytes());
    }
}

#[test]
fn test_ratchet_alternating_directions() {
    let (mut alice, mut bob) = ratchet_pair();

    // Alice -> Bob
    let (h, c) = a2b(&mut alice, b"Hi Bob");
    assert_eq!(b_recv(&mut bob, &h, &c).unwrap(), b"Hi Bob");

    // Bob -> Alice
    let (h, c) = b2a(&mut bob, b"Hi Alice");
    assert_eq!(a_recv(&mut alice, &h, &c).unwrap(), b"Hi Alice");

    // Alice -> Bob again
    let (h, c) = a2b(&mut alice, b"How are you?");
    assert_eq!(b_recv(&mut bob, &h, &c).unwrap(), b"How are you?");

    // Bob -> Alice again
    let (h, c) = b2a(&mut bob, b"Good!");
    assert_eq!(a_recv(&mut alice, &h, &c).unwrap(), b"Good!");
}

#[test]
fn test_ratchet_out_of_order_messages() {
    let (mut alice, mut bob) = ratchet_pair();

    // Alice sends 3 messages
    let (h1, c1) = a2b(&mut alice, b"msg1");
    let (h2, c2) = a2b(&mut alice, b"msg2");
    let (h3, c3) = a2b(&mut alice, b"msg3");

    // Bob receives them out of order
    assert_eq!(b_recv(&mut bob, &h3, &c3).unwrap(), b"msg3");
    assert_eq!(b_recv(&mut bob, &h1, &c1).unwrap(), b"msg1");
    assert_eq!(b_recv(&mut bob, &h2, &c2).unwrap(), b"msg2");
    assert!(bob.skipped_keys.is_empty());
}

#[test]
fn test_ratchet_serialization() {
    let (mut alice, _bob) = ratchet_pair();

    // Send a message to advance state
    let _ = a2b(&mut alice, b"test");

    // Serialize and deserialize
    let json = serde_json::to_string(&alice).unwrap();
    let restored: RatchetState = serde_json::from_str(&json).unwrap();

    assert_eq!(alice.dh_self_public, restored.dh_self_public);
    assert_eq!(alice.root_key, restored.root_key);
    assert_eq!(alice.send_count, restored.send_count);
}

#[test]
fn test_ratchet_debug_output_has_no_key_material() {
    let (alice, _bob) = ratchet_pair();
    let debug = format!("{alice:?}");
    assert!(debug.contains("send_count"));
    assert!(!debug.contains("root_key"));
    assert!(!debug.contains("dh_self_private"));
}

#[test]
fn test_ratchet_wrong_key_fails() {
    let (mut alice, _bob) = ratchet_pair();

    // Eve has a different shared secret
    let mut eve_private = [0u8; 32];
    getrandom::fill(&mut eve_private).unwrap();
    let eve_public = x25519_public_from_private(&eve_private);
    let mut eve = RatchetState::init_bob(&[99u8; 32], (eve_private, eve_public));

    let (header, ciphertext) = a2b(&mut alice, b"secret message");
    assert_eq!(
        b_recv(&mut eve, &header, &ciphertext),
        Err(CryptoError::DecryptionFailed)
    );
}

#[test]
fn test_ratchet_associated_data_binds_identities() {
    let (mut alice, mut bob) = ratchet_pair();
    let (header, ciphertext) = a2b(&mut alice, b"bound to alice and bob");
    let other = [0xcc; 32];

    // Different sender, different receiver, swapped roles.
    assert_eq!(
        bob.decrypt(&header, &ciphertext, &other, &BOB_ID),
        Err(CryptoError::DecryptionFailed)
    );
    assert_eq!(
        bob.decrypt(&header, &ciphertext, &ALICE_ID, &other),
        Err(CryptoError::DecryptionFailed)
    );
    assert_eq!(
        bob.decrypt(&header, &ciphertext, &BOB_ID, &ALICE_ID),
        Err(CryptoError::DecryptionFailed)
    );

    // The failed attempts did not change Bob's state.
    assert_eq!(
        b_recv(&mut bob, &header, &ciphertext).unwrap(),
        b"bound to alice and bob"
    );
}

#[test]
fn test_ratchet_associated_data_binds_header() {
    let (mut alice, mut bob) = ratchet_pair();
    let (h0, c0) = a2b(&mut alice, b"zero");
    assert_eq!(b_recv(&mut bob, &h0, &c0).unwrap(), b"zero");

    let (header, ciphertext) = a2b(&mut alice, b"one");

    // `previous_chain_length` does not select the message key on an existing chain,
    // so only the associated data detects the change.
    let mut altered = header.clone();
    altered.previous_chain_length += 1;
    assert_eq!(
        b_recv(&mut bob, &altered, &ciphertext),
        Err(CryptoError::DecryptionFailed)
    );

    let mut altered = header.clone();
    altered.message_number += 1;
    assert_eq!(
        b_recv(&mut bob, &altered, &ciphertext),
        Err(CryptoError::DecryptionFailed)
    );

    let mut altered = header.clone();
    altered.dh_public[0] ^= 1;
    assert_eq!(
        b_recv(&mut bob, &altered, &ciphertext),
        Err(CryptoError::DecryptionFailed)
    );

    // No rejected header advanced the chain, stored a skipped key or ran a DH step.
    assert!(bob.skipped_keys.is_empty());
    assert_eq!(bob.recv_count, 1);
    assert_eq!(b_recv(&mut bob, &header, &ciphertext).unwrap(), b"one");
}

#[test]
fn test_ratchet_replayed_message_fails() {
    let (mut alice, mut bob) = ratchet_pair();
    let (header, ciphertext) = a2b(&mut alice, b"once");
    assert_eq!(b_recv(&mut bob, &header, &ciphertext).unwrap(), b"once");
    assert_eq!(
        b_recv(&mut bob, &header, &ciphertext),
        Err(CryptoError::DecryptionFailed)
    );

    // The next message still decrypts.
    let (header, ciphertext) = a2b(&mut alice, b"twice");
    assert_eq!(b_recv(&mut bob, &header, &ciphertext).unwrap(), b"twice");
}

#[test]
fn test_ratchet_encrypt_without_sending_chain_is_an_error() {
    let (mut alice, mut bob) = ratchet_pair();

    // Bob has no sending chain before Alice's first message.
    assert_eq!(
        bob.encrypt(b"too early", &BOB_ID, &ALICE_ID).err(),
        Some(CryptoError::NoSendingChain)
    );
    assert_eq!(bob.send_count, 0);

    let (h, c) = a2b(&mut alice, b"first");
    assert_eq!(b_recv(&mut bob, &h, &c).unwrap(), b"first");
    let (h, c) = b2a(&mut bob, b"now it works");
    assert_eq!(a_recv(&mut alice, &h, &c).unwrap(), b"now it works");
}

#[test]
fn test_ratchet_skip_limit_per_chain() {
    let (mut alice, mut bob) = ratchet_pair();

    // MAX_SKIP + 1 undelivered messages, then one more: the gap exceeds MAX_SKIP.
    for _ in 0..=MAX_SKIP {
        let _ = a2b(&mut alice, b"lost");
    }
    let (header, ciphertext) = a2b(&mut alice, b"too far");
    assert_eq!(header.message_number, MAX_SKIP + 1);
    assert_eq!(
        b_recv(&mut bob, &header, &ciphertext),
        Err(CryptoError::TooManySkipped)
    );
    assert!(bob.skipped_keys.is_empty());

    // A header near u32::MAX is rejected without arithmetic overflow.
    let mut huge = header.clone();
    huge.message_number = u32::MAX;
    huge.previous_chain_length = u32::MAX;
    assert_eq!(
        b_recv(&mut bob, &huge, &ciphertext),
        Err(CryptoError::TooManySkipped)
    );
}

#[test]
fn test_ratchet_counter_overflow_is_an_error() {
    let (mut alice, mut bob) = ratchet_pair();
    let (h, c) = a2b(&mut alice, b"first");
    assert_eq!(b_recv(&mut bob, &h, &c).unwrap(), b"first");

    alice.send_count = u32::MAX;
    assert_eq!(
        alice.encrypt(b"x", &ALICE_ID, &BOB_ID).err(),
        Some(CryptoError::ChainExhausted)
    );

    bob.recv_count = u32::MAX;
    let mut header = h.clone();
    header.message_number = u32::MAX;
    assert_eq!(
        b_recv(&mut bob, &header, &c),
        Err(CryptoError::ChainExhausted)
    );
}

#[test]
fn test_ratchet_skipped_keys_are_capped() {
    let (mut alice, mut bob) = ratchet_pair();
    let per_round = MAX_SKIP as usize;
    let rounds = MAX_SKIPPED_KEYS / per_round + 2;

    let mut first_lost = None;
    let mut last_lost = None;
    for round in 0..rounds {
        // Alice sends MAX_SKIP messages that are not delivered, then one that is.
        for i in 0..MAX_SKIP {
            let lost = a2b(&mut alice, b"lost");
            if round == 0 && i == 0 {
                first_lost = Some(lost);
            } else if round == rounds - 1 && i == 0 {
                last_lost = Some(lost);
            }
        }
        let (h, c) = a2b(&mut alice, b"delivered");
        assert_eq!(b_recv(&mut bob, &h, &c).unwrap(), b"delivered");
        assert!(bob.skipped_keys.len() <= MAX_SKIPPED_KEYS);

        // Bob replies, so Alice's next messages start a new chain.
        let (h, c) = b2a(&mut bob, b"reply");
        assert_eq!(a_recv(&mut alice, &h, &c).unwrap(), b"reply");
    }
    assert_eq!(bob.skipped_keys.len(), MAX_SKIPPED_KEYS);

    // The oldest skipped key was discarded; a recent one is still stored.
    let (h, c) = first_lost.unwrap();
    assert!(b_recv(&mut bob, &h, &c).is_err());
    let (h, c) = last_lost.unwrap();
    assert_eq!(b_recv(&mut bob, &h, &c).unwrap(), b"lost");
    assert_eq!(bob.skipped_keys.len(), MAX_SKIPPED_KEYS - 1);
}

#[test]
fn test_full_noise_then_ratchet() {
    // Full integration: Noise handshake -> Double Ratchet conversation
    let mut alice_ed = [0u8; 32];
    let mut bob_ed = [0u8; 32];
    getrandom::fill(&mut alice_ed).unwrap();
    getrandom::fill(&mut bob_ed).unwrap();

    let alice_x = ed25519_secret_to_x25519(&alice_ed);
    let alice_x_pub = x25519_public_from_private(&alice_x);
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
    let mut alice = RatchetState::init_alice(&shared_secret, &bob_x_pub).unwrap();
    let mut bob = RatchetState::init_bob(&bob_shared, (bob_x, bob_x_pub));

    // Conversation, with the long-term DM public keys as associated data
    let (h, c) = alice
        .encrypt(b"Hello from Alice", &alice_x_pub, &bob_x_pub)
        .unwrap();
    assert_eq!(
        bob.decrypt(&h, &c, &alice_x_pub, &bob_x_pub).unwrap(),
        b"Hello from Alice"
    );

    let (h, c) = bob
        .encrypt(b"Hello from Bob", &bob_x_pub, &alice_x_pub)
        .unwrap();
    assert_eq!(
        alice.decrypt(&h, &c, &bob_x_pub, &alice_x_pub).unwrap(),
        b"Hello from Bob"
    );

    let (h, c) = alice
        .encrypt(b"This is E2E encrypted!", &alice_x_pub, &bob_x_pub)
        .unwrap();
    assert_eq!(
        bob.decrypt(&h, &c, &alice_x_pub, &bob_x_pub).unwrap(),
        b"This is E2E encrypted!"
    );
}
