use crate::delegation::{SigningKeyDelegation, verify_delegation};
use crate::protocol::{
    IdentityResponse, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN, MAX_LINKED_DEVICES,
};
use crate::types::{Interaction, Post, Profile};
use crate::validation::{ID_HEX_LEN, is_valid_pubkey, validate_profile};
use iroh::{PublicKey, SecretKey, Signature};
use serde::Serialize;
use sha2::{Digest, Sha256};

// Domain tags. Each signed byte string is a JSON array whose first element is one of these,
// so a signature made for one message type never verifies as another.
const POST_ID_TAG: &str = "proscenium/post-id/2";
const POST_TAG: &str = "proscenium/post/2";
const INTERACTION_TAG: &str = "proscenium/interaction/2";
const PROFILE_TAG: &str = "proscenium/profile/2";
const DELETE_POST_TAG: &str = "proscenium/delete-post/2";
const DELETE_INTERACTION_TAG: &str = "proscenium/delete-interaction/2";
const LINKED_DEVICES_TAG: &str = "proscenium/linked-devices/2";

/// Serialize a tuple of fields as a JSON array.
/// Element order is the tuple order, independent of any serde_json feature.
pub(crate) fn canonical_bytes<T: Serialize>(fields: &T) -> Vec<u8> {
    serde_json::to_vec(fields).expect("serializing a signing tuple cannot fail")
}

/// Derive a post id from every post field except `id` and `signature`.
/// The author is part of the hash input, so two authors cannot produce the same id.
pub fn compute_post_id(post: &Post) -> String {
    let bytes = canonical_bytes(&(
        POST_ID_TAG,
        &post.author,
        &post.content,
        post.timestamp,
        &post.media,
        &post.reply_to,
        &post.reply_to_author,
        &post.quote_of,
        &post.quote_of_author,
    ));
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..ID_HEX_LEN / 2])
}

/// Canonical bytes for signing a Post (every field except `signature`).
fn post_signing_bytes(post: &Post) -> Vec<u8> {
    canonical_bytes(&(
        POST_TAG,
        &post.id,
        &post.author,
        &post.content,
        post.timestamp,
        &post.media,
        &post.reply_to,
        &post.reply_to_author,
        &post.quote_of,
        &post.quote_of_author,
    ))
}

/// Canonical bytes for signing an Interaction (every field except `signature`).
fn interaction_signing_bytes(interaction: &Interaction) -> Vec<u8> {
    canonical_bytes(&(
        INTERACTION_TAG,
        &interaction.id,
        &interaction.author,
        interaction.kind,
        &interaction.target_post_id,
        &interaction.target_author,
        interaction.timestamp,
    ))
}

pub fn signature_to_hex(sig: &Signature) -> String {
    hex::encode(sig.to_bytes())
}

pub fn hex_to_signature(hex_str: &str) -> Result<Signature, String> {
    if hex_str.len() != 128 {
        return Err(format!("invalid signature hex length: {}", hex_str.len()));
    }
    let bytes = hex::decode(hex_str).map_err(|e| format!("invalid hex: {e}"))?;
    let arr: [u8; 64] = bytes
        .try_into()
        .map_err(|_| "invalid signature length".to_string())?;
    Ok(Signature::from_bytes(&arr))
}

/// Set `post.id` from the post content, then sign the post in place.
pub fn sign_post(post: &mut Post, secret_key: &SecretKey) {
    post.id = compute_post_id(post);
    let bytes = post_signing_bytes(post);
    let sig = secret_key.sign(&bytes);
    post.signature = signature_to_hex(&sig);
}

/// Sign an Interaction in place using the given secret key.
pub fn sign_interaction(interaction: &mut Interaction, secret_key: &SecretKey) {
    let bytes = interaction_signing_bytes(interaction);
    let sig = secret_key.sign(&bytes);
    interaction.signature = signature_to_hex(&sig);
}

