//! Signed test fixtures and endpoints for tests.

use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};
use proscenium_types::{
    DeviceEntry, Interaction, InteractionKind, LinkedDevicesAnnouncement, Post, Profile,
    ProfileUpdateRequest, RegistrationPayload, RegistrationRequest, SigningKeyDelegation,
    Visibility, derive_signing_key, sign_delegation, sign_interaction,
    sign_linked_devices_announcement, sign_post, sign_profile, sign_profile_update,
    sign_registration,
};

/// An identity with a master key derived from `seed` and a delegated signing key.
pub struct TestUser {
    master: SecretKey,
    master_bytes: [u8; 32],
    pub signing: SecretKey,
    pub master_pubkey: String,
    pub transport_node_id: String,
    pub delegation: SigningKeyDelegation,
}

/// An endpoint with one UDP socket on 127.0.0.1 (port chosen by the OS), no relay and
/// no address lookup service.
pub async fn loopback_endpoint(secret: SecretKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// The public key string of the key with bytes `[seed; 32]`.
pub fn node_id(seed: u8) -> String {
    SecretKey::from_bytes(&[seed; 32]).public().to_string()
}

impl TestUser {
    /// Identity `seed` with signing key index 0.
    pub fn new(seed: u8) -> Self {
        Self::with_key_index(seed, 0)
    }

    /// Identity `seed` with the signing key and delegation of `key_index`.
    pub fn with_key_index(seed: u8, key_index: u32) -> Self {
        let master_bytes = [seed; 32];
        let master = SecretKey::from_bytes(&master_bytes);
        let signing = SecretKey::from_bytes(&derive_signing_key(&master_bytes, key_index));
        let delegation = sign_delegation(
            &master,
            &signing.public(),
            key_index,
            &"ab".repeat(32),
            0,
            1_000 + u64::from(key_index),
        );
        Self {
            master_pubkey: master.public().to_string(),
            // Distinct from every master and signing key used in tests.
            transport_node_id: node_id(seed.wrapping_add(100)),
            master,
            master_bytes,
            signing,
            delegation,
        }
    }

    /// The same identity after a signing key rotation to `key_index`.
    pub fn rotated(&self, key_index: u32) -> Self {
        let mut rotated = Self::with_key_index(self.master_bytes[0], key_index);
        rotated.transport_node_id = self.transport_node_id.clone();
        rotated
    }

    /// A delegation for the current signing key with another DM key index.
    pub fn delegation_with_dm_index(&self, dm_key_index: u32) -> SigningKeyDelegation {
        sign_delegation(
            &self.master,
            &self.signing.public(),
            self.delegation.key_index,
            &"cd".repeat(32),
            dm_key_index,
            2_000,
        )
    }

    pub fn sign_registration_request(&self, req: &RegistrationRequest) -> String {
        let payload = RegistrationPayload {
            master_pubkey: req.master_pubkey.clone(),
            transport_node_id: req.transport_node_id.clone(),
            server_url: req.server_url.clone(),
            timestamp: req.timestamp,
            visibility: req.visibility,
            action: req.action.clone(),
        };
        sign_registration(&payload, &self.signing)
    }

    pub fn registration(
        &self,
        server_url: &str,
        timestamp: u64,
        visibility: Visibility,
        action: Option<&str>,
    ) -> RegistrationRequest {
        let mut req = RegistrationRequest {
            master_pubkey: self.master_pubkey.clone(),
            transport_node_id: self.transport_node_id.clone(),
            server_url: server_url.to_string(),
            timestamp,
            visibility,
            action: action.map(str::to_string),
            signature: String::new(),
            delegation: self.delegation.clone(),
        };
        req.signature = self.sign_registration_request(&req);
        req
    }

    pub fn profile(&self, display_name: &str, visibility: Visibility, updated_at: u64) -> Profile {
        let mut profile = Profile {
            display_name: display_name.to_string(),
            bio: format!("bio of {display_name}"),
            avatar_hash: None,
            avatar_ticket: None,
            visibility,
            updated_at,
            signature: String::new(),
        };
        sign_profile(&mut profile, &self.master_pubkey, &self.signing);
        profile
    }

    pub fn profile_update(
        &self,
        server_url: &str,
        timestamp: u64,
        profile: Profile,
    ) -> ProfileUpdateRequest {
        let signature = sign_profile_update(
            &self.master_pubkey,
            server_url,
            timestamp,
            &profile,
            &self.signing,
        );
        ProfileUpdateRequest {
            master_pubkey: self.master_pubkey.clone(),
            server_url: server_url.to_string(),
            timestamp,
            profile,
            signature,
            delegation: self.delegation.clone(),
        }
    }

    pub fn post(&self, content: &str, timestamp: u64) -> Post {
        self.post_with(content, timestamp, None, None)
    }

    /// A post that replies to `reply_to` and/or quotes `quote_of`.
    pub fn post_with(
        &self,
        content: &str,
        timestamp: u64,
        reply_to: Option<&Post>,
        quote_of: Option<&Post>,
    ) -> Post {
        let mut post = Post {
            id: String::new(),
            author: self.master_pubkey.clone(),
            content: content.to_string(),
            timestamp,
            media: vec![],
            reply_to: reply_to.map(|p| p.id.clone()),
            reply_to_author: reply_to.map(|p| p.author.clone()),
            quote_of: quote_of.map(|p| p.id.clone()),
            quote_of_author: quote_of.map(|p| p.author.clone()),
            signature: String::new(),
        };
        sign_post(&mut post, &self.signing);
        post
    }

    /// A `Like` of `target` with the id `{n:032x}`.
    pub fn like(&self, target: &Post, n: u32, timestamp: u64) -> Interaction {
        let mut interaction = Interaction {
            id: format!("{n:032x}"),
            author: self.master_pubkey.clone(),
            kind: InteractionKind::Like,
            target_post_id: target.id.clone(),
            target_author: target.author.clone(),
            timestamp,
            signature: String::new(),
        };
        sign_interaction(&mut interaction, &self.signing);
        interaction
    }

    pub fn announcement(&self, version: u64, node_ids: &[String]) -> LinkedDevicesAnnouncement {
        let mut announcement = LinkedDevicesAnnouncement {
            master_pubkey: self.master_pubkey.clone(),
            delegation: self.delegation.clone(),
            devices: node_ids
                .iter()
                .map(|node_id| DeviceEntry {
                    node_id: node_id.clone(),
                    device_name: "device".to_string(),
                    is_primary: false,
                    added_at: 1,
                })
                .collect(),
            version,
            timestamp: version,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut announcement, &self.signing);
        announcement
    }
}
