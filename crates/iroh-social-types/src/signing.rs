use crate::types::{Interaction, Post};
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
/// The signer is the user key (from a cached UserKeyDelegation), NOT post.author
/// (which is the master pubkey / permanent identity).
pub fn verify_post_signature(post: &Post, signer_pubkey: &PublicKey) -> Result<(), String> {
    let sig = hex_to_signature(&post.signature)?;
    let bytes = post_signing_bytes(post);
    signer_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "post signature verification failed".to_string())
}

/// Verify an Interaction's signature against the given signer public key.
/// The signer is the user key (from a cached UserKeyDelegation), NOT interaction.author
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
