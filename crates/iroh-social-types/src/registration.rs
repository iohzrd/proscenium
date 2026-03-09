use crate::delegation::UserKeyDelegation;
use crate::signing::{hex_to_signature, signature_to_hex};
use crate::types::Visibility;
use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationPayload {
    /// The user's permanent identity (master public key).
    pub master_pubkey: String,
    /// The transport NodeId of the registering device.
    pub transport_node_id: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: Visibility,
    pub action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationRequest {
    /// The user's permanent identity (master public key).
    pub master_pubkey: String,
    /// The transport NodeId of the registering device.
    pub transport_node_id: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: Visibility,
    pub action: Option<String>,
    /// Signature from the user key (verified via cached delegation).
    pub signature: String,
    /// The user key delegation (so the server can verify the signer).
    pub delegation: UserKeyDelegation,
}

fn registration_signing_bytes(
    master_pubkey: &str,
    transport_node_id: &str,
    server_url: &str,
    timestamp: u64,
    visibility: &Visibility,
    action: &Option<String>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": action,
        "master_pubkey": master_pubkey,
        "server_url": server_url,
        "timestamp": timestamp,
        "transport_node_id": transport_node_id,
        "visibility": visibility,
    }))
    .expect("json serialization should not fail")
}

/// Sign a registration payload with the user key.
pub fn sign_registration(payload: &RegistrationPayload, user_secret_key: &SecretKey) -> String {
    let bytes = registration_signing_bytes(
        &payload.master_pubkey,
        &payload.transport_node_id,
        &payload.server_url,
        payload.timestamp,
        &payload.visibility,
        &payload.action,
    );
    let sig = user_secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a registration request's signature against the user key from the delegation.
pub fn verify_registration_signature(request: &RegistrationRequest) -> Result<(), String> {
    let sig = hex_to_signature(&request.signature)?;
    // The signer is the user key from the delegation
    let user_pubkey: PublicKey = request
        .delegation
        .user_pubkey
        .parse()
        .map_err(|e| format!("invalid user pubkey in delegation: {e}"))?;
    let bytes = registration_signing_bytes(
        &request.master_pubkey,
        &request.transport_node_id,
        &request.server_url,
        request.timestamp,
        &request.visibility,
        &request.action,
    );
    user_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "registration signature verification failed".to_string())
}