/// Verify a Post's signature against the given signer public key.
/// The signer is the signing key (from a cached SigningKeyDelegation), NOT post.author
/// (which is the master pubkey / permanent identity).
pub fn verify_post_signature(post: &Post, signer_pubkey: &PublicKey) -> Result<(), String> {
    let sig = hex_to_signature(&post.signature)?;
    let bytes = post_signing_bytes(post);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "post signature verification failed".to_string())
}

/// Verify an Interaction's signature against the given signer public key.
/// The signer is the signing key (from a cached SigningKeyDelegation), NOT interaction.author
/// (which is the master pubkey / permanent identity).
pub fn verify_interaction_signature(
    interaction: &Interaction,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(&interaction.signature)?;
    let bytes = interaction_signing_bytes(interaction);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "interaction signature verification failed".to_string())
}

/// Canonical bytes for signing a Profile.
/// `author` is the owner's master pubkey; it is signed so a profile cannot be
/// stored under another identity. `updated_at` is signed so receivers can order profiles.
fn profile_signing_bytes(profile: &Profile, author: &str) -> Vec<u8> {
    canonical_bytes(&(
        PROFILE_TAG,
        author,
        &profile.display_name,
        &profile.bio,
        &profile.avatar_hash,
        &profile.avatar_ticket,
        profile.visibility,
        profile.updated_at,
    ))
}

/// Sign a Profile in place. `author` is the owner's master pubkey.
pub fn sign_profile(profile: &mut Profile, author: &str, secret_key: &SecretKey) {
    let bytes = profile_signing_bytes(profile, author);
    let sig = secret_key.sign(&bytes);
    profile.signature = signature_to_hex(&sig);
}

/// Verify a Profile's signature. `author` is the master pubkey the profile is stored under.
pub fn verify_profile_signature(
    profile: &Profile,
    author: &str,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(&profile.signature)?;
    let bytes = profile_signing_bytes(profile, author);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "profile signature verification failed".to_string())
}

/// Canonical bytes for signing a delete-post action.
fn delete_post_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    canonical_bytes(&(DELETE_POST_TAG, id, author))
}

/// Sign a delete-post action. Returns the hex-encoded signature.
pub fn sign_delete_post(id: &str, author: &str, secret_key: &SecretKey) -> String {
    let bytes = delete_post_signing_bytes(id, author);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a delete-post signature against the given signer public key.
pub fn verify_delete_post_signature(
    id: &str,
    author: &str,
    signature: &str,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(signature)?;
    let bytes = delete_post_signing_bytes(id, author);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "delete post signature verification failed".to_string())
}

/// Canonical bytes for signing a delete-interaction action.
fn delete_interaction_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    canonical_bytes(&(DELETE_INTERACTION_TAG, id, author))
}

/// Sign a delete-interaction action. Returns the hex-encoded signature.
pub fn sign_delete_interaction(id: &str, author: &str, secret_key: &SecretKey) -> String {
    let bytes = delete_interaction_signing_bytes(id, author);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a delete-interaction signature against the given signer public key.
pub fn verify_delete_interaction_signature(
    id: &str,
    author: &str,
    signature: &str,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(signature)?;
    let bytes = delete_interaction_signing_bytes(id, author);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "delete interaction signature verification failed".to_string())
}

/// Canonical bytes for signing a LinkedDevicesAnnouncement (every field except `signature`
/// and `delegation`; the delegation carries its own master-key signature).
fn linked_devices_signing_bytes(announcement: &LinkedDevicesAnnouncement) -> Vec<u8> {
    canonical_bytes(&(
        LINKED_DEVICES_TAG,
        &announcement.master_pubkey,
        &announcement.devices,
        announcement.version,
        announcement.timestamp,
    ))
}

/// Sign a LinkedDevicesAnnouncement in place using the given signing key.
pub fn sign_linked_devices_announcement(
    announcement: &mut LinkedDevicesAnnouncement,
    secret_key: &SecretKey,
) {
    let bytes = linked_devices_signing_bytes(announcement);
    let sig = secret_key.sign(&bytes);
    announcement.signature = signature_to_hex(&sig);
}

