use crate::signing::{
    canonical_bytes, signature_to_hex, verify_profile_signature, verify_signed_by,
};
use crate::types::{Profile, Visibility};
use crate::validation::{is_valid_pubkey, validate_profile};
use iroh::SecretKey;
use serde::{Deserialize, Serialize};

const REGISTRATION_TAG: &str = "proscenium/server-registration/2";
const PROFILE_UPDATE_TAG: &str = "proscenium/server-profile-update/2";

/// `RegistrationRequest::action` value for `DELETE /api/v1/register`.
pub const REGISTRATION_ACTION_UNREGISTER: &str = "unregister";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationPayload {
    /// The user's identity (master public key).
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
    /// The user's identity (master public key).
    pub master_pubkey: String,
    /// The transport NodeId of the registering device.
    pub transport_node_id: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: Visibility,
    pub action: Option<String>,
    /// Signature by the master key of `master_pubkey`.
    pub signature: String,
}

fn registration_signing_bytes(
    master_pubkey: &str,
    transport_node_id: &str,
    server_url: &str,
    timestamp: u64,
    visibility: &Visibility,
    action: &Option<String>,
) -> Vec<u8> {
    canonical_bytes(&(
        REGISTRATION_TAG,
        action,
        master_pubkey,
        server_url,
        timestamp,
        transport_node_id,
        visibility,
    ))
}

/// Sign a registration payload with the master key of `payload.master_pubkey`.
pub fn sign_registration(payload: &RegistrationPayload, master_key: &SecretKey) -> String {
    let bytes = registration_signing_bytes(
        &payload.master_pubkey,
        &payload.transport_node_id,
        &payload.server_url,
        payload.timestamp,
        &payload.visibility,
        &payload.action,
    );
    let sig = master_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a registration request.
///
/// Fails unless `transport_node_id` is a canonical public key and the request signature
/// was made by the key `master_pubkey`.
///
/// Does not check `timestamp` or `server_url`; the server compares those with its clock
/// and configured URL.
pub fn verify_registration_signature(request: &RegistrationRequest) -> Result<(), String> {
    if !is_valid_pubkey(&request.transport_node_id) {
        return Err("invalid transport NodeId".to_string());
    }
    verify_signed_by(
        &request.master_pubkey,
        &registration_signing_bytes(
            &request.master_pubkey,
            &request.transport_node_id,
            &request.server_url,
            request.timestamp,
            &request.visibility,
            &request.action,
        ),
        &request.signature,
        "registration",
    )
}

/// Body of `PUT /api/v1/register`: replaces the profile a server holds for a registered user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileUpdateRequest {
    /// The user's identity (master public key).
    pub master_pubkey: String,
    pub server_url: String,
    pub timestamp: u64,
    /// The profile, signed by the master key of `master_pubkey`.
    pub profile: Profile,
    /// Signature by the master key over
    /// `(master_pubkey, server_url, timestamp, profile.signature)`.
    pub signature: String,
}

fn profile_update_signing_bytes(
    master_pubkey: &str,
    server_url: &str,
    timestamp: u64,
    profile_signature: &str,
) -> Vec<u8> {
    canonical_bytes(&(
        PROFILE_UPDATE_TAG,
        master_pubkey,
        server_url,
        timestamp,
        profile_signature,
    ))
}

/// Sign a profile update for `server_url` with the master key of `master_pubkey`.
/// `profile` must already be signed with `sign_profile`.
pub fn sign_profile_update(
    master_pubkey: &str,
    server_url: &str,
    timestamp: u64,
    profile: &Profile,
    master_key: &SecretKey,
) -> String {
    let bytes =
        profile_update_signing_bytes(master_pubkey, server_url, timestamp, &profile.signature);
    let sig = master_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a profile update request.
///
/// Fails unless: the profile passes `validate_profile` and is signed by the key
/// `master_pubkey` for `master_pubkey`, and the request signature was made by the same key.
///
/// Does not check `timestamp` or `server_url`; the server compares those with its clock
/// and configured URL.
pub fn verify_profile_update(request: &ProfileUpdateRequest) -> Result<(), String> {
    validate_profile(&request.profile)?;
    verify_profile_signature(&request.profile, &request.master_pubkey)?;
    verify_signed_by(
        &request.master_pubkey,
        &profile_update_signing_bytes(
            &request.master_pubkey,
            &request.server_url,
            request.timestamp,
            &request.profile.signature,
        ),
        &request.signature,
        "profile update",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::sign_profile;
    use crate::validation::now_millis;

    fn request(master_pubkey: &str, signer: &SecretKey) -> RegistrationRequest {
        let payload = RegistrationPayload {
            master_pubkey: master_pubkey.to_string(),
            transport_node_id: SecretKey::from_bytes(&[9u8; 32]).public().to_string(),
            server_url: "https://example.test".to_string(),
            timestamp: now_millis(),
            visibility: Visibility::Public,
            action: None,
        };
        let signature = sign_registration(&payload, signer);
        RegistrationRequest {
            master_pubkey: payload.master_pubkey,
            transport_node_id: payload.transport_node_id,
            server_url: payload.server_url,
            timestamp: payload.timestamp,
            visibility: payload.visibility,
            action: payload.action,
            signature,
        }
    }

    #[test]
    fn valid_registration_verifies() {
        let master = SecretKey::from_bytes(&[1u8; 32]);
        let req = request(&master.public().to_string(), &master);
        assert!(verify_registration_signature(&req).is_ok());
    }

    #[test]
    fn registration_signed_by_another_key_is_rejected() {
        // The attacker signs with its own key and names the victim as master.
        let victim = SecretKey::from_bytes(&[1u8; 32]);
        let attacker = SecretKey::from_bytes(&[2u8; 32]);
        let req = request(&victim.public().to_string(), &attacker);
        assert!(verify_registration_signature(&req).is_err());
    }

    #[test]
    fn registration_signature_covers_the_fields() {
        let master = SecretKey::from_bytes(&[1u8; 32]);
        let mut req = request(&master.public().to_string(), &master);
        req.visibility = Visibility::Listed;
        assert!(verify_registration_signature(&req).is_err());
    }

    #[test]
    fn registration_rejects_non_key_node_id() {
        let master = SecretKey::from_bytes(&[1u8; 32]);
        let mut req = request(&master.public().to_string(), &master);
        req.transport_node_id = "aaaaaaa\u{20ac}".to_string();
        assert!(verify_registration_signature(&req).is_err());
    }

    #[test]
    fn profile_update_covers_profile_fields() {
        let master = SecretKey::from_bytes(&[1u8; 32]);
        let master_pubkey = master.public().to_string();
        let mut profile = Profile {
            display_name: "name".to_string(),
            bio: "bio".to_string(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility: Visibility::Public,
            updated_at: 5,
            signature: String::new(),
        };
        sign_profile(&mut profile, &master_pubkey, &master);
        let timestamp = now_millis();
        let url = "https://example.test";
        let signature = sign_profile_update(&master_pubkey, url, timestamp, &profile, &master);
        let mut req = ProfileUpdateRequest {
            master_pubkey,
            server_url: url.to_string(),
            timestamp,
            profile,
            signature,
        };
        assert!(verify_profile_update(&req).is_ok());
        req.profile.bio = "changed".to_string();
        assert!(verify_profile_update(&req).is_err());
    }
}
