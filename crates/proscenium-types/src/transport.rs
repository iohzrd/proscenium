//! QUIC transport limits shared by the client and server endpoints.

use iroh::EndpointId;
use iroh::endpoint::{Connection, QuicTransportConfig, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

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
/// `CONNECTION_RECEIVE_WINDOW`. The crates.io release of iroh-gossip 0.102.0 reads every
/// stream at once and sizes each frame buffer to the declared frame length, so a connection
/// could hold this many frames of 64 KiB. The `[patch.crates-io]` entry in Cargo.toml uses a
/// build that grows a buffer with the received bytes and limits the frame storage of a
/// connection (`Builder::max_recv_buffer`).
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

/// Close code of a connection that `ConnectionLimit` refused.
pub const CONNECTION_LIMIT_CLOSE_CODE: u32 = 1;

/// Wraps a protocol handler whose `accept` hands the connection on and returns before the
/// connection closes, as iroh-gossip's does, and limits the connections it holds: at most
/// `max_total`, and at most `max_per_node` per remote NodeId. A connection is counted
/// until it closes. A connection beyond either limit is closed with
/// `CONNECTION_LIMIT_CLOSE_CODE` and does not reach the handler. Connections the local
/// node opens are not counted.
#[derive(Debug, Clone)]
pub struct ConnectionLimit<P> {
    inner: P,
    max_total: usize,
    max_per_node: usize,
    counts: Arc<Mutex<ConnectionCounts>>,
}

#[derive(Debug, Default)]
struct ConnectionCounts {
    total: usize,
    per_node: HashMap<EndpointId, usize>,
}

/// One counted connection; decrements the counts when dropped.
struct ConnectionSlot {
    counts: Arc<Mutex<ConnectionCounts>>,
    node: EndpointId,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.total = counts.total.saturating_sub(1);
        if let Some(count) = counts.per_node.get_mut(&self.node) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.per_node.remove(&self.node);
            }
        }
    }
}

impl<P> ConnectionLimit<P> {
    pub fn new(inner: P, max_total: usize, max_per_node: usize) -> Self {
        Self {
            inner,
            max_total,
            max_per_node,
            counts: Arc::default(),
        }
    }

    /// Count one connection of `node`; `None` if either limit is reached.
    fn acquire(&self, node: EndpointId) -> Option<ConnectionSlot> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        let per_node = counts.per_node.get(&node).copied().unwrap_or(0);
        if counts.total >= self.max_total || per_node >= self.max_per_node {
            return None;
        }
        counts.total += 1;
        counts.per_node.insert(node, per_node + 1);
        Some(ConnectionSlot {
            counts: self.counts.clone(),
            node,
        })
    }
}

impl<P: ProtocolHandler> ProtocolHandler for ConnectionLimit<P> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Some(_slot) = self.acquire(connection.remote_id()) else {
            connection.close(
                VarInt::from_u32(CONNECTION_LIMIT_CLOSE_CODE),
                b"connection limit reached",
            );
            return Ok(());
        };
        let held = connection.clone();
        self.inner.accept(connection).await?;
        held.closed().await;
        Ok(())
    }

    async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Handler;

    impl ProtocolHandler for Handler {
        async fn accept(&self, _connection: Connection) -> Result<(), AcceptError> {
            Ok(())
        }
    }

    fn node(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn connection_limit_counts_total_and_per_node_until_dropped() {
        let limit = ConnectionLimit::new(Handler, 3, 2);
        let a1 = limit.acquire(node(1)).unwrap();
        let _a2 = limit.acquire(node(1)).unwrap();
        assert!(limit.acquire(node(1)).is_none());
        let _b = limit.acquire(node(2)).unwrap();
        assert!(limit.acquire(node(3)).is_none());
        drop(a1);
        assert!(limit.acquire(node(3)).is_some());
        assert!(limit.acquire(node(1)).is_some());
    }
}
