//! Keys derived from the master key.
//!
//! The master key (Ed25519) is the user's identity and signs all of the user's content.
//! Every device of the user holds it.

use hkdf::Hkdf;
use sha2::Sha256;

/// Derive the transport key (iroh endpoint secret key) of the device with derivation
/// index `device_index`. Each device uses its own index, so the devices of one user have
/// distinct NodeIds. The first device uses index 0. The X25519 form of the transport key
/// decrypts the DMs sent to the device.
pub fn derive_transport_key(master_secret: &[u8; 32], device_index: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&device_index.to_be_bytes()), master_secret);
    let mut transport_secret = [0u8; 32];
    hk.expand(b"proscenium/transport-key", &mut transport_secret)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    transport_secret
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_keys_are_deterministic_and_differ_per_device() {
        let master = [42u8; 32];
        assert_eq!(
            derive_transport_key(&master, 0),
            derive_transport_key(&master, 0)
        );
        assert_ne!(
            derive_transport_key(&master, 0),
            derive_transport_key(&master, 1)
        );
        assert_ne!(
            derive_transport_key(&master, 0),
            derive_transport_key(&[43u8; 32], 0)
        );
        assert_ne!(derive_transport_key(&master, 0), master);
    }
}
