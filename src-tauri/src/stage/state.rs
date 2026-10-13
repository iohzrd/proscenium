use crate::stage::command::ActorSender;
use crate::stage::control::{ControlPlane, ControlSigner};
use crate::stage::fanout::Fanout;
use crate::stage::mixer::MixerHandle;
use crate::stage::topology::TopologyManager;
use iroh::endpoint::Connection;
use iroh::{Endpoint, PublicKey};
use proscenium_types::{
    MAX_STAGE_BANNED, MAX_STAGE_PARTICIPANTS, MAX_STAGE_SPEAKERS, MAX_TIMESTAMP_DRIFT_MS,
    RoomState, StageRole, StageTicket,
};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64},
};
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

/// Listener connections one node serves. The topology manager assigns at most this many
/// listeners to the host.
pub(super) const LISTENER_CAPACITY: usize = 64;
/// Extra listener connections the host accepts above `LISTENER_CAPACITY`. Covers the
/// upstream connections of relays, which the topology manager does not count.
pub(super) const LISTENER_CAPACITY_RESERVE: usize = MAX_STAGE_SPEAKERS;
/// Listener connections accepted per master pubkey (one per device).
pub(super) const MAX_LISTENER_CONNS_PER_IDENTITY: usize = 4;
/// Relay NodeIds a listener stops using after they failed.
pub(super) const MAX_FAILED_UPSTREAMS: usize = 32;

/// Chat messages accepted per participant: burst size and milliseconds per refill.
const CHAT_BURST: u32 = 5;
const CHAT_REFILL_MS: u64 = 2_000;
/// Reactions accepted per participant: burst size and milliseconds per refill.
const REACTION_BURST: u32 = 10;
const REACTION_REFILL_MS: u64 = 500;

// ---- Role rules ---------------------------------------------------------

/// A moderation action applied by one participant to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Moderation {
    Promote,
    Demote,
    Mute,
    Kick,
    Ban,
}

/// Roles assigned by the host. On the host this is the authoritative copy; on every
/// other node it is the copy of the last host-signed `RoomState`, plus moderation
/// messages applied since. All pubkeys are master pubkeys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoomRoles {
    pub(super) host_pubkey: String,
    pub(super) version: u64,
    pub(super) co_hosts: HashSet<String>,
    pub(super) speakers: HashSet<String>,
    pub(super) host_muted: HashSet<String>,
    pub(super) banned: HashSet<String>,
}

impl RoomRoles {
    pub(super) fn new(host_pubkey: String) -> Self {
        Self {
            host_pubkey,
            version: 0,
            co_hosts: HashSet::new(),
            speakers: HashSet::new(),
            host_muted: HashSet::new(),
            banned: HashSet::new(),
        }
    }

    /// Role of `pubkey`. A pubkey the host did not assign a role to is a listener.
    pub(super) fn role_of(&self, pubkey: &str) -> StageRole {
        if pubkey == self.host_pubkey {
            StageRole::Host
        } else if self.co_hosts.contains(pubkey) {
            StageRole::CoHost
        } else if self.speakers.contains(pubkey) {
            StageRole::Speaker
        } else {
            StageRole::Listener
        }
    }

    pub(super) fn is_banned(&self, pubkey: &str) -> bool {
        self.banned.contains(pubkey)
    }

    pub(super) fn is_host_muted(&self, pubkey: &str) -> bool {
        self.host_muted.contains(pubkey)
    }

    /// Number of speakers and co-hosts, not counting the host.
    pub(super) fn speaker_count(&self) -> usize {
        self.co_hosts.len() + self.speakers.len()
    }

    /// True if `actor` may apply `action` to `target`.
    ///
    /// The host may apply every action to every other participant. A co-host may promote,
    /// demote, mute and kick participants that are neither the host nor a co-host.
    /// Nobody else may moderate, and nobody may moderate the host.
    pub(super) fn may_moderate(&self, actor: &str, target: &str, action: Moderation) -> bool {
        if target == self.host_pubkey {
            return false;
        }
        match self.role_of(actor) {
            StageRole::Host => true,
            StageRole::CoHost => {
                action != Moderation::Ban && self.role_of(target) != StageRole::CoHost
            }
            StageRole::Speaker | StageRole::Listener => false,
        }
    }