/// Verify a LinkedDevicesAnnouncement.
/// 1. Verifies the delegation (master key signed the signing key binding).
/// 2. Checks that the delegation's master_pubkey equals the announcement's master_pubkey.
/// 3. Checks the device list bounds and that every NodeId is a canonical public key.
/// 4. Verifies the announcement signature against the signing key from the delegation.
///
/// Does not compare the delegation or version with previously stored values;
/// the caller does that when it stores the announcement.
pub fn verify_linked_devices_announcement(
    announcement: &LinkedDevicesAnnouncement,
) -> Result<(), String> {
    verify_delegation(&announcement.delegation)?;

    if announcement.delegation.master_pubkey != announcement.master_pubkey {
        return Err("announcement master_pubkey does not match delegation".to_string());
    }

    if announcement.devices.is_empty() || announcement.devices.len() > MAX_LINKED_DEVICES {
        return Err(format!(
            "announcement lists {} devices (allowed 1..={MAX_LINKED_DEVICES})",
            announcement.devices.len()
        ));
    }
    for device in &announcement.devices {
        if !is_valid_pubkey(&device.node_id) {
            return Err("announcement contains an invalid device NodeId".to_string());
        }
        if device.device_name.len() > MAX_DEVICE_NAME_LEN {
            return Err("announcement contains a device name that is too long".to_string());
        }
    }

    let signer: PublicKey = announcement
        .delegation
        .signing_pubkey
        .parse()
        .map_err(|e| format!("invalid signing pubkey in delegation: {e}"))?;
    let sig = hex_to_signature(&announcement.signature)?;
    let bytes = linked_devices_signing_bytes(announcement);
    signer
        .verify(&bytes, &sig)
        .map_err(|_| "linked devices announcement signature verification failed".to_string())
}

/// An `IdentityResponse` whose delegation, device list and profile signatures were verified,
/// and whose signed device list contains the NodeId that sent it.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    pub master_pubkey: String,
    pub announcement: LinkedDevicesAnnouncement,
    pub profile: Option<Profile>,
}

impl VerifiedIdentity {
    pub fn delegation(&self) -> &SigningKeyDelegation {
        &self.announcement.delegation
    }

    pub fn transport_node_ids(&self) -> Vec<String> {
        self.announcement
            .devices
            .iter()
            .map(|d| d.node_id.clone())
            .collect()
    }
}

