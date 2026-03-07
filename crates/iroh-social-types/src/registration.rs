use crate::signing::{hex_to_signature, signature_to_hex};
use crate::types::Visibility;
use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationPayload {
    pub pubkey: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: Visibility,
    pub action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationRequest {
    pub pubkey: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: Visibility,
    pub action: Option<String>,
    pub signature: String,
}

fn registration_signing_bytes(
    pubkey: &str,
    server_url: &str,
    timestamp: u64,
    visibility: &Visibility,
    action: &Option<String>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": action,
        "pubkey": pubkey,
        "server_url": server_url,
        "timestamp": timestamp,
        "visibility": visibility,
    }))
    .expect("json serialization should not fail")
}

pub fn sign_registration(payload: &RegistrationPayload, secret_key: &SecretKey) -> String {
    let bytes = registration_signing_bytes(
        &payload.pubkey,
        &payload.server_url,
        payload.timestamp,
        &payload.visibility,
        &payload.action,
    );
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

pub fn verify_registration_signature(request: &RegistrationRequest) -> Result<(), String> {
    let sig = hex_to_signature(&request.signature)?;
    let pubkey: PublicKey = request
        .pubkey
        .parse()
        .map_err(|e| format!("invalid pubkey: {e}"))?;
    let bytes = registration_signing_bytes(
        &request.pubkey,
        &request.server_url,
        request.timestamp,
        &request.visibility,
        &request.action,
    );
    pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "registration signature verification failed".to_string())
}
