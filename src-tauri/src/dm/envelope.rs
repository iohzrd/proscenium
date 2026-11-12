//! Encryption of a DM payload to one device.
//!
//! HPKE (RFC 9180) in Auth mode with DHKEM(X25519, HKDF-SHA256), HKDF-SHA256 and
//! ChaCha20-Poly1305. The sender and recipient keys are the X25519 forms of the two
//! devices' Ed25519 transport keys: the private scalar of the seed and the Montgomery form
//! of the NodeId.

use crate::error::AppError;
use curve25519_dalek::edwards::CompressedEdwardsY;
use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable, Kem as KemTrait, OpModeR, OpModeS, Serializable};
use iroh::{PublicKey, SecretKey};
use proscenium_types::{DmEnvelope, LinkedDevicesAnnouncement, parse_pubkey};
use sha2::{Digest, Sha512};
use zeroize::{Zeroize, Zeroizing};

type Kem = X25519HkdfSha256;
type PrivateKey = <Kem as KemTrait>::PrivateKey;
type Public = <Kem as KemTrait>::PublicKey;

/// HPKE `info` of every DM envelope.
const DM_INFO: &[u8] = b"proscenium/dm/2";
/// Domain tag of the envelope AAD.
const DM_AAD_TAG: &str = "proscenium/dm-envelope/2";

/// X25519 private key of the device with transport key `transport_key`: the first 32
/// bytes of SHA-512 of the Ed25519 seed (the Ed25519 signing scalar), clamped by hpke.
fn x25519_private(transport_key: &SecretKey) -> Result<PrivateKey, AppError> {
    let mut seed = transport_key.to_bytes();
    let mut hash = Sha512::digest(seed);
    seed.zeroize();
    let key = PrivateKey::from_bytes(&hash[..32]);
    hash.as_mut_slice().zeroize();
    key.map_err(|e| AppError::Other(format!("invalid X25519 private key: {e}")))
}

/// X25519 public key of the device with NodeId `node_id`: the Montgomery form of the
/// Ed25519 point.
fn x25519_public(node_id: &PublicKey) -> Result<Public, AppError> {
    let point = CompressedEdwardsY(*node_id.as_bytes())
        .decompress()
        .ok_or_else(|| AppError::Other("NodeId is not an Ed25519 point".into()))?;
    Public::from_bytes(&point.to_montgomery().to_bytes())
        .map_err(|e| AppError::Other(format!("invalid X25519 public key: {e}")))
}

/// AAD of an envelope: the sender and recipient identities and devices. Binding the
/// master pubkeys prevents an envelope from being presented under another identity that
/// lists the same device.
fn aad(sender: &str, sender_device: &str, recipient: &str, recipient_device: &str) -> Vec<u8> {
    serde_json::to_vec(&(
        DM_AAD_TAG,
        sender,
        sender_device,
        recipient,
        recipient_device,
    ))
    .expect("serializing a tuple of strings cannot fail")
}

/// Encrypt `plaintext` (a serialized `DmPayload`) from the device with transport key
/// `sender_key` of the user `sender` to the device `recipient_device` of `recipient`.
pub(super) fn seal(
    sender_key: &SecretKey,
    sender: &str,
    sender_devices: &LinkedDevicesAnnouncement,
    recipient: &str,
    recipient_device: &PublicKey,
    plaintext: &[u8],
) -> Result<DmEnvelope, AppError> {
    let sender_private = x25519_private(sender_key)?;
    let sender_public = Kem::sk_to_pk(&sender_private);
    let recipient_public = x25519_public(recipient_device)?;
    let sender_device = sender_key.public().to_string();
    let recipient_device = recipient_device.to_string();
    let aad = aad(sender, &sender_device, recipient, &recipient_device);
    let (encapped_key, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeS::Auth((sender_private, sender_public)),
        &recipient_public,
        DM_INFO,
        plaintext,
        &aad,
    )
    .map_err(|e| AppError::Other(format!("DM encryption failed: {e}")))?;
    Ok(DmEnvelope {
        sender: sender.to_string(),
        sender_device,
        recipient: recipient.to_string(),
        recipient_device,
        sender_devices: sender_devices.clone(),
        encapped_key: encapped_key.to_bytes().to_vec(),
        ciphertext,
    })
}

