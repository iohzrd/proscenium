use crate::delegation::verify_delegation;
use crate::protocol::LinkedDevicesAnnouncement;
use crate::types::{Interaction, Post, Profile};
use iroh::{PublicKey, SecretKey, Signature};

/// Produce the canonical bytes for signing a Post.
/// Fields are serialized in a deterministic order, excluding `signature`.
fn post_signing_bytes(post: &Post) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": post.id,
        "author": post.author,
        "content": post.content,
        "timestamp": post.timestamp,
        "media": post.media,
        "reply_to": post.reply_to,
        "reply_to_author": post.reply_to_author,
        "quote_of": post.quote_of,
        "quote_of_author": post.quote_of_author,
    }))
    .expect("json serialization should not fail")
}

/// Produce the canonical bytes for signing an Interaction.
/// Fields are serialized in a deterministic order, excluding `signature`.
fn interaction_signing_bytes(interaction: &Interaction) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": interaction.id,
        "author": interaction.author,
        "kind": interaction.kind,
        "target_post_id": interaction.target_post_id,
        "target_author": interaction.target_author,
        "timestamp": interaction.timestamp,
    }))
    .expect("json serialization should not fail")
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

/// Sign a Post in place using the given secret key.
pub fn sign_post(post: &mut Post, secret_key: &SecretKey) {
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

/// Produce the canonical bytes for signing a Profile.
/// Excludes `signature` to avoid circular dependency.
fn profile_signing_bytes(profile: &Profile) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "display_name": profile.display_name,
        "bio": profile.bio,
        "avatar_hash": profile.avatar_hash,
        "avatar_ticket": profile.avatar_ticket,
        "visibility": profile.visibility,
    }))
    .expect("json serialization should not fail")
}

/// Sign a Profile in place using the given secret key.
pub fn sign_profile(profile: &mut Profile, secret_key: &SecretKey) {
    let bytes = profile_signing_bytes(profile);
    let sig = secret_key.sign(&bytes);
    profile.signature = signature_to_hex(&sig);
}

/// Verify a Profile's signature against the given signer public key.
pub fn verify_profile_signature(
    profile: &Profile,
    signer_pubkey: &PublicKey,
) -> Result<(), String> {
    let sig = hex_to_signature(&profile.signature)?;
    let bytes = profile_signing_bytes(profile);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "profile signature verification failed".to_string())
}

/// Produce the canonical bytes for signing a delete-post action.
fn delete_post_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "delete_post",
        "id": id,
        "author": author,
    }))
    .expect("json serialization should not fail")
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

/// Produce the canonical bytes for signing a delete-interaction action.
fn delete_interaction_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "delete_interaction",
        "id": id,
        "author": author,
    }))
    .expect("json serialization should not fail")
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

/// Produce the canonical bytes for signing a LinkedDevicesAnnouncement.
/// Excludes `signature` to avoid circular dependency.
fn linked_devices_signing_bytes(announcement: &LinkedDevicesAnnouncement) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "master_pubkey": announcement.master_pubkey,
        "devices": announcement.devices,
        "version": announcement.version,
        "timestamp": announcement.timestamp,
    }))
    .expect("json serialization should not fail")
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
/// 2. Verifies the announcement signature against the signing key from the delegation.
/// 3. Checks that the delegation's master_pubkey matches the announcement's master_pubkey.
pub fn verify_linked_devices_announcement(
    announcement: &LinkedDevicesAnnouncement,
) -> Result<(), String> {
    // Verify the delegation chain
    verify_delegation(&announcement.delegation)?;

    // Check master pubkey consistency
    if announcement.delegation.master_pubkey != announcement.master_pubkey {
        return Err("announcement master_pubkey does not match delegation".to_string());
    }

    // Verify announcement signature against signing key
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