    /// Make `pubkey` a speaker. Returns false if it already has a speaking role, is
    /// banned, or the speaker limit is reached.
    pub(super) fn promote(&mut self, pubkey: &str) -> bool {
        if self.role_of(pubkey) != StageRole::Listener
            || self.is_banned(pubkey)
            || self.speaker_count() >= MAX_STAGE_SPEAKERS
        {
            return false;
        }
        self.speakers.insert(pubkey.to_string())
    }

    /// Make `pubkey` a listener and clear its host mute. Returns false if it had no
    /// speaking role.
    pub(super) fn demote(&mut self, pubkey: &str) -> bool {
        let was_speaker = self.speakers.remove(pubkey);
        let was_co_host = self.co_hosts.remove(pubkey);
        self.host_muted.remove(pubkey);
        was_speaker || was_co_host
    }

    /// Mark a speaker or co-host as muted by the host. Returns false if `pubkey` has no
    /// speaking role or is already muted.
    pub(super) fn mute(&mut self, pubkey: &str) -> bool {
        matches!(self.role_of(pubkey), StageRole::Speaker | StageRole::CoHost)
            && self.host_muted.insert(pubkey.to_string())
    }

    /// Remove every role of `pubkey`.
    pub(super) fn remove(&mut self, pubkey: &str) {
        self.demote(pubkey);
    }

    /// Remove every role of `pubkey` and add it to the banned set.
    /// Returns false if the banned set is full or `pubkey` is the host.
    pub(super) fn ban(&mut self, pubkey: &str) -> bool {
        if pubkey == self.host_pubkey {
            return false;
        }
        if !self.banned.contains(pubkey) && self.banned.len() >= MAX_STAGE_BANNED {
            return false;
        }
        self.remove(pubkey);
        self.banned.insert(pubkey.to_string());
        true
    }

    /// Build the snapshot the host signs. Lists are sorted.
    pub(super) fn snapshot(&self) -> RoomState {
        fn sorted(set: &HashSet<String>) -> Vec<String> {
            let mut list: Vec<String> = set.iter().cloned().collect();
            list.sort_unstable();
            list
        }
        RoomState {
            version: self.version,
            co_hosts: sorted(&self.co_hosts),
            speakers: sorted(&self.speakers),
            host_muted: sorted(&self.host_muted),
            banned: sorted(&self.banned),
        }
    }

    /// Replace the role sets with a host-signed snapshot.
    /// Returns `None` if `state.version` is lower than the version already applied,
    /// otherwise whether any set changed. Entries naming the host are ignored.
    pub(super) fn apply_snapshot(&mut self, state: &RoomState) -> Option<bool> {
        if state.version < self.version {
            return None;
        }
        let host = self.host_pubkey.clone();
        let to_set = |list: &[String]| -> HashSet<String> {
            list.iter().filter(|pk| **pk != host).cloned().collect()
        };
        let banned = to_set(&state.banned);
        let co_hosts: HashSet<String> = to_set(&state.co_hosts)
            .into_iter()
            .filter(|pk| !banned.contains(pk))
            .collect();
        let speakers: HashSet<String> = to_set(&state.speakers)
            .into_iter()
            .filter(|pk| !banned.contains(pk) && !co_hosts.contains(pk))
            .collect();
        let host_muted: HashSet<String> = to_set(&state.host_muted)
            .into_iter()
            .filter(|pk| co_hosts.contains(pk) || speakers.contains(pk))
            .collect();

        let changed = banned != self.banned
            || co_hosts != self.co_hosts
            || speakers != self.speakers
            || host_muted != self.host_muted;
        self.version = state.version;
        self.banned = banned;
        self.co_hosts = co_hosts;
        self.speakers = speakers;
        self.host_muted = host_muted;
        Some(changed)
    }
}

// ---- Signer table (replay rejection) ------------------------------------

struct SignerEntry {
    master_pubkey: String,
    /// Highest `SignedStageControl.timestamp` accepted from this signer.
    last_timestamp: u64,
}

/// Signing keys bound to a master pubkey by a verified `Presence` (or by the stage ticket,
/// for the host), with the highest message timestamp accepted from each.
#[derive(Default)]
pub(super) struct SignerTable {
    entries: HashMap<String, SignerEntry>,
}

impl SignerTable {
    /// Largest number of signing keys tracked.
    const MAX_ENTRIES: usize = 2 * MAX_STAGE_PARTICIPANTS;