/// Decrypt `envelope` with the transport key `recipient_key` of the receiving device.
/// Fails unless the payload was encrypted by the key of `envelope.sender_device` for this
/// device, with the AAD of the envelope's sender and recipient fields.
pub(super) fn open(
    recipient_key: &SecretKey,
    envelope: &DmEnvelope,
) -> Result<Zeroizing<Vec<u8>>, AppError> {
    let sender_device = parse_pubkey(&envelope.sender_device).map_err(AppError::Other)?;
    let sender_public = x25519_public(&sender_device)?;
    let recipient_private = x25519_private(recipient_key)?;
    let encapped_key = <Kem as KemTrait>::EncappedKey::from_bytes(&envelope.encapped_key)
        .map_err(|e| AppError::Other(format!("invalid encapsulated key: {e}")))?;
    let aad = aad(
        &envelope.sender,
        &envelope.sender_device,
        &envelope.recipient,
        &envelope.recipient_device,
    );
    hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeR::Auth(sender_public),
        &recipient_private,
        &encapped_key,
        DM_INFO,
        &envelope.ciphertext,
        &aad,
    )
    .map(Zeroizing::new)
    .map_err(|e| AppError::Other(format!("DM decryption failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(master: &str) -> LinkedDevicesAnnouncement {
        LinkedDevicesAnnouncement {
            master_pubkey: master.to_string(),
            devices: vec![],
            version: 1,
            signature: String::new(),
        }
    }

    struct Fixture {
        alice_device: SecretKey,
        bob_device: SecretKey,
        alice: String,
        bob: String,
    }

    fn fixture() -> Fixture {
        Fixture {
            alice_device: SecretKey::from_bytes(&[1u8; 32]),
            bob_device: SecretKey::from_bytes(&[2u8; 32]),
            alice: SecretKey::from_bytes(&[11u8; 32]).public().to_string(),
            bob: SecretKey::from_bytes(&[12u8; 32]).public().to_string(),
        }
    }

    fn sealed(f: &Fixture, plaintext: &[u8]) -> DmEnvelope {
        seal(
            &f.alice_device,
            &f.alice,
            &devices(&f.alice),
            &f.bob,
            &f.bob_device.public(),
            plaintext,
        )
        .unwrap()
    }

    #[test]
    fn envelope_opens_only_on_the_recipient_device() {
        let f = fixture();
        let envelope = sealed(&f, b"hello");
        assert_eq!(envelope.sender_device, f.alice_device.public().to_string());
        assert_eq!(envelope.recipient_device, f.bob_device.public().to_string());
        assert_eq!(open(&f.bob_device, &envelope).unwrap().as_slice(), b"hello");

        // Another device, even of the same user, cannot open it.
        let other = SecretKey::from_bytes(&[3u8; 32]);
        assert!(open(&other, &envelope).is_err());
        assert!(open(&f.alice_device, &envelope).is_err());
    }

    #[test]
    fn envelope_fields_are_authenticated() {
        let f = fixture();
        let envelope = sealed(&f, b"hello");

        // A sender device that did not encrypt it.
        let mut wrong_sender = envelope.clone();
        wrong_sender.sender_device = SecretKey::from_bytes(&[4u8; 32]).public().to_string();
        assert!(open(&f.bob_device, &wrong_sender).is_err());

        // The same device presented under another identity.
        let mut relabeled = envelope.clone();
        relabeled.sender = SecretKey::from_bytes(&[13u8; 32]).public().to_string();
        assert!(open(&f.bob_device, &relabeled).is_err());

        let mut other_recipient = envelope.clone();
        other_recipient.recipient = f.alice.clone();
        assert!(open(&f.bob_device, &other_recipient).is_err());

        let mut tampered = envelope.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(open(&f.bob_device, &tampered).is_err());

        let mut short_key = envelope;
        short_key.encapped_key.pop();
        assert!(open(&f.bob_device, &short_key).is_err());
    }

    #[test]
    fn each_envelope_uses_a_new_encapsulated_key() {
        let f = fixture();
        let a = sealed(&f, b"same");
        let b = sealed(&f, b"same");
        assert_ne!(a.encapped_key, b.encapped_key);
        assert_ne!(a.ciphertext, b.ciphertext);
    }
}
