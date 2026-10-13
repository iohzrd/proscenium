use crate::delegation::{SigningKeyDelegation, verify_delegation};
use crate::signing::{
    canonical_bytes, hex_to_signature, signature_to_hex, verify_profile_signature,
};
use crate::types::{Profile, Visibility};
use crate::validation::{is_valid_pubkey, validate_profile};
use iroh::{PublicKey, SecretKey};
use serde::{Deserialize, Serialize};

const REGISTRATION_TAG: &str = "proscenium/server-registration/2";
const PROFILE_UPDATE_TAG: &str = "proscenium/server-profile-update/2";

/// `RegistrationRequest::action` value for `DELETE /api/v1/register`.
pub const REGISTRATION_ACTION_UNREGISTER: &str = "unregister";

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
    /// Signature from the signing key named in `delegation`.
    pub signature: String,
    /// The signing key delegation, signed by the master key.
    pub delegation: SigningKeyDelegation,
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

/// Sign a registration payload with the signing key.
pub fn sign_registration(payload: &RegistrationPayload, signing_secret_key: &SecretKey) -> String {
    let bytes = registration_signing_bytes(
        &payload.master_pubkey,
        &payload.transport_node_id,
        &payload.server_url,
        payload.timestamp,
        &payload.visibility,
        &payload.action,
    );
    let sig = signing_secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a registration request.
///
/// Fails unless: the delegation is signed by the master key it names, that master key
/// equals `request.master_pubkey`, `transport_node_id` is a canonical public key, and the
/// request signature was made by the delegated signing key.
///
/// Does not check `timestamp` or `server_url`; the server compares those with its clock
/// and configured URL.
pub fn verify_registration_signature(request: &RegistrationRequest) -> Result<(), String> {
    verify_delegation(&request.delegation)?;
    if request.delegation.master_pubkey != request.master_pubkey {
        return Err("delegation master_pubkey does not match request".to_string());
    }
    if !is_valid_pubkey(&request.transport_node_id) {
        return Err("invalid transport NodeId".to_string());
    }
    let sig = hex_to_signature(&request.signature)?;
    let signing_pubkey: PublicKey = request
        .delegation
        .signing_pubkey
        .parse()
        .map_err(|e| format!("invalid signing pubkey in delegation: {e}"))?;
    let bytes = registration_signing_bytes(
        &request.master_pubkey,
        &request.transport_node_id,
        &request.server_url,
        request.timestamp,
        &request.visibility,
        &request.action,
    );
    signing_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "registration signature verification failed".to_string())
}

/// Body of `PUT /api/v1/register`: replaces the profile a server holds for a registered user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileUpdateRequest {
    /// The user's permanent identity (master public key).
    pub master_pubkey: String,
    pub server_url: String,
    pub timestamp: u64,
    /// The profile, signed by the signing key for `master_pubkey`.
    pub profile: Profile,
    /// Signature from the signing key over
    /// `(master_pubkey, server_url, timestamp, profile.signature)`.
    pub signature: String,
    /// The signing key delegation, signed by the master key.
    pub delegation: SigningKeyDelegation,
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