    /// Bind `signing_pubkey` to `master_pubkey`. The caller verified the delegation.
    /// Returns false if the key is bound to another master pubkey or the table is full.
    pub(super) fn bind(&mut self, signing_pubkey: &str, master_pubkey: &str) -> bool {
        if let Some(entry) = self.entries.get(signing_pubkey) {
            return entry.master_pubkey == master_pubkey;
        }
        if self.entries.len() >= Self::MAX_ENTRIES {
            return false;
        }
        self.entries.insert(
            signing_pubkey.to_string(),
            SignerEntry {
                master_pubkey: master_pubkey.to_string(),
                last_timestamp: 0,
            },
        );
        true
    }

    /// Master pubkey bound to `signing_pubkey`, if any.
    pub(super) fn master_of(&self, signing_pubkey: &str) -> Option<&str> {
        self.entries
            .get(signing_pubkey)
            .map(|e| e.master_pubkey.as_str())
    }

    /// Record the timestamp of a message from `signing_pubkey`. Returns false for an
    /// unbound signer and for a timestamp that is not higher than the last one accepted
    /// from that signer (a replayed or reordered message).
    pub(super) fn accept_timestamp(&mut self, signing_pubkey: &str, timestamp: u64) -> bool {
        match self.entries.get_mut(signing_pubkey) {
            Some(entry) if timestamp > entry.last_timestamp => {
                entry.last_timestamp = timestamp;
                true
            }
            _ => false,
        }
    }

    /// Remove signers that `keep` rejects and whose last message is older than the
    /// accepted timestamp window. A replay of such a message fails the window check.
    pub(super) fn prune(&mut self, now_ms: u64, keep: impl Fn(&str) -> bool) {
        let cutoff = now_ms.saturating_sub(2 * MAX_TIMESTAMP_DRIFT_MS);
        self.entries
            .retain(|_, e| e.last_timestamp >= cutoff || keep(&e.master_pubkey));
    }
}

// ---- Rate limiting ------------------------------------------------------

/// Token bucket: `capacity` tokens, one token added every `refill_ms`.
pub(super) struct RateLimiter {
    tokens: u32,
    capacity: u32,
    refill_ms: u64,
    last_refill_ms: u64,
}

impl RateLimiter {
    pub(super) fn new(capacity: u32, refill_ms: u64, now_ms: u64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_ms: refill_ms.max(1),
            last_refill_ms: now_ms,
        }
    }

    /// Take one token. Returns false if none is available.
    pub(super) fn allow(&mut self, now_ms: u64) -> bool {
        let elapsed = now_ms.saturating_sub(self.last_refill_ms);
        let refill = elapsed / self.refill_ms;
        if refill > 0 {
            self.tokens = self.capacity.min(
                self.tokens
                    .saturating_add(u32::try_from(refill).unwrap_or(u32::MAX)),
            );
            self.last_refill_ms += refill * self.refill_ms;
        }
        if self.tokens == 0 {
            return false;
        }
        self.tokens -= 1;
        true
    }
}

// ---- Participant tracking -----------------------------------------------

/// Live participant entry, created by a verified `Presence` and keyed by master pubkey.
/// The role and host-mute state are read from `RoomRoles`.
pub(super) struct Participant {
    pub(super) pubkey: String,
    pub(super) hand_raised: bool,
    pub(super) self_muted: bool,
    pub(super) last_seen_ms: u64,
    pub(super) chat_limit: RateLimiter,
    pub(super) reaction_limit: RateLimiter,
}

impl Participant {
    pub(super) fn new(pubkey: String, now_ms: u64) -> Self {
        Self {
            pubkey,
            hand_raised: false,
            self_muted: false,
            last_seen_ms: now_ms,
            chat_limit: RateLimiter::new(CHAT_BURST, CHAT_REFILL_MS, now_ms),
            reaction_limit: RateLimiter::new(REACTION_BURST, REACTION_REFILL_MS, now_ms),
        }
    }
}

// ---- Audio connections --------------------------------------------------

/// An accepted listener connection.
pub(super) struct ListenerConn {
    pub(super) conn_id: u64,
    pub(super) master_pubkey: String,
    pub(super) conn: Connection,
    /// Stops the task that writes the mixed stream to this connection.
    pub(super) cancel: CancellationToken,
}

