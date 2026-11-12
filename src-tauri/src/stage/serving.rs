//! Admission of audio connections, relay assignment and the host's `RoomState` broadcast.

use super::fanout::Fanout;
use super::pipeline::{self, ConnKind, IncomingAudio};
use super::relay::{RelayConfig, run_relay_upstream};
use super::state::{
    ActiveStage, HostState, LISTENER_CAPACITY, ListenerConn, MAX_FAILED_UPSTREAMS,
    MAX_LISTENER_CONNS_PER_IDENTITY, MixedStreamServer, SpeakerConn,
};
use super::{CLOSE_BANNED, CLOSE_FULL, CLOSE_NORMAL, CLOSE_REJECTED, CLOSE_REPLACED, StageCommand};
use crate::audio::PCM_CHANNEL_CAPACITY;
use crate::error::AppError;
use proscenium_types::{
    MAX_RELAY_ASSIGNMENT_LISTENERS, MAX_STAGE_SPEAKERS, StageControl, StageRole, now_millis,
    short_id,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;

/// The host broadcasts a `RoomState` at least this often.
const ROOM_STATE_INTERVAL_MS: u64 = 10_000;

/// True if `master_pubkey` has a listener connection to `serving` or a speaker
/// connection to `host`.
pub(super) fn has_audio_connection(
    serving: Option<&MixedStreamServer>,
    host: Option<&HostState>,
    master_pubkey: &str,
) -> bool {
    let listening =
        serving.is_some_and(|s| s.conns.values().any(|c| c.master_pubkey == master_pubkey));
    let speaking = host.is_some_and(|h| {
        h.speaker_conns
            .values()
            .any(|c| c.master_pubkey == master_pubkey)
    });
    listening || speaking
}

impl ActiveStage {
    /// Broadcast a control message signed by this node. A send failure is logged.
    pub(super) async fn broadcast(&self, control: StageControl) {
        if let Err(e) = self.control_plane.broadcast(control).await {
            log::warn!("[stage] failed to broadcast control: {e}");
        }
    }

    // ---- Connection admission --------------------------------------------

    /// Serve an incoming audio connection whose hello was verified, or close it.
    pub(super) async fn admit(&mut self, incoming: IncomingAudio, conn_id: u64) {
        if self.roles.is_banned(&incoming.master_pubkey) {
            log::info!(
                "[stage] rejected connection of banned identity {}",
                short_id(&incoming.master_pubkey)
            );
            incoming.conn.close(CLOSE_BANNED.into(), b"banned");
            return;
        }
        match incoming.kind {
            ConnKind::Listener => self.admit_listener(incoming, conn_id),
            ConnKind::Speaker => self.admit_speaker(incoming, conn_id).await,
        }
    }

    /// Serve the mixed stream to a listener connection.
    /// One connection per NodeId (a new one replaces the old one), at most
    /// `MAX_LISTENER_CONNS_PER_IDENTITY` per master pubkey, at most `capacity` in total.
    /// An identity without a speaking role may not use the last `reserved_for_roles`
    /// entries, which are kept for the upstream connections of relays.
    fn admit_listener(&mut self, incoming: IncomingAudio, conn_id: u64) {
        let IncomingAudio {
            conn,
            master_pubkey,
            node_id,
            send,
            ..
        } = incoming;
        let Some(server) = self.serving.as_mut() else {
            conn.close(CLOSE_REJECTED.into(), b"not serving");
            return;
        };

        if let Some(old) = server.conns.remove(&node_id) {
            old.cancel.cancel();
            old.conn.close(CLOSE_REPLACED.into(), b"replaced");
        }
        let same_identity = server
            .conns
            .values()
            .filter(|c| c.master_pubkey == master_pubkey)
            .count();
        let limit = if self.roles.role_of(&master_pubkey) == StageRole::Listener {
            server.capacity - server.reserved_for_roles
        } else {
            server.capacity
        };
        if same_identity >= MAX_LISTENER_CONNS_PER_IDENTITY || server.conns.len() >= limit {
            log::info!(
                "[stage] rejected listener {}: no free slot",
                short_id(&node_id)
            );
            conn.close(CLOSE_FULL.into(), b"full");
            return;
        }

        log::info!("[stage] listener connected: {}", short_id(&node_id));
        let cancel = self.cancel.child_token();
        let subscriber = server.fanout.add_subscriber(send, cancel.clone());
        server.conns.insert(
            node_id.clone(),
            ListenerConn {
                conn_id,
                master_pubkey,
                conn: conn.clone(),
                cancel,
            },
        );

        // Free the slot when the stream ends.
        let cmd_tx = self.rt.cmd_tx.clone();
        tokio::spawn(async move {
            let _ = subscriber.await;
            conn.close(CLOSE_NORMAL.into(), b"stream ended");
            cmd_tx
                .send(StageCommand::ListenerConnClosed { node_id, conn_id })
                .await;
        });
    }

    /// Host only: accept a speaker connection if the host assigned a speaking role to
    /// the identity. One connection per NodeId and per master pubkey; a new one replaces
    /// the old one.
    async fn admit_speaker(&mut self, incoming: IncomingAudio, conn_id: u64) {
        let IncomingAudio {
            conn,
            master_pubkey,
            node_id,
            send,
            recv,
            ..
        } = incoming;
        let Some(host) = self.host.as_mut() else {
            conn.close(CLOSE_REJECTED.into(), b"not the host");
            return;
        };
        if !matches!(
            self.roles.role_of(&master_pubkey),
            StageRole::Speaker | StageRole::CoHost
        ) {
            log::info!(
                "[stage-host] rejected speaker connection of {}: no speaking role",
                short_id(&master_pubkey)
            );
            conn.close(CLOSE_REJECTED.into(), b"not a speaker");
            return;
        }

        let replaced: Vec<String> = host
            .speaker_conns
            .iter()
            .filter(|(id, c)| **id == node_id || c.master_pubkey == master_pubkey)
            .map(|(id, _)| id.clone())
            .collect();
        for id in replaced {
            if let Some(old) = host.speaker_conns.remove(&id) {
                old.cancel.cancel();
                old.conn.close(CLOSE_REPLACED.into(), b"replaced");
            }
        }
        if host.speaker_conns.len() >= MAX_STAGE_SPEAKERS {
            conn.close(CLOSE_FULL.into(), b"full");
            return;
        }

        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<f32>>(PCM_CHANNEL_CAPACITY);
        if host
            .mixer
            .add_speaker(conn_id, node_id.clone(), pcm_rx)
            .await
            .is_err()
        {
            conn.close(CLOSE_REJECTED.into(), b"mixer stopped");
            return;
        }
        log::info!("[stage-host] speaker connected: {}", short_id(&node_id));
        pipeline::spawn_speaker_accepted(send);

        let cancel = self.cancel.child_token();
        let fanout = Arc::new(Fanout::new());
        let muted = Arc::new(AtomicBool::new(self.roles.is_host_muted(&master_pubkey)));

        // Start receiving first, then open the forwarding streams. Each stream is
        // opened by its own task with a timeout, so a peer that grants no stream
        // credit delays nobody else.
        tokio::spawn(pipeline::speaker_recv_loop(
            recv,
            node_id.clone(),
            conn_id,
            pcm_tx,
            fanout.clone(),
            muted.clone(),
            cancel.clone(),
            self.rt.cmd_tx.clone(),
        ));
        for existing in host.speaker_conns.values() {
            pipeline::spawn_forward(existing.fanout.clone(), conn.clone(), cancel.clone());
            pipeline::spawn_forward(
                fanout.clone(),
                existing.conn.clone(),
                existing.cancel.clone(),
            );
        }
        pipeline::spawn_forward(host.host_sfu_fanout.clone(), conn.clone(), cancel.clone());

        host.speaker_conns.insert(
            node_id,
            SpeakerConn {
                conn_id,
                master_pubkey,
                conn,
                cancel,
                fanout,
                muted,
            },
        );
    }

    /// Remove the entry of a listener connection whose stream ended. `conn_id` must
    /// match, so the end of a replaced connection does not remove its replacement.
    pub(super) fn listener_conn_closed(&mut self, node_id: &str, conn_id: u64) {
        if let Some(server) = self.serving.as_mut()
            && server
                .conns
                .get(node_id)
                .is_some_and(|c| c.conn_id == conn_id)
        {
            server.conns.remove(node_id);
            log::debug!("[stage] listener disconnected: {}", short_id(node_id));
        }
    }

    /// Remove the entry of a speaker connection whose receive loop ended and close the
    /// connection. `conn_id` must match.
    pub(super) fn speaker_conn_closed(&mut self, node_id: &str, conn_id: u64) {
        if let Some(host) = self.host.as_mut()
            && host
                .speaker_conns
                .get(node_id)
                .is_some_and(|c| c.conn_id == conn_id)
            && let Some(entry) = host.speaker_conns.remove(node_id)
        {
            entry.cancel.cancel();
            entry.conn.close(CLOSE_NORMAL.into(), b"stream ended");
            log::info!("[stage-host] speaker disconnected: {}", short_id(node_id));
        }
    }

    /// Close every audio connection of `master_pubkey` with the given close code.
    pub(super) fn close_connections_of(
        &mut self,
        master_pubkey: &str,
        code: u32,
        reason: &'static [u8],
    ) {
        if let Some(server) = self.serving.as_mut() {
            server.close_identity(master_pubkey, code, reason);
        }
        self.close_speaker_connections_of(master_pubkey, code, reason);
    }

    /// Host only: close the speaker connections of `master_pubkey`.
    pub(super) fn close_speaker_connections_of(
        &mut self,
        master_pubkey: &str,
        code: u32,
        reason: &'static [u8],
    ) {
        if let Some(host) = self.host.as_mut() {
            host.speaker_conns.retain(|_, entry| {
                if entry.master_pubkey != master_pubkey {
                    return true;
                }
                entry.cancel.cancel();
                entry.conn.close(code.into(), reason);
                false
            });
        }
    }

    /// True if `master_pubkey` has an audio connection to this node.
    pub(super) fn has_connection(&self, master_pubkey: &str) -> bool {
        has_audio_connection(self.serving.as_ref(), self.host.as_ref(), master_pubkey)
    }

    // ---- Listener upstream ------------------------------------------------

    /// Record that the listener pipeline stopped using relay `node_id`.
    /// Later assignments to that relay are ignored.
    pub(super) fn upstream_failed(&mut self, node_id: String) {
        if self.listener_upstream_id.as_deref() == Some(node_id.as_str()) {
            self.listener_upstream_id = Some(self.host_node_id.clone());
        }
        if self.failed_upstreams.len() < MAX_FAILED_UPSTREAMS {
            self.failed_upstreams.insert(node_id);
        }
    }

    // ---- Relay ------------------------------------------------------------

    /// Offer this node as a relay: start reading the mixed stream from the host, accept
    /// listener connections and broadcast `RelayVolunteer`. Requires a speaking role.
    /// The connection to the host is made by a background task.
    pub(super) async fn volunteer_as_relay(&mut self, capacity: u32) -> Result<(), AppError> {
        if self.host.is_some() {
            return Err(AppError::Other(
                "cannot volunteer as relay: this node is the host".into(),
            ));
        }
        if !matches!(self.my_role, StageRole::Speaker | StageRole::CoHost) {
            return Err(AppError::Other(
                "cannot volunteer as relay: role must be Speaker or CoHost".into(),
            ));
        }
        let capacity = usize::try_from(capacity)
            .unwrap_or(LISTENER_CAPACITY)
            .clamp(1, LISTENER_CAPACITY);

        match self.serving.as_mut() {
            Some(server) => server.capacity = capacity,
            None => {
                let fanout = Arc::new(Fanout::new());
                let cancel = self.cancel.child_token();
                tokio::spawn(run_relay_upstream(RelayConfig {
                    endpoint: self.rt.endpoint.clone(),
                    host_node_id: self.host_node_id.clone(),
                    signer: self.signer.clone(),
                    fanout: fanout.clone(),
                    cancel: cancel.clone(),
                }));
                let mut server = MixedStreamServer::new(fanout, capacity, 0);
                server.upstream_cancel = Some(cancel);
                self.serving = Some(server);
            }
        }

        self.broadcast(StageControl::RelayVolunteer {
            capacity: u32::try_from(capacity).unwrap_or(u32::MAX),
        })
        .await;
        log::info!("[stage] volunteered as relay (capacity {capacity})");
        Ok(())
    }

    /// Stop relaying: close the downstream connections and the upstream task.
    /// No effect on the host.
    pub(super) fn stop_relay(&mut self) {
        if self.host.is_some() {
            return;
        }
        if let Some(mut server) = self.serving.take() {
            server.shutdown(b"relay stopped");
            log::info!("[stage] stopped relaying");
        }
    }

    /// Host only: register the relay offered by `actor` and assign it the listeners
    /// that have no source. The relay's NodeId is the one of `actor`'s speaker
    /// connection, which QUIC authenticated; an offer from an identity without a
    /// speaking role or without a speaker connection is ignored.
    pub(super) async fn handle_relay_volunteer(&mut self, actor: &str, capacity: u32) {
        if !matches!(
            self.roles.role_of(actor),
            StageRole::Speaker | StageRole::CoHost
        ) {
            log::debug!(
                "[stage-host] ignoring relay offer of {}: no speaking role",
                short_id(actor)
            );
            return;
        }
        let Some(host) = self.host.as_mut() else {
            return;
        };
        let Some(relay_node_id) = host
            .speaker_conns
            .iter()
            .find(|(_, c)| c.master_pubkey == actor)
            .map(|(node_id, _)| node_id.clone())
        else {
            log::debug!(
                "[stage-host] ignoring relay offer of {}: no speaker connection",
                short_id(actor)
            );
            return;
        };
        let capacity = usize::try_from(capacity)
            .unwrap_or(LISTENER_CAPACITY)
            .clamp(1, LISTENER_CAPACITY);
        if !host.topology.add_relay(relay_node_id.clone(), capacity) {
            return;
        }
        host.relay_owners
            .insert(actor.to_string(), relay_node_id.clone());
        log::info!(
            "[stage-host] relay {} registered (capacity {capacity})",
            short_id(&relay_node_id)
        );

        let unassigned: Vec<String> = self
            .participants
            .keys()
            .filter(|pk| {
                self.roles.role_of(pk) == StageRole::Listener
                    && host.topology.assignment(pk).is_none()
            })
            .cloned()
            .collect();
        let assignments = self.assign_listeners(unassigned);
        self.broadcast_assignments(assignments).await;
    }

    /// Host only: assign each listener to a source. Returns the assigned listeners
    /// grouped by source; a listener no source has capacity for is omitted.
    fn assign_listeners(&mut self, listeners: Vec<String>) -> HashMap<String, Vec<String>> {
        let mut by_source: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(host) = self.host.as_mut() {
            for pubkey in listeners {
                if let Some(source) = host.topology.assign_listener(&pubkey) {
                    by_source.entry(source).or_default().push(pubkey);
                }
            }
        }
        by_source
    }

    /// Host only: broadcast one `RelayAssignment` per source and per
    /// `MAX_RELAY_ASSIGNMENT_LISTENERS` listeners.
    async fn broadcast_assignments(&self, assignments: HashMap<String, Vec<String>>) {
        for (source_node_id, listeners) in assignments {
            for chunk in listeners.chunks(MAX_RELAY_ASSIGNMENT_LISTENERS) {
                self.broadcast(StageControl::RelayAssignment {
                    source_node_id: source_node_id.clone(),
                    listener_pubkeys: chunk.to_vec(),
                })
                .await;
            }
        }
    }

    /// Host only: assign a new listener to a source. Broadcasts the assignment if the
    /// source is a relay; a listener dials the host without being told.
    pub(super) async fn assign_new_listener(&mut self, pubkey: &str) {
        let host_node_id = self.host_node_id.clone();
        let mut assignments = self.assign_listeners(vec![pubkey.to_string()]);
        assignments.remove(&host_node_id);
        self.broadcast_assignments(assignments).await;
    }

    /// Host only: remove the relay of `master_pubkey`, if it registered one, and assign
    /// its listeners to the remaining sources. Listeners moved to the host are told too,
    /// because they are connected to the removed relay.
    pub(super) async fn remove_relay_of(&mut self, master_pubkey: &str) {
        let Some(host) = self.host.as_mut() else {
            return;
        };
        let Some(relay_node_id) = host.relay_owners.remove(master_pubkey) else {
            return;
        };
        let displaced = host.topology.remove_relay(&relay_node_id);
        log::info!(
            "[stage-host] relay {} removed, {} listeners reassigned",
            short_id(&relay_node_id),
            displaced.len()
        );
        let assignments = self.assign_listeners(displaced);
        self.broadcast_assignments(assignments).await;
    }

    // ---- RoomState --------------------------------------------------------

    /// Host only: increment the role version and broadcast the new `RoomState`.
    pub(super) async fn publish_roles(&mut self) {
        if self.host.is_none() {
            return;
        }
        self.roles.version += 1;
        self.broadcast_room_state().await;
    }

    /// Host only: broadcast the current `RoomState`.
    async fn broadcast_room_state(&mut self) {
        let Some(host) = self.host.as_mut() else {
            return;
        };
        host.room_state_pending = false;
        host.last_room_state_ms = now_millis();
        let snapshot = self.roles.snapshot();
        self.broadcast(StageControl::RoomState(snapshot)).await;
    }

    /// Host only: broadcast the `RoomState` if a participant joined since the last
    /// broadcast or `ROOM_STATE_INTERVAL_MS` passed.
    pub(super) async fn host_tick(&mut self) {
        let Some(host) = self.host.as_ref() else {
            return;
        };
        let due = now_millis().saturating_sub(host.last_room_state_ms) >= ROOM_STATE_INTERVAL_MS;
        if host.room_state_pending || due {
            self.broadcast_room_state().await;
        }
    }
}
