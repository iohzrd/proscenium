use crate::signing::compute_post_id;
use crate::types::{Interaction, MediaAttachment, Post, Profile};
use iroh::PublicKey;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_POST_CONTENT_LEN: usize = 10_000;
pub const MAX_MEDIA_COUNT: usize = 10;
pub const MAX_TIMESTAMP_DRIFT_MS: u64 = 5 * 60 * 1000;
pub const MAX_BLOB_SIZE: usize = 50 * 1024 * 1024;
pub const MAX_DISPLAY_NAME_LEN: usize = 200;
pub const MAX_BIO_LEN: usize = 2_000;
/// Length of post, interaction and message ids (lowercase hex digits).
pub const ID_HEX_LEN: usize = 32;
pub const MAX_BLOB_HASH_LEN: usize = 128;
pub const MAX_BLOB_TICKET_LEN: usize = 2_048;
pub const MAX_MIME_TYPE_LEN: usize = 127;
pub const MAX_FILENAME_LEN: usize = 255;

/// Return the longest prefix of `s` holding at most `max_chars` characters.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Return the first 8 characters of an ID string, or the whole string if shorter.
pub fn short_id(id: &str) -> &str {
    truncate_chars(id, 8)
}

/// Milliseconds since the Unix epoch; 0 if the system clock is before the epoch.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// True if `s` is exactly `ID_HEX_LEN` lowercase hex digits.
pub fn is_valid_id(s: &str) -> bool {
    s.len() == ID_HEX_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// True if `s` is the canonical string form of an Ed25519 public key.
/// Rejects alternate encodings of the same key, so string equality equals key equality.
pub fn is_valid_pubkey(s: &str) -> bool {
    parse_pubkey(s).is_ok()
}

/// Parse `s` as an Ed25519 public key. Returns an error unless `s` is the canonical
/// string form of the key (see `is_valid_pubkey`).
pub fn parse_pubkey(s: &str) -> Result<PublicKey, String> {
    let key: PublicKey = s.parse().map_err(|e| format!("invalid pubkey: {e}"))?;
    if key.to_string() != s {
        return Err("pubkey is not in canonical form".to_string());
    }
    Ok(key)
}

/// True if `timestamp` is at most `MAX_TIMESTAMP_DRIFT_MS` away from the local clock.
pub fn is_recent_timestamp(timestamp: u64) -> bool {
    timestamp.abs_diff(now_millis()) <= MAX_TIMESTAMP_DRIFT_MS
}

pub fn validate_media_attachment(media: &MediaAttachment) -> Result<(), String> {
    if media.hash.is_empty() || media.hash.len() > MAX_BLOB_HASH_LEN {
        return Err(format!(
            "media hash length {} out of range (1..={MAX_BLOB_HASH_LEN})",
            media.hash.len()
        ));
    }
    if media.ticket.is_empty() || media.ticket.len() > MAX_BLOB_TICKET_LEN {
        return Err(format!(
            "media ticket length {} out of range (1..={MAX_BLOB_TICKET_LEN})",
            media.ticket.len()
        ));
    }
    if media.mime_type.len() > MAX_MIME_TYPE_LEN {
        return Err(format!(
            "media mime type too long: {} bytes (max {MAX_MIME_TYPE_LEN})",
            media.mime_type.len()
        ));
    }
    if media.filename.len() > MAX_FILENAME_LEN {
        return Err(format!(
            "media filename too long: {} bytes (max {MAX_FILENAME_LEN})",
            media.filename.len()
        ));
    }
    if media.size > MAX_BLOB_SIZE as u64 {
        return Err(format!(
            "media size {} exceeds {MAX_BLOB_SIZE} bytes",
            media.size
        ));
    }
    Ok(())
}

/// Validate a list of attachments: count, per-item bounds, no repeated hash.
pub fn validate_media_list(media: &[MediaAttachment]) -> Result<(), String> {
    if media.len() > MAX_MEDIA_COUNT {
        return Err(format!(
            "too many media attachments: {} (max {MAX_MEDIA_COUNT})",
            media.len()
        ));
    }
    let mut seen = HashSet::with_capacity(media.len());
    for item in media {
        validate_media_attachment(item)?;
        if !seen.insert(item.hash.as_str()) {
            return Err(format!("duplicate media hash {}", short_id(&item.hash)));
        }
    }
    Ok(())
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
    if profile
        .avatar_hash
        .as_ref()
        .is_some_and(|h| h.len() > MAX_BLOB_HASH_LEN)
    {
        return Err(format!(
            "avatar hash too long (max {MAX_BLOB_HASH_LEN} bytes)"
        ));
    }
    if profile
        .avatar_ticket
        .as_ref()
        .is_some_and(|t| t.len() > MAX_BLOB_TICKET_LEN)
    {
        return Err(format!(
            "avatar ticket too long (max {MAX_BLOB_TICKET_LEN} bytes)"
        ));
    }
    if profile.updated_at > now_millis() + MAX_TIMESTAMP_DRIFT_MS {
        return Err("profile updated_at is too far in the future".to_string());
    }
    Ok(())
}

pub fn validate_interaction(interaction: &Interaction) -> Result<(), String> {
    if !is_valid_id(&interaction.id) {
        return Err("invalid interaction id".to_string());
    }
    if !is_valid_pubkey(&interaction.author) {
        return Err("invalid interaction author".to_string());
    }
    if !is_valid_id(&interaction.target_post_id) {
        return Err("invalid interaction target post id".to_string());
    }
    if !is_valid_pubkey(&interaction.target_author) {
        return Err("invalid interaction target author".to_string());
    }
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
    if !is_valid_pubkey(&post.author) {
        return Err("invalid post author".to_string());
    }
    if post.content.len() > MAX_POST_CONTENT_LEN {
        return Err(format!(
            "post content too long: {} bytes (max {})",
            post.content.len(),
            MAX_POST_CONTENT_LEN
        ));
    }
    validate_media_list(&post.media)?;
    if post.reply_to.as_deref().is_some_and(|id| !is_valid_id(id)) {
        return Err("invalid reply_to id".to_string());
    }
    if post
        .reply_to_author
        .as_deref()
        .is_some_and(|pk| !is_valid_pubkey(pk))
    {
        return Err("invalid reply_to_author".to_string());
    }
    if post.quote_of.as_deref().is_some_and(|id| !is_valid_id(id)) {
        return Err("invalid quote_of id".to_string());
    }
    if post
        .quote_of_author
        .as_deref()
        .is_some_and(|pk| !is_valid_pubkey(pk))
    {
        return Err("invalid quote_of_author".to_string());
    }
    let now = now_millis();
    if post.timestamp > now + MAX_TIMESTAMP_DRIFT_MS {
        return Err(format!(
            "post timestamp {} is too far in the future (now: {})",
            post.timestamp, now
        ));
    }
    // The id is a hash of the other fields, so two authors cannot hold the same id.
    if post.id != compute_post_id(post) {
        return Err("post id does not match post content".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_stops_on_char_boundary() {
        assert_eq!(short_id("aaaaaaa\u{20ac}bcd"), "aaaaaaa\u{20ac}");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
        let cjk = "\u{65e5}".repeat(30);
        assert_eq!(truncate_chars(&cjk, 77), cjk);
        assert_eq!(truncate_chars(&cjk, 2).chars().count(), 2);
    }

    #[test]
    fn id_and_pubkey_format_checks() {
        assert!(is_valid_id(&"0".repeat(32)));
        assert!(!is_valid_id(&"0".repeat(31)));
        assert!(!is_valid_id(&"G".repeat(32)));
        assert!(!is_valid_id(&"A".repeat(32)));
        let key = iroh::SecretKey::from_bytes(&[7u8; 32]).public().to_string();
        assert!(is_valid_pubkey(&key));
        assert!(!is_valid_pubkey(&key.to_uppercase()));
        assert!(!is_valid_pubkey("aaaaaaa\u{20ac}"));
    }
}