/// Serves the mixed stream to listener connections. Present on the host and on relays.
pub(super) struct MixedStreamServer {
    pub(super) fanout: Arc<Fanout>,
    /// Maximum number of entries in `conns`.
    pub(super) capacity: usize,
    /// Transport NodeId -> connection. One connection per NodeId.
    pub(super) conns: HashMap<String, ListenerConn>,
    /// Relay only: stops the task that reads the mixed stream from the host.
    pub(super) upstream_cancel: Option<CancellationToken>,
}

impl MixedStreamServer {
    pub(super) fn new(fanout: Arc<Fanout>, capacity: usize) -> Self {
        Self {
            fanout,
            capacity,
            conns: HashMap::new(),
            upstream_cancel: None,
        }
    }

    /// Close every connection and stop the upstream task.
    pub(super) fn shutdown(&mut self, reason: &'static [u8]) {
        if let Some(cancel) = self.upstream_cancel.take() {
            cancel.cancel();
        }
        for (_, entry) in self.conns.drain() {
            entry.cancel.cancel();
            entry.conn.close(crate::stage::CLOSE_NORMAL.into(), reason);
        }
    }

    /// Close the connections of `master_pubkey` with the given close code.
    pub(super) fn close_identity(&mut self, master_pubkey: &str, code: u32, reason: &'static [u8]) {
        self.conns.retain(|_, entry| {
            if entry.master_pubkey != master_pubkey {
                return true;
            }
            entry.cancel.cancel();
            entry.conn.close(code.into(), reason);
            false
        });
    }
}

/// An accepted speaker connection (host only).
pub(super) struct SpeakerConn {
    pub(super) conn_id: u64,
    pub(super) master_pubkey: String,
    pub(super) conn: Connection,
    /// Stops the receive loop and the forwarding streams of this connection.
    pub(super) cancel: CancellationToken,
    /// This speaker's raw Opus frames, forwarded to the other speakers.
    pub(super) fanout: Arc<Fanout>,
    /// While set, the receive loop discards this speaker's frames.
    pub(super) muted: Arc<AtomicBool>,
}

/// State held only by the host.
pub(super) struct HostState {
    pub(super) mixer: MixerHandle,
    /// The host's own voice, forwarded to every speaker connection.
    pub(super) host_sfu_fanout: Arc<Fanout>,
    /// Transport NodeId -> speaker connection. One connection per NodeId and per
    /// master pubkey.
    pub(super) speaker_conns: HashMap<String, SpeakerConn>,
    pub(super) topology: TopologyManager,
    /// Master pubkey of a relay's owner -> NodeId the relay is registered under.
    pub(super) relay_owners: HashMap<String, String>,
    pub(super) ticket: StageTicket,
    /// Set when a `RoomState` must be broadcast at the next tick.
    pub(super) room_state_pending: bool,
    pub(super) last_room_state_ms: u64,
}

/// State held while the local user is a speaker or co-host (not the host).
pub(super) struct SpeakerSession {
    /// Stops the speaker pipeline, the microphone thread and the speaker mixer actor
    /// (which owns the playback stream).
    pub(super) cancel: CancellationToken,
}

// ---- ActiveStage --------------------------------------------------------

/// Handles the stage state uses to start pipelines and report to the frontend.
#[derive(Clone)]
pub(super) struct StageRuntime {
    pub(super) endpoint: Endpoint,
    pub(super) app_handle: AppHandle,
    /// Sender of the `StageActor` command channel, for background tasks.
    pub(super) cmd_tx: ActorSender,
}

