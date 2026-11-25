//! QUIC transport limits shared by the client and server endpoints.

use iroh::EndpointId;
use iroh::endpoint::{Connection, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// Streams a peer may have open on one gossip connection (iroh-gossip
/// `Builder::max_peer_streams`, set on the client and the server). iroh-gossip opens one per
/// shared topic, so a client that follows many users registered on the discovery server
/// needs more than the default 100.
pub const GOSSIP_MAX_PEER_STREAMS: u32 = 1024;

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
