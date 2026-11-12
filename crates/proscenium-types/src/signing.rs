use crate::protocol::{
    IdentityResponse, LinkedDevicesAnnouncement, MAX_DEVICE_NAME_LEN, MAX_LINKED_DEVICES,
    MAX_REMOVED_DEVICES,
};
use crate::types::{Interaction, Post, Profile};
use crate::validation::{ID_HEX_LEN, is_valid_pubkey, parse_pubkey, validate_profile};
use iroh::{SecretKey, Signature};
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

/// Verify that `signature` (hex) over `bytes` was made by the key `signer` (canonical
/// pubkey string). `what` names the signed message in the error.
pub(crate) fn verify_signed_by(
    signer: &str,
    bytes: &[u8],
    signature: &str,
    what: &str,
) -> Result<(), String> {
    let key = parse_pubkey(signer)?;
    let sig = hex_to_signature(signature)?;
    key.verify(bytes, &sig)
        .map_err(|_| format!("{what} signature verification failed"))
}

/// Set `post.id` from the post content, then sign the post in place with the author's
/// master key.
pub fn sign_post(post: &mut Post, master_key: &SecretKey) {
    post.id = compute_post_id(post);
    let bytes = post_signing_bytes(post);
    let sig = master_key.sign(&bytes);
    post.signature = signature_to_hex(&sig);
}

/// Sign an Interaction in place with the author's master key.
pub fn sign_interaction(interaction: &mut Interaction, master_key: &SecretKey) {
    let bytes = interaction_signing_bytes(interaction);
    let sig = master_key.sign(&bytes);
    interaction.signature = signature_to_hex(&sig);
}

/// Verify that `post.signature` was made by the key `post.author`.
pub fn verify_post_signature(post: &Post) -> Result<(), String> {
    verify_signed_by(
        &post.author,
        &post_signing_bytes(post),
        &post.signature,
        "post",
    )
}

/// Verify that `interaction.signature` was made by the key `interaction.author`.
pub fn verify_interaction_signature(interaction: &Interaction) -> Result<(), String> {
    verify_signed_by(
        &interaction.author,
        &interaction_signing_bytes(interaction),
        &interaction.signature,
        "interaction",
    )
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

/// Sign a Profile in place. `author` is the owner's master pubkey and `master_key` the
/// matching secret key.
pub fn sign_profile(profile: &mut Profile, author: &str, master_key: &SecretKey) {
    let bytes = profile_signing_bytes(profile, author);
    let sig = master_key.sign(&bytes);
    profile.signature = signature_to_hex(&sig);
}

/// Verify that `profile.signature` was made by the key `author` for `author`.
pub fn verify_profile_signature(profile: &Profile, author: &str) -> Result<(), String> {
    verify_signed_by(
        author,
        &profile_signing_bytes(profile, author),
        &profile.signature,
        "profile",
    )
}

/// Canonical bytes for signing a delete-post action.
fn delete_post_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    canonical_bytes(&(DELETE_POST_TAG, id, author))
}

/// Sign a delete-post action with the author's master key. Returns the hex-encoded
/// signature.
pub fn sign_delete_post(id: &str, author: &str, master_key: &SecretKey) -> String {
    let bytes = delete_post_signing_bytes(id, author);
    let sig = master_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify that a delete-post `signature` was made by the key `author`.
pub fn verify_delete_post_signature(id: &str, author: &str, signature: &str) -> Result<(), String> {
    verify_signed_by(
        author,
        &delete_post_signing_bytes(id, author),
        signature,
        "delete post",
    )
}

/// Canonical bytes for signing a delete-interaction action.
fn delete_interaction_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    canonical_bytes(&(DELETE_INTERACTION_TAG, id, author))
}

/// Sign a delete-interaction action with the author's master key. Returns the
/// hex-encoded signature.
pub fn sign_delete_interaction(id: &str, author: &str, master_key: &SecretKey) -> String {
    let bytes = delete_interaction_signing_bytes(id, author);
    let sig = master_key.sign(&bytes);
    signature_to_hex(&sig)
}

/// Verify that a delete-interaction `signature` was made by the key `author`.
pub fn verify_delete_interaction_signature(
    id: &str,
    author: &str,
    signature: &str,
) -> Result<(), String> {
    verify_signed_by(
        author,
        &delete_interaction_signing_bytes(id, author),
        signature,
        "delete interaction",
    )
}

