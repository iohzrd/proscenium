use crate::types::{Interaction, Post, Profile};
use iroh::PublicKey;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_POST_CONTENT_LEN: usize = 10_000;
pub const MAX_MEDIA_COUNT: usize = 10;
pub const MAX_TIMESTAMP_DRIFT_MS: u64 = 5 * 60 * 1000;
pub const MAX_BLOB_SIZE: usize = 50 * 1024 * 1024;
pub const MAX_DISPLAY_NAME_LEN: usize = 200;
pub const MAX_BIO_LEN: usize = 2_000;

/// Return the first 8 characters of an ID string, or the whole string if shorter.
pub fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

pub fn validate_profile(profile: &Profile) -> Result<(), String> {
    if profile.display_name.len() > MAX_DISPLAY_NAME_LEN {
        return Err(format!(
            "display name too long: {} bytes (max {})",
            profile.display_name.len(),
            MAX_DISPLAY_NAME_LEN
        ));
    }
    if profile.bio.len() > MAX_BIO_LEN {
        return Err(format!(
            "bio too long: {} bytes (max {})",
            profile.bio.len(),
            MAX_BIO_LEN
        ));
    }
    Ok(())
}

pub fn validate_interaction(interaction: &Interaction) -> Result<(), String> {
    let now = now_millis();
    if interaction.timestamp > now + MAX_TIMESTAMP_DRIFT_MS {
        return Err(format!(
            "interaction timestamp {} is too far in the future (now: {})",
            interaction.timestamp, now
        ));
    }
    Ok(())
}

pub fn validate_post(post: &Post) -> Result<(), String> {
    if post.content.len() > MAX_POST_CONTENT_LEN {
        return Err(format!(
            "post content too long: {} bytes (max {})",
            post.content.len(),
            MAX_POST_CONTENT_LEN
        ));
    }
    if post.media.len() > MAX_MEDIA_COUNT {
        return Err(format!(
            "too many media attachments: {} (max {})",
            post.media.len(),
            MAX_MEDIA_COUNT
        ));
    }
    let now = now_millis();
    if post.timestamp > now + MAX_TIMESTAMP_DRIFT_MS {
        return Err(format!(
            "post timestamp {} is too far in the future (now: {})",
            post.timestamp, now
        ));
    }
    Ok(())
}

/// Extract all valid pubkey mentions from post content.
/// Mentions use the format @{pubkey} where pubkey is a valid iroh public key hex string.
pub fn parse_mentions(content: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
                end += 1;
            }
            let len = end - start;
            if len >= 52 {
                let candidate = content[start..end].to_lowercase();
                if candidate.parse::<PublicKey>().is_ok() && !mentions.contains(&candidate) {
                    mentions.push(candidate);
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    mentions
}
