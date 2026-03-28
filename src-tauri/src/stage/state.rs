use crate::stage::control::ControlPlane;
use crate::stage::fanout::Fanout;
use crate::stage::mixer::MixerHandle;
use crate::stage::relay;
use crate::stage::speaker_mixer;
use crate::stage::topology::TopologyManager;
use proscenium_types::{StageRole, StageTicket};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, atomic::AtomicBool};
use tokio_util::sync::CancellationToken;

// ---- Participant tracking -----------------------------------------------

/// Live participant entry, updated by Presence heartbeats.
pub(super) struct Participant {
    pub(super) pubkey: String,
    pub(super) role: StageRole,
    pub(super) hand_raised: bool,
    pub(super) self_muted: bool,
    pub(super) host_muted: bool,
    pub(super) last_seen_ms: u64,
    /// Transport NodeId of this participant, populated from Presence heartbeats.
    pub(super) node_id: Option<String>,
}

// ---- SFU hub (host-only) ------------------------------------------------

/// Host-only shared state for speaker SFU forwarding.
///
/// When a speaker connects, the host creates a `Fanout` for their mic stream
/// and opens uni-streams on all other speaker connections to subscribe them.
/// Wrapped in `Arc<Mutex>` so the connection handler task can update it without
/// going through the actor command channel.
pub(super) struct SfuHub {
    /// QUIC node-id -> their mic Fanout (raw Opus forwarded to other speakers).
    pub(super) fanouts: HashMap<String, Arc<Fanout>>,
    /// QUIC node-id -> their live Connection (for opening new uni-streams).
    pub(super) connections: HashMap<String, iroh::endpoint::Connection>,
}

// ---- ActiveStage --------------------------------------------------------

/// All live state for an active Stage session.
pub(crate) struct ActiveStage {
    pub(super) stage_id: String,
    pub(super) title: String,
    pub(super) host_pubkey: String,
    pub(super) my_pubkey: String,
    pub(super) my_role: StageRole,
    /// Whether this node has muted its own microphone. Shared with the audio
    /// capture thread so mute takes effect immediately without round-tripping
    /// through the actor.
    pub(super) self_muted: Arc<AtomicBool>,
    pub(super) started_at: u64,
    /// Live participant list, keyed by master pubkey.
    pub(super) participants: HashMap<String, Participant>,
    /// Gossip control plane for this room.
    pub(super) control_plane: ControlPlane,
    /// Cancellation token for this stage session's background tasks.
    pub(super) cancel: CancellationToken,
    /// Host/co-host only: the audio mixer actor handle.
    pub(super) mixer_handle: Option<MixerHandle>,
    /// Host/co-host only: fanout for distributing the mixed stream to subscribers.
    pub(super) fanout: Option<Arc<Fanout>>,
    /// Host only: listener assignment topology manager.
    pub(super) topology: Option<TopologyManager>,
    /// Set when this node is acting as a relay: handle to the relay actor.
    pub(super) relay_handle: Option<relay::RelayHandle>,
    /// Invite ticket (host only; `None` for joiners).
    pub(super) ticket: Option<StageTicket>,
    /// Transport NodeId we're currently receiving audio from (host or an upstream relay).
    pub(super) listener_upstream_id: Option<String>,
    /// Cancellation token for the current listener audio pipeline.
    /// Cancelled on relay reassignment to restart the pipeline pointing at the new upstream.
    pub(super) listener_pipeline_cancel: Option<CancellationToken>,
    /// Speaker-role only: PCM mixer actor handle. Each accepted uni-stream is registered
    /// as a separate input; the actor sums them on a 20 ms tick and drives AudioPlayback.
    pub(super) speaker_mixer: Option<speaker_mixer::SpeakerMixerHandle>,
    /// Host-only: SFU hub -- per-speaker Fanouts and Connections for raw Opus forwarding.
    pub(super) sfu_hub: Option<Arc<tokio::sync::Mutex<SfuHub>>>,
    /// Host-only: Fanout for the host's own voice, fed by the mixer each tick.
    /// Speakers subscribe to this via uni-streams opened on their connection.
    pub(super) host_sfu_fanout: Option<Arc<Fanout>>,
    /// The host's stable transport NodeId. Set from ticket on join, or own NodeId on create.
    /// Used by the speaker pipeline to dial the host directly -- never a relay's NodeId.
    pub(super) host_node_id: String,
    /// Transport NodeIds of banned participants. Checked at connection time.
    pub(super) banned_node_ids: HashSet<String>,
}

/// Returns true if `sender_pubkey` is allowed to issue moderation actions
/// (promote/demote/mute/kick/ban) in this stage.
pub(super) fn is_moderator(stage: &ActiveStage, sender_pubkey: &str) -> bool {
    sender_pubkey == stage.host_pubkey
        || stage
            .participants
            .get(sender_pubkey)
            .is_some_and(|p| p.role == StageRole::CoHost)
}
