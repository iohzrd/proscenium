use std::collections::HashMap;

/// Where a listener should connect to receive the mixed audio stream.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioAssignment {
    /// The transport NodeId of the node that serves the mixed stream.
    pub source_endpoint_id: String,
}

/// Source type tracked by the topology manager.
#[derive(Debug, Clone)]
enum SourceKind {
    Host,
    Relay,
}

#[derive(Debug, Clone)]
struct Source {
    kind: SourceKind,
    endpoint_id: String,
    capacity: u32,
    assigned: u32,
}

/// Host-side topology manager.
///
/// Decides which source (host direct or relay) each listener should connect
/// to in order to receive the single mixed audio stream.
///
/// Phase 3 implementation: supports host-direct and volunteer relays.
/// Relay tree (multi-tier) is Phase 5.
pub struct TopologyManager {
    sources: Vec<Source>,
    /// pubkey -> assigned source endpoint_id
    assignments: HashMap<String, String>,
}

impl TopologyManager {
    /// Create a new topology manager. `host_endpoint_id` is the host's transport NodeId.
    /// `host_capacity` is how many direct listeners the host will serve (default 15).
    pub fn new(host_endpoint_id: String, host_capacity: u32) -> Self {
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

    /// Register a volunteer relay with self-reported capacity.
    pub fn add_relay(&mut self, endpoint_id: String, capacity: u32) {
        // Avoid duplicates
        if !self.sources.iter().any(|s| s.endpoint_id == endpoint_id) {
            self.sources.push(Source {
                kind: SourceKind::Relay,
                endpoint_id,
                capacity,
                assigned: 0,
            });
        }
    }

    /// Assign (or reassign) a listener to a source. Returns the `AudioAssignment`.
    /// Returns `None` if all sources are at capacity.
    ///
    /// Any existing assignment for this pubkey is always released first so that
    /// reconnecting listeners never hold two capacity slots simultaneously.
    pub fn assign_listener(&mut self, pubkey: &str) -> Option<AudioAssignment> {
        // Always release any existing slot before (re-)assigning. This ensures
        // that a listener who reconnects without having called release_listener
        // (e.g. after a network partition) does not permanently inflate the
        // assigned count.
        if let Some(old) = self.assignments.remove(pubkey)
            && let Some(s) = self.sources.iter_mut().find(|s| s.endpoint_id == old)
        {
            s.assigned = s.assigned.saturating_sub(1);
        }

        // Priority: Host first (most reliable), then relays by available capacity.
        let source = self
            .sources
            .iter_mut()
            .find(|s| matches!(s.kind, SourceKind::Host) && s.assigned < s.capacity);

        let source = if source.is_none() {
            self.sources
                .iter_mut()
                .filter(|s| s.assigned < s.capacity)
                .max_by_key(|s| s.capacity - s.assigned)
        } else {
            source
        };

        let source = source?;
        source.assigned += 1;
        let endpoint_id = source.endpoint_id.clone();
        self.assignments
            .insert(pubkey.to_string(), endpoint_id.clone());

        Some(AudioAssignment {
            source_endpoint_id: endpoint_id,
        })
    }
}
