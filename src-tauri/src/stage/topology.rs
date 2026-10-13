use std::collections::HashMap;

/// Largest number of relays the host tracks.
const MAX_RELAYS: usize = 16;

/// Source type tracked by the topology manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Host,
    Relay,
}

#[derive(Debug, Clone)]
struct Source {
    kind: SourceKind,
    endpoint_id: String,
    capacity: usize,
    assigned: usize,
}

/// Host-side topology manager.
///
/// Assigns each listener the source (the host or a relay) it receives the single mixed
/// audio stream from. A listener keeps its source until it leaves or the source is
/// removed.
pub struct TopologyManager {
    sources: Vec<Source>,
    /// Listener master pubkey -> assigned source endpoint_id
    assignments: HashMap<String, String>,
}

impl TopologyManager {
    /// Create a new topology manager. `host_endpoint_id` is the host's transport NodeId.
    /// `host_capacity` is how many direct listeners the host will serve.
    pub fn new(host_endpoint_id: String, host_capacity: usize) -> Self {
        Self {
            sources: vec![Source {
                kind: SourceKind::Host,
                endpoint_id: host_endpoint_id,
                capacity: host_capacity,
                assigned: 0,
            }],
            assignments: HashMap::new(),
        }
    }

    /// Register a relay, or update the capacity of a registered one.
    /// Returns false if the relay limit is reached or `endpoint_id` is the host.
    pub fn add_relay(&mut self, endpoint_id: String, capacity: usize) -> bool {
        if let Some(source) = self
            .sources
            .iter_mut()
            .find(|s| s.endpoint_id == endpoint_id)
        {
            if source.kind == SourceKind::Host {
                return false;
            }
            source.capacity = capacity;
            return true;
        }
        if self.sources.len() > MAX_RELAYS {
            return false;
        }
        self.sources.push(Source {
            kind: SourceKind::Relay,
            endpoint_id,
            capacity,
            assigned: 0,
        });
        true
    }

    /// Remove a relay. Returns the pubkeys of the listeners that were assigned to it;
    /// they have no assignment afterwards.
    pub fn remove_relay(&mut self, endpoint_id: &str) -> Vec<String> {
        let before = self.sources.len();
        self.sources
            .retain(|s| s.kind == SourceKind::Host || s.endpoint_id != endpoint_id);
        if self.sources.len() == before {
            return Vec::new();
        }
        let displaced: Vec<String> = self
            .assignments
            .iter()
            .filter(|(_, source)| source.as_str() == endpoint_id)
            .map(|(pubkey, _)| pubkey.clone())
            .collect();
        for pubkey in &displaced {
            self.assignments.remove(pubkey);
        }
        displaced
    }

    /// Source assigned to a listener, if any.
    pub fn assignment(&self, pubkey: &str) -> Option<&str> {
        self.assignments.get(pubkey).map(String::as_str)
    }

    /// Assign a listener to a source and return the source's endpoint id.
    /// A listener that already has an assignment keeps it.
    /// Returns `None` if all sources are at capacity.
    pub fn assign_listener(&mut self, pubkey: &str) -> Option<String> {
        if let Some(existing) = self.assignments.get(pubkey) {
            return Some(existing.clone());
        }

        // Priority: the host while it has capacity, then the relay with the most
        // free capacity.
        let host = self
            .sources
            .iter_mut()
            .find(|s| s.kind == SourceKind::Host && s.assigned < s.capacity);
        let source = match host {
            Some(host) => host,
            None => self
                .sources
                .iter_mut()
                .filter(|s| s.assigned < s.capacity)
                .max_by_key(|s| s.capacity - s.assigned)?,
        };

        source.assigned += 1;
        let endpoint_id = source.endpoint_id.clone();
        self.assignments
            .insert(pubkey.to_string(), endpoint_id.clone());
        Some(endpoint_id)
    }

    /// Release the capacity slot of a listener that left or stopped being a listener.
    pub fn release_listener(&mut self, pubkey: &str) {
        if let Some(endpoint_id) = self.assignments.remove(pubkey)
            && let Some(source) = self
                .sources
                .iter_mut()
                .find(|s| s.endpoint_id == endpoint_id)
        {
            source.assigned = source.assigned.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_first_then_relay_and_none_when_full() {
        let mut topology = TopologyManager::new("host".to_string(), 2);
        assert_eq!(topology.assign_listener("a").as_deref(), Some("host"));
        assert_eq!(topology.assign_listener("b").as_deref(), Some("host"));
        assert_eq!(topology.assign_listener("c"), None);

        assert!(topology.add_relay("relay".to_string(), 1));
        assert_eq!(topology.assign_listener("c").as_deref(), Some("relay"));
        assert_eq!(topology.assign_listener("d"), None);
        // An assigned listener keeps its source and uses one slot.
        assert_eq!(topology.assign_listener("a").as_deref(), Some("host"));
        assert_eq!(topology.assign_listener("c").as_deref(), Some("relay"));
    }

    #[test]
    fn release_frees_the_slot() {
        let mut topology = TopologyManager::new("host".to_string(), 1);
        assert_eq!(topology.assign_listener("a").as_deref(), Some("host"));
        assert_eq!(topology.assign_listener("b"), None);
        topology.release_listener("a");
        assert_eq!(topology.assignment("a"), None);
        assert_eq!(topology.assign_listener("b").as_deref(), Some("host"));
        // Releasing an unknown listener changes nothing.
        topology.release_listener("unknown");
        assert_eq!(topology.assign_listener("c"), None);
    }

    #[test]
    fn remove_relay_returns_displaced_listeners() {
        let mut topology = TopologyManager::new("host".to_string(), 1);
        topology.assign_listener("a");
        topology.add_relay("relay".to_string(), 5);
        topology.assign_listener("b");
        topology.assign_listener("c");

        let mut displaced = topology.remove_relay("relay");
        displaced.sort();
        assert_eq!(displaced, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(topology.assignment("b"), None);
        assert_eq!(topology.assign_listener("b"), None);
        assert!(topology.remove_relay("relay").is_empty());
        // The host cannot be removed or registered as a relay.
        assert!(topology.remove_relay("host").is_empty());
        assert!(!topology.add_relay("host".to_string(), 9));
        assert_eq!(topology.assignment("a"), Some("host"));
    }

    #[test]
    fn relay_count_is_bounded() {
        let mut topology = TopologyManager::new("host".to_string(), 0);
        for i in 0..MAX_RELAYS {
            assert!(topology.add_relay(format!("relay-{i}"), 1));
        }
        assert!(!topology.add_relay("one-more".to_string(), 1));
        // Updating a registered relay is still accepted.
        assert!(topology.add_relay("relay-0".to_string(), 3));
    }
}
