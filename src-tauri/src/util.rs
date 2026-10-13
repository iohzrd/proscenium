pub fn generate_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("failed to generate random bytes");
    let (a, b) = bytes.split_at(8);
    format!(
        "{:016x}{:016x}",
        u64::from_le_bytes(a.try_into().unwrap()),
        u64::from_le_bytes(b.try_into().unwrap())
    )
}

/// Maximum number of characters of a message shown in the conversation list.
const MESSAGE_PREVIEW_CHARS: usize = 80;

/// Conversation-list preview of a message: the whole text if it has at most
/// `MESSAGE_PREVIEW_CHARS` characters, otherwise the first 77 characters followed by "...".
/// Truncation is by character, never inside a UTF-8 sequence.
pub fn message_preview(content: &str) -> String {
    if content.chars().count() > MESSAGE_PREVIEW_CHARS {
        format!(
            "{}...",
            proscenium_types::truncate_chars(content, MESSAGE_PREVIEW_CHARS - 3)
        )
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_preview_truncates_on_char_boundaries() {
        assert_eq!(message_preview("short"), "short");
        let cjk = "\u{65e5}".repeat(100);
        let preview = message_preview(&cjk);
        assert_eq!(preview.chars().count(), 80);
        assert!(preview.ends_with("..."));
        let cyrillic = "\u{43f}".repeat(45);
        assert_eq!(message_preview(&cyrillic), cyrillic);
    }
    use std::collections::HashSet;

    #[test]
    fn generate_id_is_32_hex_chars() {
        let id = generate_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_id_is_unique() {
        let ids: HashSet<String> = (0..100).map(|_| generate_id()).collect();
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn generate_id_is_lowercase() {
        let id = generate_id();
        assert_eq!(id, id.to_lowercase());
    }
}
