//! QUIC transport settings shared by the client and server endpoints.

/// Streams a peer may have open on one gossip connection (iroh-gossip
/// `Builder::max_peer_streams`, set on the client and the server). iroh-gossip opens one per
/// shared topic, so a client that follows many users registered on the discovery server
/// needs more than the default 100.
pub const GOSSIP_MAX_PEER_STREAMS: u32 = 1024;
