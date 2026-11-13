use iroh::EndpointId;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// One counted item (a connection or a served sync) of a remote NodeId; decrements the
/// count when dropped.
pub(crate) struct NodeSlot {
    counts: Arc<Mutex<HashMap<EndpointId, usize>>>,
    node: EndpointId,
}

impl NodeSlot {
    /// Count one more item of `node` in `counts`. Returns `None` if `node` already has
    /// `limit` items.
    pub(crate) fn acquire(
        counts: &Arc<Mutex<HashMap<EndpointId, usize>>>,
        node: EndpointId,
        limit: usize,
    ) -> Option<Self> {
        let mut map = counts.lock().unwrap_or_else(|e| e.into_inner());
        let count = map.entry(node).or_insert(0);
        if *count >= limit {
            return None;
        }
        *count += 1;
        Some(Self {
            counts: counts.clone(),
            node,
        })
    }
}

impl Drop for NodeSlot {
    fn drop(&mut self) {
        let mut map = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = map.get_mut(&self.node) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.node);
            }
        }
    }
}

/// Admits at most `max` events per sliding `window`.
#[derive(Debug)]
pub(crate) struct RateWindow {
    max: usize,
    window: Duration,
    admitted: VecDeque<Instant>,
}

impl RateWindow {
    pub(crate) fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            admitted: VecDeque::with_capacity(max),
        }
    }

    /// Remove the events older than `window` at `now`. True if fewer than `max` remain.
    pub(crate) fn has_room(&mut self, now: Instant) -> bool {
        self.admitted
            .retain(|at| now.saturating_duration_since(*at) < self.window);
        self.admitted.len() < self.max
    }

    /// Record an event at `now` and return true if `has_room(now)`; else record nothing
    /// and return false.
    pub(crate) fn admit(&mut self, now: Instant) -> bool {
        if !self.has_room(now) {
            return false;
        }
        self.admitted.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_window_admits_max_events_per_window() {
        let mut window = RateWindow::new(3, Duration::from_secs(60));
        let start = Instant::now();
        for _ in 0..3 {
            assert!(window.admit(start));
        }
        assert!(!window.has_room(start));
        assert!(!window.admit(start + Duration::from_secs(59)));
        // After the window the earlier events no longer count.
        assert!(window.admit(start + Duration::from_secs(60)));
    }

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