/// All live state for an active Stage session.
pub(crate) struct ActiveStage {
    pub(super) rt: StageRuntime,
    pub(super) stage_id: String,
    pub(super) title: String,
    /// Master pubkey of the local user.
    pub(super) my_pubkey: String,
    /// Transport NodeId of this device.
    pub(super) my_node_id: String,
    /// Role this node acts in. Follows the role the host assigned, except that a
    /// promotion takes effect only while `hand_raised` is set.
    pub(super) my_role: StageRole,
    /// Host-assigned roles; `roles.host_pubkey` is the host's master pubkey.
    pub(super) roles: RoomRoles,
    /// Key that signs the host's control messages and audio checkpoints.
    pub(super) host_signing_pubkey: PublicKey,
    /// The host's transport NodeId. Speakers and relays always dial this node.
    pub(super) host_node_id: String,
    /// Whether this node has muted its own microphone. Shared with the microphone
    /// thread so mute takes effect without a round trip through the actor.
    pub(super) self_muted: Arc<AtomicBool>,
    /// Whether the local user requested a speaker slot.
    pub(super) hand_raised: bool,
    pub(super) started_at: u64,
    /// Live participant list, keyed by master pubkey.
    pub(super) participants: HashMap<String, Participant>,
    pub(super) signers: SignerTable,
    /// Signs this node's control messages.
    pub(super) signer: Arc<ControlSigner>,
    /// Gossip control plane for this room.
    pub(super) control_plane: ControlPlane,
    /// Cancellation token for this stage session's background tasks.
    pub(super) cancel: CancellationToken,
    pub(super) host: Option<HostState>,
    /// Host and relays: the listener connections this node serves.
    pub(super) serving: Option<MixedStreamServer>,
    pub(super) speaker: Option<SpeakerSession>,
    /// Transport NodeId the listener pipeline receives from (host or relay).
    pub(super) listener_upstream_id: Option<String>,
    /// Cancellation token for the current listener audio pipeline.
    pub(super) listener_pipeline_cancel: Option<CancellationToken>,
    /// Relays that failed; assignments to them are ignored.
    pub(super) failed_upstreams: HashSet<String>,
    /// Highest checkpoint sequence number verified in this session. A stream whose
    /// checkpoints are not above it is rejected as a replay.
    pub(super) checkpoint_floor: Arc<AtomicU64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles() -> RoomRoles {
        let mut roles = RoomRoles::new("host".to_string());
        roles.co_hosts.insert("cohost".to_string());
        roles.co_hosts.insert("cohost2".to_string());
        roles.speakers.insert("speaker".to_string());
        roles
    }

    #[test]
    fn role_is_derived_from_host_assigned_sets() {
        let roles = roles();
        assert_eq!(roles.role_of("host"), StageRole::Host);
        assert_eq!(roles.role_of("cohost"), StageRole::CoHost);
        assert_eq!(roles.role_of("speaker"), StageRole::Speaker);
        assert_eq!(roles.role_of("listener"), StageRole::Listener);
        assert_eq!(roles.role_of("unknown"), StageRole::Listener);
    }

    #[test]
    fn host_may_moderate_everyone_but_itself() {
        let roles = roles();
        for action in [
            Moderation::Promote,
            Moderation::Demote,
            Moderation::Mute,
            Moderation::Kick,
            Moderation::Ban,
        ] {
            assert!(roles.may_moderate("host", "listener", action));
            assert!(roles.may_moderate("host", "speaker", action));
            assert!(roles.may_moderate("host", "cohost", action));
            assert!(!roles.may_moderate("host", "host", action));
        }
    }

    #[test]
    fn co_host_may_moderate_only_speakers_and_listeners_and_never_ban() {
        let roles = roles();
        for action in [
            Moderation::Promote,
            Moderation::Demote,
            Moderation::Mute,
            Moderation::Kick,
        ] {
            assert!(roles.may_moderate("cohost", "listener", action));
            assert!(roles.may_moderate("cohost", "speaker", action));
            assert!(!roles.may_moderate("cohost", "cohost2", action));
            assert!(!roles.may_moderate("cohost", "host", action));
        }
        assert!(!roles.may_moderate("cohost", "listener", Moderation::Ban));
    }

    #[test]
    fn speakers_and_listeners_may_not_moderate() {
        let roles = roles();
        for actor in ["speaker", "listener", "unknown"] {
            for action in [
                Moderation::Promote,
                Moderation::Demote,
                Moderation::Mute,
                Moderation::Kick,
                Moderation::Ban,
            ] {
                assert!(!roles.may_moderate(actor, "listener", action));
                assert!(!roles.may_moderate(actor, "host", action));
            }
        }
    }

    #[test]
    fn promote_respects_limit_and_bans() {
        let mut roles = RoomRoles::new("host".to_string());
        assert!(roles.ban("banned"));
        assert!(!roles.promote("banned"));
        assert!(!roles.promote("host"));
        for i in 0..MAX_STAGE_SPEAKERS {
            assert!(roles.promote(&format!("s{i}")));
        }
        assert!(!roles.promote("one-more"));
        assert!(!roles.promote("s0"));
        assert!(roles.demote("s0"));
        assert!(roles.promote("one-more"));
    }

