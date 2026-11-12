//! Signed test fixtures and endpoints for tests.

use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};
use proscenium_types::{
    DeviceEntry, Interaction, InteractionKind, LinkedDevicesAnnouncement, Post, Profile,
    ProfileUpdateRequest, RegistrationPayload, RegistrationRequest, Visibility, sign_interaction,
    sign_linked_devices_announcement, sign_post, sign_profile, sign_profile_update,
    sign_registration,
};

/// A test identity. Its methods sign with `key` and name `master_pubkey` as the author.
pub struct TestUser {
    pub key: SecretKey,
    pub master_pubkey: String,
    pub transport_node_id: String,
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
    /// The identity whose master key has the bytes `[seed; 32]`.
    pub fn new(seed: u8) -> Self {
        let key = SecretKey::from_bytes(&[seed; 32]);
        Self {
            master_pubkey: key.public().to_string(),
            // Distinct from every master key used in tests.
            transport_node_id: node_id(seed.wrapping_add(100)),
            key,
        }
    }

    /// A user that names `victim.master_pubkey` as the author and signs with the key of
    /// `self`, so its signatures fail verification.
    pub fn forging_as(&self, victim: &TestUser) -> Self {
        Self {
            key: self.key.clone(),
            master_pubkey: victim.master_pubkey.clone(),
            transport_node_id: self.transport_node_id.clone(),
        }
    }

    pub fn registration(
        &self,
        server_url: &str,
        timestamp: u64,
        visibility: Visibility,
        action: Option<&str>,
    ) -> RegistrationRequest {
        let payload = RegistrationPayload {
            master_pubkey: self.master_pubkey.clone(),
            transport_node_id: self.transport_node_id.clone(),
            server_url: server_url.to_string(),
            timestamp,
            visibility,
            action: action.map(str::to_string),
        };
        let signature = sign_registration(&payload, &self.key);
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
        sign_profile(&mut profile, &self.master_pubkey, &self.key);
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
            &self.key,
        );
        ProfileUpdateRequest {
            master_pubkey: self.master_pubkey.clone(),
            server_url: server_url.to_string(),
            timestamp,
            profile,
            signature,
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
        sign_post(&mut post, &self.key);
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
        sign_interaction(&mut interaction, &self.key);
        interaction
    }

    pub fn announcement(&self, version: u64, node_ids: &[String]) -> LinkedDevicesAnnouncement {
        let mut announcement = LinkedDevicesAnnouncement {
            master_pubkey: self.master_pubkey.clone(),
            devices: node_ids
                .iter()
                .map(|node_id| DeviceEntry {
                    node_id: node_id.clone(),
                    device_name: "device".to_string(),
                    added_at: 1,
                })
                .collect(),
            version,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut announcement, &self.key);
        announcement
    }
}