/// Sign a profile update for `server_url`. `profile` must already be signed with `sign_profile`.
pub fn sign_profile_update(
    master_pubkey: &str,
    server_url: &str,
    timestamp: u64,
    profile: &Profile,
    signing_secret_key: &SecretKey,
) -> String {
    let bytes =
        profile_update_signing_bytes(master_pubkey, server_url, timestamp, &profile.signature);
    let sig = signing_secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify a profile update request.
///
/// Fails unless: the delegation is signed by the master key it names, that master key
/// equals `request.master_pubkey`, the profile passes `validate_profile` and is signed for
/// `master_pubkey` by the delegated signing key, and the request signature was made by
/// the same key.
///
/// Does not check `timestamp` or `server_url`; the server compares those with its clock
/// and configured URL.
pub fn verify_profile_update(request: &ProfileUpdateRequest) -> Result<(), String> {
    verify_delegation(&request.delegation)?;
    if request.delegation.master_pubkey != request.master_pubkey {
        return Err("delegation master_pubkey does not match request".to_string());
    }
    let signing_pubkey: PublicKey = request
        .delegation
        .signing_pubkey
        .parse()
        .map_err(|e| format!("invalid signing pubkey in delegation: {e}"))?;
    validate_profile(&request.profile)?;
    verify_profile_signature(&request.profile, &request.master_pubkey, &signing_pubkey)?;
    let sig = hex_to_signature(&request.signature)?;
    let bytes = profile_update_signing_bytes(
        &request.master_pubkey,
        &request.server_url,
        request.timestamp,
        &request.profile.signature,
    );
    signing_pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "profile update signature verification failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::{derive_signing_key, sign_delegation};
    use crate::signing::sign_profile;
    use crate::validation::now_millis;

    fn keys(seed: u8) -> (SecretKey, SecretKey, SigningKeyDelegation) {
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
        (master, signing, delegation)
    }

    fn request(
        master: &SecretKey,
        signing: &SecretKey,
        delegation: SigningKeyDelegation,
    ) -> RegistrationRequest {
        let payload = RegistrationPayload {
            master_pubkey: master.public().to_string(),
            transport_node_id: SecretKey::from_bytes(&[9u8; 32]).public().to_string(),
            server_url: "https://example.test".to_string(),
            timestamp: now_millis(),
            visibility: Visibility::Public,
            action: None,
        };
        let signature = sign_registration(&payload, signing);
        RegistrationRequest {
            master_pubkey: payload.master_pubkey,
            transport_node_id: payload.transport_node_id,
            server_url: payload.server_url,
            timestamp: payload.timestamp,
            visibility: payload.visibility,
            action: payload.action,
            signature,
            delegation,
        }
    }

    #[test]
    fn valid_registration_verifies() {
        let (master, signing, delegation) = keys(1);
        assert!(verify_registration_signature(&request(&master, &signing, delegation)).is_ok());
    }

    #[test]
    fn registration_with_self_made_delegation_is_rejected() {
        // The attacker signs with its own key and names the victim as master.
        let (victim_master, _, _) = keys(1);
        let (_, attacker_signing, mut attacker_delegation) = keys(2);
        attacker_delegation.master_pubkey = victim_master.public().to_string();
        let req = request(&victim_master, &attacker_signing, attacker_delegation);
        assert!(verify_registration_signature(&req).is_err());
    }

    #[test]
    fn registration_with_delegation_of_other_identity_is_rejected() {
        let (victim_master, _, _) = keys(1);
        let (_, attacker_signing, attacker_delegation) = keys(2);
        let req = request(&victim_master, &attacker_signing, attacker_delegation);
        assert!(verify_registration_signature(&req).is_err());
    }

    #[test]
    fn registration_rejects_non_key_node_id() {
        let (master, signing, delegation) = keys(1);
        let mut req = request(&master, &signing, delegation);
        req.transport_node_id = "aaaaaaa\u{20ac}".to_string();
        assert!(verify_registration_signature(&req).is_err());
    }

    #[test]
    fn profile_update_covers_profile_fields() {
        let (master, signing, delegation) = keys(1);
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
        sign_profile(&mut profile, &master_pubkey, &signing);
        let timestamp = now_millis();
        let url = "https://example.test";
        let signature = sign_profile_update(&master_pubkey, url, timestamp, &profile, &signing);
        let mut req = ProfileUpdateRequest {
            master_pubkey,
            server_url: url.to_string(),
            timestamp,
            profile,
            signature,
            delegation,
        };
        assert!(verify_profile_update(&req).is_ok());
        req.profile.bio = "changed".to_string();
        assert!(verify_profile_update(&req).is_err());
    }
}
