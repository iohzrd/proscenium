//! QUIC transport limits shared by the client and server endpoints.

use iroh::endpoint::{QuicTransportConfig, VarInt};

/// Bytes a peer may send on one connection, over all streams, before the receiver reads
/// them. Bounds the unread data buffered per connection.
pub const CONNECTION_RECEIVE_WINDOW: u32 = 4 * 1024 * 1024;
/// Bytes a peer may send on one stream before the receiver reads them.
pub const STREAM_RECEIVE_WINDOW: u32 = 1024 * 1024;
/// Bidirectional streams a peer may have open on one connection. The protocols of this
/// application open at most two per connection; iroh-blobs opens one per request.
pub const MAX_BIDI_STREAMS: u32 = 32;
/// Unidirectional streams a peer may have open on one connection. iroh-gossip opens one
/// per topic that the two endpoints share and closes the connection when an open waits
/// 15 s at this limit, so two nodes that share more topics than this, such as a client
/// following many registered users and the discovery server, would reconnect
/// repeatedly. Stage forwards one per speaker. Received data counts toward
/// `CONNECTION_RECEIVE_WINDOW`. The crates.io release of iroh-gossip 0.102.0 sizes a frame
/// buffer to the declared frame length before the frame's bytes arrive; the
/// `[patch.crates-io]` entry in Cargo.toml uses a build that grows it with the bytes.
pub const MAX_UNI_STREAMS: u32 = 1024;

/// iroh's default transport configuration with the limits above.
pub fn quic_transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .receive_window(VarInt::from_u32(CONNECTION_RECEIVE_WINDOW))
        .stream_receive_window(VarInt::from_u32(STREAM_RECEIVE_WINDOW))
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_BIDI_STREAMS))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_UNI_STREAMS))
        .build()
}