/// Verify an `IdentityResponse` received from `remote_node_id` (the QUIC peer's NodeId).
///
/// Fails unless: the delegation is signed by the master key, the device list is signed by
/// the delegated signing key, `remote_node_id` is in that device list, and the profile
/// (if present) passes `validate_profile` and is signed for this master pubkey.
pub fn verify_identity_response(
    response: IdentityResponse,
    remote_node_id: &str,
) -> Result<VerifiedIdentity, String> {
    verify_linked_devices_announcement(&response.announcement)?;

    if !response
        .announcement
        .devices
        .iter()
        .any(|d| d.node_id == remote_node_id)
    {
        return Err("responding NodeId is not in the signed device list".to_string());
    }

    let master_pubkey = response.announcement.master_pubkey.clone();

    if let Some(profile) = &response.profile {
        validate_profile(profile)?;
        let signer: PublicKey = response
            .announcement
            .delegation
            .signing_pubkey
            .parse()
            .map_err(|e| format!("invalid signing pubkey in delegation: {e}"))?;
        verify_profile_signature(profile, &master_pubkey, &signer)?;
    }

    Ok(VerifiedIdentity {
        master_pubkey,
        announcement: response.announcement,
        profile: response.profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::{derive_signing_key, sign_delegation};
    use crate::types::{DeviceEntry, Visibility};
    use crate::validation::{now_millis, validate_post};

    struct TestIdentity {
        master_pubkey: String,
        signing: SecretKey,
        delegation: SigningKeyDelegation,
    }

    fn identity(seed: u8) -> TestIdentity {
        let master_bytes = [seed; 32];
        let master = SecretKey::from_bytes(&master_bytes);
        let signing = SecretKey::from_bytes(&derive_signing_key(&master_bytes, 0));
        let delegation = sign_delegation(
            &master,
            &signing.public(),
            0,
            &"ab".repeat(32),
            0,
            now_millis(),
        );
        TestIdentity {
            master_pubkey: master.public().to_string(),
            signing,
            delegation,
        }
    }

    fn node_id(seed: u8) -> String {
        SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    fn announcement(id: &TestIdentity, nodes: &[String]) -> LinkedDevicesAnnouncement {
        let mut a = LinkedDevicesAnnouncement {
            master_pubkey: id.master_pubkey.clone(),
            delegation: id.delegation.clone(),
            devices: nodes
                .iter()
                .map(|n| DeviceEntry {
                    node_id: n.clone(),
                    device_name: "d".to_string(),
                    is_primary: true,
                    added_at: 1,
                })
                .collect(),
            version: 1,
            timestamp: 1,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut a, &id.signing);
        a
    }

    fn post(author: &str) -> Post {
        Post {
            id: String::new(),
            author: author.to_string(),
            content: "hello".to_string(),
            timestamp: now_millis(),
            media: vec![],
            reply_to: None,
            reply_to_author: None,
            quote_of: None,
            quote_of_author: None,
            signature: String::new(),
        }
    }

    #[test]
    fn post_id_is_derived_and_author_bound() {
        let a = identity(1);
        let b = identity(2);
        let mut p = post(&a.master_pubkey);
        sign_post(&mut p, &a.signing);
        assert!(validate_post(&p).is_ok());
        assert!(verify_post_signature(&p, &a.signing.public()).is_ok());

        // Same content under another author yields another id.
        let mut q = post(&b.master_pubkey);
        q.timestamp = p.timestamp;
        assert_ne!(compute_post_id(&q), p.id);

        // Reusing the id under another author fails validation.
        q.id = p.id.clone();
        assert!(validate_post(&q).is_err());
    }

    #[test]
    fn profile_signature_binds_author_and_version() {
        let a = identity(1);
        let b = identity(2);
        let mut profile = Profile {
            display_name: "n".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Public,
            updated_at: 10,
            signature: String::new(),
        };
        sign_profile(&mut profile, &a.master_pubkey, &a.signing);
        let signer = a.signing.public();
        assert!(verify_profile_signature(&profile, &a.master_pubkey, &signer).is_ok());
        assert!(verify_profile_signature(&profile, &b.master_pubkey, &signer).is_err());
        profile.updated_at = 11;
        assert!(verify_profile_signature(&profile, &a.master_pubkey, &signer).is_err());
    }

    #[test]
    fn identity_response_requires_listed_node_and_valid_chain() {
        let a = identity(1);
        let listed = node_id(50);
        let other = node_id(51);
        let response = IdentityResponse {
            announcement: announcement(&a, std::slice::from_ref(&listed)),
            profile: None,
        };
        assert!(verify_identity_response(response.clone(), &listed).is_ok());
        assert!(verify_identity_response(response, &other).is_err());

        // A delegation whose signature was not made by the claimed master key is rejected.
        let b = identity(2);
        let mut forged = announcement(&b, std::slice::from_ref(&other));
        forged.master_pubkey = a.master_pubkey.clone();
        forged.delegation.master_pubkey = a.master_pubkey.clone();
        sign_linked_devices_announcement(&mut forged, &b.signing);
        let response = IdentityResponse {
            announcement: forged,
            profile: None,
        };
        assert!(verify_identity_response(response, &other).is_err());
    }

    #[test]
    fn identity_response_rejects_profile_signed_for_other_identity() {
        let a = identity(1);
        let b = identity(2);
        let listed = node_id(50);
        let mut profile = Profile {
            display_name: "n".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Private,
            updated_at: 1,
            signature: String::new(),
        };
        sign_profile(&mut profile, &b.master_pubkey, &b.signing);
        let response = IdentityResponse {
            announcement: announcement(&a, std::slice::from_ref(&listed)),
            profile: Some(profile),
        };
        assert!(verify_identity_response(response, &listed).is_err());
    }
}