    #[test]
    fn demote_clears_host_mute_and_ban_clears_roles() {
        let mut roles = roles();
        assert!(roles.mute("speaker"));
        assert!(!roles.mute("speaker"));
        assert!(!roles.mute("listener"));
        assert!(roles.demote("speaker"));
        assert!(!roles.is_host_muted("speaker"));
        assert_eq!(roles.role_of("speaker"), StageRole::Listener);

        assert!(roles.ban("cohost"));
        assert_eq!(roles.role_of("cohost"), StageRole::Listener);
        assert!(roles.is_banned("cohost"));
        assert!(!roles.ban("host"));
    }

    #[test]
    fn snapshot_round_trip_and_stale_version_rejected() {
        let mut host = roles();
        host.mute("speaker");
        host.ban("troll");
        host.version = 7;

        let mut joiner = RoomRoles::new("host".to_string());
        assert_eq!(joiner.apply_snapshot(&host.snapshot()), Some(true));
        assert_eq!(joiner, host);
        // The same version again changes nothing.
        assert_eq!(joiner.apply_snapshot(&host.snapshot()), Some(false));

        let mut old = host.snapshot();
        old.version = 6;
        old.speakers.clear();
        assert_eq!(joiner.apply_snapshot(&old), None);
        assert_eq!(joiner.role_of("speaker"), StageRole::Speaker);
    }

    #[test]
    fn snapshot_cannot_assign_a_role_to_the_host_or_a_banned_pubkey() {
        let mut joiner = RoomRoles::new("host".to_string());
        let state = RoomState {
            version: 1,
            co_hosts: vec!["a".to_string()],
            speakers: vec!["host".to_string(), "a".to_string(), "b".to_string()],
            host_muted: vec!["c".to_string(), "b".to_string()],
            banned: vec!["host".to_string(), "b".to_string()],
        };
        assert_eq!(joiner.apply_snapshot(&state), Some(true));
        assert_eq!(joiner.role_of("host"), StageRole::Host);
        assert!(!joiner.is_banned("host"));
        assert_eq!(joiner.role_of("a"), StageRole::CoHost);
        assert_eq!(joiner.role_of("b"), StageRole::Listener);
        assert!(joiner.host_muted.is_empty());
    }

    #[test]
    fn signer_table_rejects_unbound_replayed_and_rebound_signers() {
        let mut table = SignerTable::default();
        assert!(!table.accept_timestamp("key-a", 100));
        assert!(table.master_of("key-a").is_none());

        assert!(table.bind("key-a", "master-a"));
        assert_eq!(table.master_of("key-a"), Some("master-a"));
        assert!(table.bind("key-a", "master-a"));
        assert!(!table.bind("key-a", "master-b"));

        assert!(table.accept_timestamp("key-a", 100));
        // Same message again.
        assert!(!table.accept_timestamp("key-a", 100));
        // Older message.
        assert!(!table.accept_timestamp("key-a", 99));
        assert!(table.accept_timestamp("key-a", 101));
    }

    #[test]
    fn signer_table_is_bounded_and_prunes_departed_signers() {
        let mut table = SignerTable::default();
        for i in 0..SignerTable::MAX_ENTRIES {
            assert!(table.bind(&format!("key-{i}"), &format!("master-{i}")));
        }
        assert!(!table.bind("one-more", "master-x"));

        let now = 10 * MAX_TIMESTAMP_DRIFT_MS;
        assert!(table.accept_timestamp("key-0", now));
        table.prune(now, |master| master == "master-1");
        assert!(table.master_of("key-0").is_some());
        assert!(table.master_of("key-1").is_some());
        assert!(table.master_of("key-2").is_none());
        assert!(table.bind("one-more", "master-x"));
    }

    #[test]
    fn rate_limiter_allows_burst_then_refills() {
        let mut limiter = RateLimiter::new(3, 1_000, 0);
        assert!(limiter.allow(0));
        assert!(limiter.allow(0));
        assert!(limiter.allow(10));
        assert!(!limiter.allow(999));
        assert!(limiter.allow(1_000));
        assert!(!limiter.allow(1_500));
        // A long pause refills to the capacity, not beyond it.
        assert!(limiter.allow(100_000));
        assert!(limiter.allow(100_000));
        assert!(limiter.allow(100_000));
        assert!(!limiter.allow(100_000));
    }
}