/// Canonical bytes for signing a LinkedDevicesAnnouncement (every field except
/// `signature`).
fn linked_devices_signing_bytes(announcement: &LinkedDevicesAnnouncement) -> Vec<u8> {
    canonical_bytes(&(
        LINKED_DEVICES_TAG,
        &announcement.master_pubkey,
        &announcement.devices,
        &announcement.removed,
        announcement.version,
    ))
}

/// Sign a LinkedDevicesAnnouncement in place with the master key of
/// `announcement.master_pubkey`.
pub fn sign_linked_devices_announcement(
    announcement: &mut LinkedDevicesAnnouncement,
    master_key: &SecretKey,
) {
    let bytes = linked_devices_signing_bytes(announcement);
    let sig = master_key.sign(&bytes);
    announcement.signature = signature_to_hex(&sig);
}

/// Verify a LinkedDevicesAnnouncement: the list holds 1 to `MAX_LINKED_DEVICES` devices
/// and at most `MAX_REMOVED_DEVICES` removed NodeIds, every NodeId is a canonical public
/// key, no removed NodeId is listed as a device, every device name is at most
/// `MAX_DEVICE_NAME_LEN` bytes, and the signature was made by `master_pubkey`.
///
/// Does not check the version: a list signed by a device with a fast clock still
/// authenticates its devices. A cache decides with `announcement_replaces`.
pub fn verify_linked_devices_announcement(
    announcement: &LinkedDevicesAnnouncement,
) -> Result<(), String> {
    if announcement.devices.is_empty() || announcement.devices.len() > MAX_LINKED_DEVICES {
        return Err(format!(
            "announcement lists {} devices (allowed 1..={MAX_LINKED_DEVICES})",
            announcement.devices.len()
        ));
    }
    if announcement.removed.len() > MAX_REMOVED_DEVICES {
        return Err(format!(
            "announcement lists {} removed devices (allowed {MAX_REMOVED_DEVICES})",
            announcement.removed.len()
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
    for node_id in &announcement.removed {
        if !is_valid_pubkey(node_id) {
            return Err("announcement contains an invalid removed NodeId".to_string());
        }
        if announcement.devices.iter().any(|d| d.node_id == *node_id) {
            return Err("announcement lists a removed NodeId as a device".to_string());
        }
    }
    verify_signed_by(
        &announcement.master_pubkey,
        &linked_devices_signing_bytes(announcement),
        &announcement.signature,
        "linked devices announcement",
    )
}

/// True if a cache that holds a device list with version `stored` replaces it with a list
/// of the same identity with version `new`, at local time `now`.
///
/// A version more than `MAX_TIMESTAMP_DRIFT_MS` after `now` is ahead of the clock. A list
/// that is not ahead replaces one that is; a list that is ahead never replaces one that is
/// not; otherwise the higher version wins. A list signed far in the future therefore never
/// displaces a current list, however often it is delivered, and a list from a device with
/// a fast clock is stored when no current list is.
pub fn announcement_replaces(new: u64, stored: u64, now: u64) -> bool {
    let limit = now.saturating_add(crate::validation::MAX_TIMESTAMP_DRIFT_MS);
    match (new > limit, stored > limit) {
        (false, true) => true,
        (true, false) => false,
        _ => new > stored,
    }
}

/// An `IdentityResponse` whose device list and profile signatures were verified, and
/// whose signed device list contains the NodeId that sent it.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    pub master_pubkey: String,
    pub announcement: LinkedDevicesAnnouncement,
    pub profile: Option<Profile>,
    /// The NodeId that sent the response. `announcement` lists it.
    pub node_id: String,
}

impl VerifiedIdentity {
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
/// Fails unless: the device list passes `verify_linked_devices_announcement`,
/// `remote_node_id` is in that device list, and the profile (if present) passes
/// `validate_profile` and is signed by the master key of the device list.
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
        verify_profile_signature(profile, &master_pubkey)?;
    }

    Ok(VerifiedIdentity {
        master_pubkey,
        announcement: response.announcement,
        profile: response.profile,
        node_id: remote_node_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DeviceEntry, Visibility};
    use crate::validation::{now_millis, validate_post};

    struct TestIdentity {
        master_pubkey: String,
        key: SecretKey,
    }

    fn identity(seed: u8) -> TestIdentity {
        let key = SecretKey::from_bytes(&[seed; 32]);
        TestIdentity {
            master_pubkey: key.public().to_string(),
            key,
        }
    }

    fn node_id(seed: u8) -> String {
        SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    fn announcement(id: &TestIdentity, nodes: &[String]) -> LinkedDevicesAnnouncement {
        let mut a = LinkedDevicesAnnouncement {
            master_pubkey: id.master_pubkey.clone(),
            devices: nodes
                .iter()
                .map(|n| DeviceEntry {
                    node_id: n.clone(),
                    device_name: "d".to_string(),
                    added_at: 1,
                })
                .collect(),
            removed: vec![],
            version: 1,
            signature: String::new(),
        };
        sign_linked_devices_announcement(&mut a, &id.key);
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

    fn profile(visibility: Visibility, updated_at: u64) -> Profile {
        Profile {
            display_name: "n".to_string(),
            bio: String::new(),
            avatar_hash: None,
            avatar_ticket: None,
            visibility,
            updated_at,
            signature: String::new(),
        }
    }

    #[test]
    fn post_id_is_derived_and_author_bound() {
        let a = identity(1);
        let b = identity(2);
        let mut p = post(&a.master_pubkey);
        sign_post(&mut p, &a.key);
        assert!(validate_post(&p).is_ok());
        assert!(verify_post_signature(&p).is_ok());

        // Same content under another author yields another id.
        let mut q = post(&b.master_pubkey);
        q.timestamp = p.timestamp;
        assert_ne!(compute_post_id(&q), p.id);

        // Reusing the id under another author fails validation.
        q.id = p.id.clone();
        assert!(validate_post(&q).is_err());
    }

    #[test]
    fn post_signed_by_another_key_than_its_author_is_rejected() {
        let a = identity(1);
        let b = identity(2);
        let mut forged = post(&b.master_pubkey);
        sign_post(&mut forged, &a.key);
        assert!(verify_post_signature(&forged).is_err());

        let mut tampered = post(&a.master_pubkey);
        sign_post(&mut tampered, &a.key);
        tampered.content = "changed".to_string();
        assert!(verify_post_signature(&tampered).is_err());
    }

    #[test]
    fn profile_signature_binds_author_and_version() {
        let a = identity(1);
        let b = identity(2);
        let mut profile = profile(Visibility::Public, 10);
        sign_profile(&mut profile, &a.master_pubkey, &a.key);
        assert!(verify_profile_signature(&profile, &a.master_pubkey).is_ok());
        assert!(verify_profile_signature(&profile, &b.master_pubkey).is_err());
        profile.updated_at = 11;
        assert!(verify_profile_signature(&profile, &a.master_pubkey).is_err());
    }

    #[test]
    fn delete_signatures_verify_only_for_their_author() {
        let a = identity(1);
        let b = identity(2);
        let id = "00".repeat(16);
        let sig = sign_delete_post(&id, &a.master_pubkey, &a.key);
        assert!(verify_delete_post_signature(&id, &a.master_pubkey, &sig).is_ok());
        assert!(verify_delete_post_signature(&id, &b.master_pubkey, &sig).is_err());
        // A delete-post signature is not a delete-interaction signature.
        assert!(verify_delete_interaction_signature(&id, &a.master_pubkey, &sig).is_err());
        let sig = sign_delete_interaction(&id, &a.master_pubkey, &a.key);
        assert!(verify_delete_interaction_signature(&id, &a.master_pubkey, &sig).is_ok());
    }

    #[test]
    fn announcement_ahead_of_the_clock_verifies() {
        let a = identity(1);
        let mut list = announcement(&a, &[node_id(50)]);
        list.version = u64::MAX;
        sign_linked_devices_announcement(&mut list, &a.key);
        assert!(verify_linked_devices_announcement(&list).is_ok());
    }

    #[test]
    fn a_list_ahead_of_the_clock_never_displaces_a_current_list() {
        let now = 1_000_000_000;
        let drift = crate::validation::MAX_TIMESTAMP_DRIFT_MS;
        let ahead = now + drift + 1;
        // Current lists: the higher version wins.
        assert!(announcement_replaces(now, now - 1, now));
        assert!(!announcement_replaces(now - 1, now, now));
        assert!(!announcement_replaces(now, now, now));
        // A list ahead of the clock does not replace a current one, at any version.
        assert!(!announcement_replaces(ahead, now, now));
        assert!(!announcement_replaces(u64::MAX, now - 1, now));
        // A current list replaces one ahead of the clock, even with a lower version.
        assert!(announcement_replaces(now - 1, ahead, now));
        // Among lists ahead of the clock the higher version wins.
        assert!(announcement_replaces(ahead + 1, ahead, now));
        assert!(!announcement_replaces(ahead, ahead + 1, now));
        // A version within the drift limit counts as current.
        assert!(announcement_replaces(now + drift, now, now));
    }

    #[test]
    fn removed_devices_are_signed_bounded_and_not_listed() {
        let a = identity(1);
        let mut list = announcement(&a, &[node_id(50)]);
        list.removed = vec![node_id(51)];
        sign_linked_devices_announcement(&mut list, &a.key);
        assert!(verify_linked_devices_announcement(&list).is_ok());

        // The removed list is covered by the signature.
        let mut changed = list.clone();
        changed.removed.clear();
        assert!(verify_linked_devices_announcement(&changed).is_err());

        // A NodeId cannot be listed and removed at once.
        let mut both = list.clone();
        both.removed = vec![node_id(50)];
        sign_linked_devices_announcement(&mut both, &a.key);
        assert!(verify_linked_devices_announcement(&both).is_err());

        let mut invalid = list.clone();
        invalid.removed = vec!["zz".to_string()];
        sign_linked_devices_announcement(&mut invalid, &a.key);
        assert!(verify_linked_devices_announcement(&invalid).is_err());

        let mut too_many = list;
        too_many.removed = (0..=MAX_REMOVED_DEVICES as u8)
            .map(|i| node_id(100 + i))
            .collect();
        sign_linked_devices_announcement(&mut too_many, &a.key);
        assert!(verify_linked_devices_announcement(&too_many).is_err());
        too_many.removed.pop();
        sign_linked_devices_announcement(&mut too_many, &a.key);
        assert!(verify_linked_devices_announcement(&too_many).is_ok());
    }

    #[test]
    fn stage_announcement_requires_a_matching_ticket_and_a_recent_start() {
        use crate::protocol::{
            MAX_STAGE_ANNOUNCEMENT_AGE_MS, sign_stage_announcement, verify_stage_announcement,
        };
        use crate::stage::StageTicket;
        let host = identity(1);
        let ticket = StageTicket {
            stage_id: "ab".repeat(16),
            host_pubkey: host.master_pubkey.clone(),
            host_node_id: node_id(50),
            title: "room".to_string(),
        };
        let check = |ticket: &StageTicket, title: &str, started_at: u64| {
            let sig = sign_stage_announcement(
                &ticket.stage_id,
                title,
                ticket,
                &host.master_pubkey,
                started_at,
                &host.key,
            );
            verify_stage_announcement(
                &ticket.stage_id,
                title,
                ticket,
                &host.master_pubkey,
                started_at,
                &sig,
            )
        };
        let now = crate::validation::now_millis();
        assert!(check(&ticket, "room", now).is_ok());
        // A title other than the ticket's, a start older than the limit or far ahead.
        assert!(check(&ticket, "other", now).is_err());
        assert!(check(&ticket, "room", now - MAX_STAGE_ANNOUNCEMENT_AGE_MS - 1).is_err());
        assert!(
            check(
                &ticket,
                "room",
                now + crate::validation::MAX_TIMESTAMP_DRIFT_MS + 60_000
            )
            .is_err()
        );
        // A ticket of another host.
        let foreign = StageTicket {
            host_pubkey: identity(2).master_pubkey,
            ..ticket.clone()
        };
        assert!(check(&foreign, "room", now).is_err());
    }

    #[test]
    fn identity_response_requires_listed_node_and_master_signature() {
        let a = identity(1);
        let listed = node_id(50);
        let other = node_id(51);
        let response = IdentityResponse {
            announcement: announcement(&a, std::slice::from_ref(&listed)),
            profile: None,
        };
        assert!(verify_identity_response(response.clone(), &listed).is_ok());
        assert!(verify_identity_response(response.clone(), &other).is_err());

        // The device list is covered by the signature.
        let mut tampered = response;
        tampered.announcement.devices[0].device_name = "other".to_string();
        assert!(verify_identity_response(tampered, &listed).is_err());

        // A device list for a's identity signed by b's key is rejected.
        let b = identity(2);
        let mut forged = announcement(&b, std::slice::from_ref(&other));
        forged.master_pubkey = a.master_pubkey.clone();
        sign_linked_devices_announcement(&mut forged, &b.key);
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
        let mut profile = profile(Visibility::Private, 1);
        sign_profile(&mut profile, &b.master_pubkey, &b.key);
        let response = IdentityResponse {
            announcement: announcement(&a, std::slice::from_ref(&listed)),
            profile: Some(profile),
        };
        assert!(verify_identity_response(response, &listed).is_err());
    }
}
