# Stage: Live Audio Rooms

Design document for implementing live, multi-participant audio rooms
in iroh-social.

## 1. Overview

A **Stage** is a live, multi-participant audio room. One user (the **host**)
creates a Stage, other users join as **listeners**, and the host can promote
listeners to **speakers**. Speakers broadcast their microphone audio to the
host, who mixes all speakers into a single stream for listeners.

The host coordinates a **relay tree** to scale beyond direct connections:
speakers send audio to a small number of direct subscribers, and volunteer
listeners act as **relays** that forward audio to additional listeners. This
keeps speaker upload bandwidth bounded regardless of audience size.

### Goals

- Audio-only (no video for now)
- Reuse existing cpal + Opus audio pipeline from `call/`
- Custom `STAGE_ALPN` protocol handler (same pattern as DM, peer, call)
- Use `iroh-gossip` for room discovery and control plane
- Zero new crate dependencies for media transport
- Host-coordinated relay tree for scalable fan-out
- Support 1 host + up to ~8 speakers + many listeners
- Integrate with existing social graph (announce Stages to followers)

### Non-goals

- Video streaming
- Recording / playback of past Stages
- Rooms with hundreds of simultaneous speakers

## 2. Dependencies

One new dependency for echo cancellation. Everything else is already in
the project:

| Crate | Version | Role |
|-------|---------|------|
| `iroh` | 0.96 | QUIC endpoint, connections, protocol handler |
| `iroh-gossip` | 0.96 | Room signaling and discovery |
| `cpal` | 0.17 | Audio capture/playback |
| `opus` | 0.3 | Opus codec |
| `sonora-aec3` | latest | Echo cancellation (pure Rust, SIMD, see section 4.10) |

Fallback AEC option: `aec3-rs` (pure Rust AEC3 port, builder API).

We reuse the audio pipeline from `call/audio.rs`, `call/codec.rs`, and
`call/transport.rs`. The media transport uses plain QUIC unidirectional
streams on a new `STAGE_ALPN`, following the same `ProtocolHandler`
pattern as `DmHandler`, `PeerHandler`, and `CallHandler`.

## 3. Concepts

### 3.1 Roles

| Role | Publishes audio | Receives audio | Mixes + forwards | Controls room |
|------|:-:|:-:|:-:|:-:|
| **Host** | Yes | Yes | Yes (primary mixer + SFU) | Yes (full control) |
| **Co-host** | Yes | Yes | No | Partial (mute, promote, demote) |
| **Speaker** | Yes | Yes | No | No (can self-mute, leave) |
| **Relay** | No | Yes | Forwards only | No (can leave, reverts to listener) |
| **Listener** | No | Yes | No | No (can raise hand, volunteer as relay, leave) |

The **host** is the user who creates the Stage. There is exactly one host.
The host runs the primary mixer and the SFU hub. The host has full control:
promote, demote, mute, assign relays, assign co-hosts, end the Stage. If
the host leaves, the Stage ends.

A **co-host** is a participant with partial room control. Co-hosts receive
speaker audio via the host SFU (like any other speaker). They can mute
speakers, promote/demote listeners, kick and ban — but they do not run an
independent mixer. Co-hosts are useful for moderation in large rooms.

A **relay** is a listener that has volunteered (or been asked) to forward
audio streams to other listeners. Relays receive Opus frames from speakers
(or from upstream relays) and re-broadcast them without decoding. This is
a lightweight role -- relays don't need to encode or decode audio, they
just forward raw bytes.

### 3.2 StageId

A Stage is identified by a `TopicId` (32 random bytes), which doubles as
the iroh-gossip topic for the room's control plane.

### 3.3 StageTicket

A shareable join token containing:
- `topic_id: TopicId` -- the gossip topic / stage identifier
- `bootstrap: Vec<EndpointId>` -- initial peers to connect to (at minimum, the host)
- `title: String` -- human-readable Stage title
- `host_pubkey: String` -- identity of the host

Serializable via `Display`/`FromStr` for sharing as a string (e.g., in a
post, DM, or QR code).

## 4. Architecture

### 4.1 Topology: Host-SFU + Mixed Single Stream

The Stage uses a **split topology**: speakers each hold one QUIC
connection to the host, which acts as an SFU for speaker-to-speaker
audio and a mixer for the listener stream.

```
SFU model (each speaker has exactly 1 connection to host):

  [Speaker A] ---QUIC---> [Host]
  [Speaker B] ---QUIC---> [Host]

  Host SfuHub:
    Fanout[A] ---uni-stream---> conn B (Speaker B hears Speaker A)
    Fanout[A] ---uni-stream---> conn C (Speaker C hears Speaker A)
    Fanout[B] ---uni-stream---> conn A (Speaker A hears Speaker B)
    Fanout[host] ---uni-stream---> conn A (Speaker A hears Host)
    Fanout[host] ---uni-stream---> conn B (Speaker B hears Host)

  Host mixer:
    decode all speaker streams -> mix PCM -> encode -> listener Fanout
                                                              |
                                                   +----+----+----+
                                                   |    |    |    |
                                                 [R1] [L1] [L2] [server relay]
                                                 /|\
                                              [L3][L4]
```

**Two audio paths:**

1. **Speaker SFU** -- each speaker opens one QUIC connection to the host.
   The host's `SfuHub` maintains a per-speaker `Fanout` and forwards each
   speaker's raw Opus frames to all other speakers via uni-streams opened
   on their connection. Speakers never connect to each other directly.
   Host voice is encoded separately by the mixer and distributed to all
   speakers via a dedicated `host_sfu_fanout`.

2. **Mixed stream** -- the host decodes all speaker streams, mixes the
   PCM samples, re-encodes as a single Opus stream, and serves it through
   a `Fanout`. Listeners, relays, server relays, and web clients all
   subscribe to this one mixed stream.

**Benefits:**
- Listeners open **1 connection** instead of S (one per speaker)
- Listeners need **1 decoder** instead of S, no client-side mixing
- Relays maintain **1 fanout** instead of S
- Web clients open **1 WebSocket** instead of S
- Speakers open **1 connection** (to host) instead of S-1 (to every peer)
- Topology management is drastically simpler (one stream to distribute)
- Bandwidth per listener: 32 kbps (fixed, regardless of speaker count)
- No mesh signaling needed; no peer NodeId exchange on promotion

**Tradeoff:**
- The host does extra work: SFU forward + decode S streams + mix + re-encode
- For ~8 speakers at 48kHz mono Opus, this is negligible CPU
- The host is a single point of failure (if the host disconnects, the
  Stage ends)

**The relay tree distributes the single mixed stream:**

```
Small Stage (direct):

  [Host] --mixed stream--> [L1]
         --mixed stream--> [L2]
         --mixed stream--> [L3]

Large Stage (with relays):

  [Host] --mixed stream--> [R1] --> [L4] [L5] [L6] ...
         --mixed stream--> [R2] --> [L7] [L8] [L9] ...
         --mixed stream--> [L1]
         --mixed stream--> [L2]
         --mixed stream--> [L3]

With server relay:

  [Host] --mixed stream (QUIC)--> [Server] --QUIC--> [native listeners]
                                           --WS----> [web listeners]
```

The host's `RoomState` gossip message includes a **topology map** that
tells each listener where to connect for the mixed stream:

```rust
/// Describes where a listener should get the mixed audio from.
/// Included in RoomState, keyed by participant pubkey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioAssignment {
    /// Which node to connect to for the mixed audio stream.
    /// Could be the host (direct), a volunteer relay, or a server relay.
    pub source_endpoint_id: String,
}
```

Each source has its own **capacity** -- the max subscribers it can serve:
- **Server relay**: reported by the server in the relay API response
  (based on its bandwidth/config, e.g., 500-3000)
- **Host**: from user settings (`host_relay_capacity`, default 15)
- **Volunteer relay**: self-reported when volunteering (they know their
  own connection quality), falls back to a conservative default (10)

When a new listener joins, the host decides where to place them:
1. If a server relay has capacity, assign there (best bandwidth/uptime).
2. Else if the host has capacity, assign directly to host.
3. Else if a volunteer relay has capacity, assign there.
4. If nothing has capacity, ask a willing listener to become a relay.

### 4.2 Control Plane (Gossip)

All participants join a shared gossip topic. The host broadcasts signed
control messages. Participants broadcast presence and hand-raise requests.

Control messages (sent as JSON over gossip):

```rust
pub const STAGE_ALPN: &[u8] = b"iroh-social/stage/1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StageControl {
    /// Host announces/updates room metadata and topology. Sent periodically
    /// (every 15s) and on any state change. Signed by host's signing key.
    RoomState {
        title: String,
        host_pubkey: String,
        version: u64,                     // monotonic counter, prevents split-brain
        co_hosts: Vec<String>,            // pubkeys of co-hosts (also speakers, with moderation rights)
        speakers: Vec<String>,            // pubkeys of speakers (not including co-hosts)
        relays: Vec<String>,              // pubkeys of active relays
        banned: Vec<String>,              // pubkeys banned from this Stage
        listener_count: u32,
        started_at: u64,
        /// Topology map: participant_pubkey -> AudioAssignment.
        /// Tells each listener/relay where to connect for the mixed stream.
        /// Omitted participants should connect to the host directly.
        topology: HashMap<String, AudioAssignment>,
        signature: String,
    },

    /// Participant announces presence. Sent on join and periodically.
    Join {
        pubkey: String,
        endpoint_id: String,
        role: StageRole,
        /// Whether this listener is willing to serve as a relay.
        willing_relay: bool,
        /// If willing_relay, how many downstream listeners they can serve.
        /// Self-reported based on their connection quality.
        /// None = use default (10).
        relay_capacity: Option<u32>,
    },

    /// Participant leaves.
    Leave {
        pubkey: String,
    },

    /// Listener requests to speak.
    RaiseHand {
        pubkey: String,
        timestamp: u64,
    },

    /// Listener cancels hand raise.
    LowerHand {
        pubkey: String,
    },

    /// Host promotes a listener to speaker (signed by host).
    PromoteSpeaker {
        pubkey: String,
        signature: String,
    },

    /// Host demotes a speaker to listener (signed by host).
    DemoteSpeaker {
        pubkey: String,
        signature: String,
    },

    /// Host designates a speaker as co-host (signed by host).
    /// The co-host gains partial moderation rights (mute, promote, demote, kick, ban).
    AssignCoHost {
        pubkey: String,
        signature: String,
    },

    /// Host revokes co-host role (signed by host).
    /// Co-host stops mixing, its downstream listeners are reassigned.
    RevokeCoHost {
        pubkey: String,
        signature: String,
    },

    /// Host assigns a willing listener as a relay (signed).
    /// The relay should connect to the host (or an upstream relay) to
    /// receive the mixed stream, then start accepting STAGE_ALPN
    /// connections from downstream listeners and forwarding the stream.
    AssignRelay {
        pubkey: String,
        /// Which node to connect to for the mixed stream upstream.
        /// Usually the host, but could be another relay for multi-tier.
        upstream_endpoint_id: String,
        signature: String,
    },

    /// Host revokes relay role (signed). Relay's downstream listeners
    /// will be reassigned in the next RoomState update.
    RevokeRelay {
        pubkey: String,
        signature: String,
    },

    /// Host mutes a speaker (signed by host or co-host).
    MuteSpeaker {
        pubkey: String,
        signature: String,
    },

    /// Kick a participant (signed by host or co-host). Forces immediate
    /// leave. The participant's client should disconnect all streams,
    /// unsubscribe from gossip, and show a "You were removed" message.
    Kick {
        pubkey: String,
        signature: String,
    },

    /// Ban a participant for the duration of the Stage (signed by host
    /// or co-host). Same as Kick but the participant cannot rejoin.
    /// All nodes track the ban list and reject STAGE_ALPN connections
    /// and gossip messages from banned pubkeys.
    Ban {
        pubkey: String,
        signature: String,
    },

    /// Host ends the Stage (signed).
    EndStage {
        signature: String,
    },

    /// Heartbeat from any participant (keeps presence alive).
    Heartbeat {
        pubkey: String,
        role: StageRole,
    },

    /// Emoji reaction from any participant. Displayed briefly in the UI
    /// as floating emoji over the speaker grid.
    Reaction {
        pubkey: String,
        emoji: String,         // single emoji character (e.g., "👏", "❤️", "😂")
        timestamp: u64,
    },

    /// Text chat message from any participant. Displayed in a side panel
    /// or inline chat below the speaker grid. Useful for sharing links,
    /// asking questions, or communicating without audio.
    Chat {
        pubkey: String,
        content: String,       // max 500 chars
        timestamp: u64,
    },

    /// Host broadcasts speaker activity levels (~5x per second).
    /// Lets listeners know who is currently talking and how loud,
    /// for UI indicators (avatar highlight, "speaking" badge, level meters).
    SpeakerActivity {
        /// Speakers with audio above the silence threshold, loudest first.
        active: Vec<SpeakerLevel>,
        timestamp: u64,
    },
}
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerLevel {
    pub pubkey: String,
    /// RMS audio level, 0.0-1.0. Computed by the host from decoded
    /// PCM samples before mixing: sqrt(sum(sample^2) / n) over
    /// each 200ms window. Used for UI indicators only.
    pub level: f32,
}
```

Host-issued commands (PromoteSpeaker, DemoteSpeaker, AssignCoHost,
RevokeCoHost, AssignRelay, RevokeRelay, MuteSpeaker, Kick, Ban, EndStage)
are signed by the host's signing key. Co-host commands (PromoteSpeaker,
DemoteSpeaker, MuteSpeaker, Kick, Ban) can also be signed by a co-host's
signing key. Participants
verify the signature against the host pubkey or any known co-host pubkey
from the latest `RoomState`.

### 4.3 Media Plane (STAGE_ALPN)

Audio transport uses plain QUIC streams on `STAGE_ALPN`.
This follows the same ProtocolHandler pattern as the rest of the codebase.

There are two distinct audio paths:

**Path 1: Speaker SFU (individual streams via host)**

Each speaker holds exactly one QUIC connection to the host. The host's
`SfuHub` forwards speaker audio to all other speakers:

1. Speaker connects to host: `endpoint.connect(host_node_id, STAGE_ALPN)`
2. Speaker opens a bi-stream, sends `CONN_TYPE_SPEAKER` byte + mic Opus on
   the send side
3. Host `SfuHub` atomically registers the connection; for each existing speaker,
   opens a new uni-stream on that speaker's connection subscribed to the new
   speaker's `Fanout`, and opens a new uni-stream on the new speaker's
   connection subscribed to each existing speaker's `Fanout`
4. Host opens a uni-stream on the new speaker's connection subscribed to
   `host_sfu_fanout` (so the speaker hears the host's voice)
5. Speaker accepts N uni-streams from the host (one per other active speaker
   + one for host voice), decoding each into the `SpeakerMixer`
6. Speakers never connect to each other directly

**Path 2: Host-mixed stream (listeners and relays)**

The host produces a single mixed stream for all non-speaker participants:

1. Host receives individual streams from all speakers (via SFU QUIC connections)
2. Host decodes each speaker's Opus stream to PCM (f32 samples)
3. Host mixes all decoded samples (additive mixing + clipping)
4. Host re-encodes the mix as a single Opus stream
5. Host feeds the mixed stream into a `Fanout`
6. Listeners/relays connect to the host (or a relay) on STAGE_ALPN
7. They receive a single uni stream of pre-mixed audio

**Relayed path (for scaling):**

1. Relay connects to host on STAGE_ALPN, receives the mixed stream
2. Relay feeds it into its own `Fanout` (forwarding raw bytes, no decode)
3. Listener connects to relay, receives the same mixed stream
4. One network hop of added latency (~10-30ms), no codec latency

**Why uni streams (not bi-streams):**
- Audio flows one direction: source -> subscriber
- Listeners never send audio back
- Speakers use separate connections for each direction

### 4.4 Fan-out with Broadcast Channel

The fan-out uses `tokio::sync::broadcast` so that the encode loop (or relay
receive loop) is never blocked by slow subscribers:

```rust
use tokio::sync::broadcast;

struct AudioFrame {
    seq: u32,
    timestamp: u32,
    payload: Vec<u8>,  // raw Opus bytes
}

struct Fanout {
    tx: broadcast::Sender<Arc<AudioFrame>>,
}

impl Fanout {
    fn new() -> Self {
        // Buffer ~1s of audio at 20ms/frame. Lagging receivers skip
        // to latest, which is correct for real-time audio.
        let (tx, _) = broadcast::channel(50);
        Self { tx }
    }

    /// Send a frame to all subscribers. Non-blocking, never fails.
    fn send_frame(&self, seq: u32, timestamp: u32, payload: Vec<u8>) {
        let _ = self.tx.send(Arc::new(AudioFrame { seq, timestamp, payload }));
    }

    /// Add a subscriber. Spawns a task that writes frames to the stream.
    /// Returns a CancellationToken to stop it.
    fn add_subscriber(&self, mut stream: SendStream) -> CancellationToken {
        let mut rx = self.tx.subscribe();
        let cancel = CancellationToken::new();
        let token = cancel.clone();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = rx.recv() => {
                        match result {
                            Ok(frame) => {
                                if transport::write_audio_frame(
                                    &mut stream, frame.seq,
                                    frame.timestamp, &frame.payload,
                                ).await.is_err() {
                                    break; // stream dead, exit
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                log::debug!("[stage] subscriber lagged {n} frames");
                                // Skip to latest -- correct for real-time audio
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
            let _ = stream.finish();
        });

        token
    }
}
```

**Key properties:**
- `send_frame()` is **non-blocking** -- the encode loop is never slowed
  by any subscriber
- Lagging subscribers skip to latest frame (correct for real-time audio --
  old frames are useless)
- Each subscriber gets its own task, isolated from others
- `Arc<AudioFrame>` means each Opus packet is allocated once, not cloned
  N times
- The same `Fanout` struct is used by both speakers and relays

### 4.5 Relay Forwarding

A relay forwards the **single mixed stream** from the host (or from an
upstream relay). It uses the same `Fanout` struct, fed from a receive
loop instead of an encoder:

```rust
/// Relay receive loop: reads frames from upstream and feeds into local fanout.
/// Does NOT decode -- just forwards raw Opus bytes.
async fn relay_forward_loop(
    mut recv: RecvStream,      // from host or upstream relay
    fanout: &Fanout,           // to downstream listeners
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = transport::read_audio_frame(&mut recv) => {
                match result {
                    Ok(Some((seq, ts, payload))) => {
                        fanout.send_frame(seq, ts, payload);
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
}
```

Since there's only one mixed stream, a relay maintains a single `Fanout`:

```rust
struct RelayState {
    /// Single fanout for the mixed stream.
    fanout: Fanout,
    /// Token for the upstream receive/forward task.
    upstream_token: CancellationToken,
    /// Tokens for downstream subscriber tasks.
    subscriber_tokens: Vec<CancellationToken>,
}
```

> **Implementation note (Tokio actor pattern):** `RelayState` in `relay.rs` should be
> implemented as a Tokio actor. Two concurrent callers need to mutate `RelayState` at
> runtime: the upstream forward loop (calls `fanout.send_frame` on every received frame)
> and `StageHandler::accept` (calls `fanout.add_subscriber` whenever a downstream
> listener connects). Wrapping `RelayState` in `Arc<Mutex<>>` would require holding the
> lock across async `add_subscriber` operations and would couple the hot forward loop to
> lock contention. Instead, the actor owns `RelayState` exclusively and processes
> `RelayCommand` messages from an `mpsc::Receiver`. An `mpsc::Sender<RelayCommand>`
> (cheap to clone) is held by both callers. Commands:
>
> ```rust
> enum RelayCommand {
>     /// New downstream listener connected — add their send stream to the fanout.
>     AddDownstream(SendStream),
>     /// Upstream frame received — forward to all downstream subscribers.
>     Frame { seq: u32, ts: u32, payload: Vec<u8> },
>     Shutdown,
> }
> ```
>
> The actor loop handles both the upstream recv future and incoming commands via
> `tokio::select!`, keeping all `RelayState` mutations sequential with no mutex.

### 4.6 Audio Pipeline

Reuses existing infrastructure from `call/`. Five distinct pipelines:

**Pipeline 1: Speaker capture + send (all speakers including host):**
```
Microphone -> AudioCapture (cpal) -> AEC capture -> OpusEncoder -> QUIC bi-stream send side to host
```
Each speaker encodes their own audio and sends it on the single QUIC
connection to the host. Same as the current 1:1 call send loop. AEC
processes the capture before encoding. On reconnect, a fresh `AudioCapture`
and AEC thread are created for the new attempt; the `SpeakerMixer` persists
across reconnects and accepts a new `far_end_tx` per attempt.

**Pipeline 2: Speaker receive + mix + playback (non-host speakers):**
```
SFU uni-stream (Speaker A) -> OpusDecoder A -> mpsc::Sender -> \
SFU uni-stream (Speaker B) -> OpusDecoder B -> mpsc::Sender ->  } SpeakerMixer (20ms tick)
SFU uni-stream (Host voice) -> OpusDecoder H -> mpsc::Sender -> /       |
                                                                   sum + clamp
                                                                         |
                                                                   far_end_tx (AEC reference)
                                                                         |
                                                                   AudioPlayback (cpal)
```
Non-host speakers accept N uni-streams from the host (one per other active
speaker + one for host voice). Each `speaker_stream_recv` task decodes
frames and sends PCM to the `SpeakerMixer` actor via a per-stream channel.
The `SpeakerMixer` runs at 20ms intervals: sums all per-stream buffers
sample-wise (clamp to -1.0..1.0), drives `AudioPlayback`, and tees each
mixed chunk to `far_end_tx` for the AEC far-end reference.

**SpeakerMixer actor:**
```rust
enum SpeakerMixerCmd {
    /// Register a new SFU stream's PCM channel.
    AddStream { id: u32, rx: mpsc::Receiver<Vec<f32>> },
    /// Deregister a stream that has ended.
    RemoveStream(u32),
    /// Wire (or rewire) the AEC far-end channel — called once per reconnect attempt.
    SetFarEndTx(mpsc::Sender<Vec<f32>>),
}
```
The actor persists for the speaker's entire promotion period. It survives
reconnects: each `run_speaker_once` attempt calls `set_far_end_tx` with a
fresh `far_end_tx` before starting capture.

**Pipeline 3: Host mixing + broadcast (host only):**
```
Speaker A recv stream -> OpusDecoder A -> mpsc::Sender -> \
Speaker B recv stream -> OpusDecoder B -> mpsc::Sender ->  } HostMixer (20ms tick)
Speaker C recv stream -> OpusDecoder C -> mpsc::Sender -> /        |
                                                            mix (sum + clamp)
                                                              /             \
                                          listener Fanout (encoded)   host_sfu_fanout (encoded separately)
                                                    |                           |
                                          broadcast (50 frames)         broadcast (50 frames)
                                           / | \                          / | \
                                       [sub][sub][sub]               [sub][sub][sub]
                                       (listeners, relays)      (Speaker A, B, C via uni-streams)
                                              |
                                         mix-minus PCM (total - host_contrib)
                                              |
                                          AEC render
                                              |
                                         AudioPlayback (cpal)
```
The host mixer decodes all speaker streams, sums PCM, and each 20ms tick:
(1) encodes the full mix → listener `Fanout` for listeners/relays;
(2) encodes the host's own contribution → `host_sfu_fanout` for connected speakers;
(3) computes mix-minus (full mix minus host contribution) → host's AEC
render reference and local playback. The host hears all other speakers but
not their own voice echoed back.

**Pipeline 4: Listener playback (listeners, also relays for their own audio):**
```
Single mixed stream (from host or relay)
  -> read_audio_frame()
  -> seq gap check  ->  decode_loss() x N  (PLC for lost frames)
  -> OpusDecoder
  -> ringbuf::HeapRb<f32> producer  (lock-free SPSC, 10-frame / ~200ms capacity)
       [wait for 3-frame / ~60ms pre-fill before starting cpal]
  -> ringbuf::HeapRb<f32> consumer  (lives in cpal callback, lock-free pop_slice)
  -> AudioPlayback (cpal)
```
Listeners receive one stream, decode it, and push PCM into a lock-free ring buffer.
The cpal output callback reads from the consumer end with `pop_slice`; on underrun it
zero-fills (silence) without blocking.

**Jitter buffer rationale:**
- The cpal callback fires on a real-time OS audio thread; it must never take a mutex or
  allocate. `Arc<Mutex<Vec<f32>>>` is incorrect here.
- `ringbuf::HeapRb<f32>` (`ringbuf = "0.4"`) is a lock-free SPSC ring buffer: the tokio
  decode task is the sole producer, the cpal callback is the sole consumer. `push_slice` /
  `pop_slice` are the core operations; no allocation or lock on either side.
- **Capacity: 10 frames (9600 samples, ~200ms)** — hard ceiling. If the buffer is full the
  push is partially dropped; this is preferable to growing unbounded lag.
- **Adaptive pre-fill depth** (governs when playback starts / restarts after reconnect):
  - Min: 3 frames (60ms) — adequate for low-latency WAN
  - Init: 4 frames (80ms) — conservative starting point
  - Max: 10 frames (200ms) — ceiling for degraded WAN / relay hops
  - The cpal callback increments an `Arc<AtomicUsize>` underrun counter (Relaxed ordering,
    purely advisory). The decode task drains the counter once per decoded frame:
    - Underrun detected → `target += 1` (capped at max), reset drift timer
    - Every ~250 frames (~5 s) with no underruns → `target -= 1` (floored at min)
  - Depth converges toward the minimum that avoids underruns on the current network path.
    WAN connections with relay hops naturally settle at a higher depth than direct LAN.
- **Packet loss concealment (PLC):** sequence numbers are tracked across frames. On a gap
  (`seq != last_seq + 1`), `OpusDecoder::decode_loss()` (passes `&[]` to
  `opus::Decoder::decode_float`) is called once per missing frame and the comfort noise is
  pushed into the ring before the next real frame. This avoids abrupt silence pops on
  isolated packet loss (rare on QUIC, but possible across relay hops).
- The same ring buffer pattern is used for 1:1 calls (`call/mod.rs`) without the pre-fill
  gate or adaptive logic (lower latency is more important for interactive calls).

**Pipeline 5: Relay forwarding (relays only):**
```
Upstream mixed stream -> read_audio_frame() -> Fanout.send_frame()
(from host or                                       |
 upstream relay)                           broadcast channel (50 frames)
                                                /    |    \
                                       [sub task] [sub task] [sub task]
                                        stream D   stream E   stream F
                                       (listener) (listener) (sub-relay)
```
Relays forward the mixed stream byte-for-byte, no decoding. If the relay
also wants to hear the audio (they're a listener too), they run Pipeline 4
in parallel from the same upstream.

**Host mixing detail:**
- The host maintains one `OpusDecoder` per speaker
- Each speaker's recv task decodes frames into a per-speaker sample buffer
- A mixing task runs at 20ms intervals (matching the Opus frame rate):
  - Reads available samples from each speaker buffer
  - Computes per-speaker RMS level (for `SpeakerActivity` gossip)
  - Sums them sample-wise, applies `clamp(-1.0, 1.0)`
  - Encodes the mixed PCM into one Opus frame
  - Calls `fanout.send_frame()` to distribute
  - Also appends to the host's own playback buffer
- Every ~200ms (10 mix cycles), broadcasts `SpeakerActivity` via gossip
  with the current per-speaker RMS levels. Only speakers above a silence
  threshold (e.g., level > 0.01) are included. This is how listeners
  know who is currently talking.

> **Implementation note (Tokio actor pattern):** `HostMixer` in `mixer.rs` is a
> Tokio actor. The mixer owns mutable per-speaker decoder state and drives a 20ms
> encode loop via `tokio::time::interval`. Multiple concurrent speaker recv tasks need
> to deliver decoded PCM to the mixer simultaneously. Each speaker recv task sends
> decoded PCM over a per-speaker `mpsc::Sender<Vec<f32>>` to the mixer actor. Commands:
>
> ```rust
> enum MixerCommand {
>     /// New speaker joined — register their PCM channel with the mixer.
>     AddSpeaker { pubkey: String, pcm_rx: mpsc::Receiver<Vec<f32>> },
>     /// Speaker left or was demoted — remove their input from the mix.
>     RemoveSpeaker(String),
>     /// Query current per-speaker RMS levels (for SpeakerActivity gossip).
>     GetLevels { reply: oneshot::Sender<HashMap<String, f32>> },
>     /// Register the host as a speaker for mix-minus local playback.
>     SetHostSpeaker { pubkey: String, pcm_tx: mpsc::Sender<Vec<f32>> },
> }
> ```
>
> The `spawn_mixer` function takes `host_sfu_fanout: Arc<Fanout>` and encodes the
> host's contribution separately each tick, sending it to `host_sfu_fanout` so
> all connected speakers receive the host's voice via uni-streams. Mix-minus
> (total − host contribution) goes to the host's local playback via `SetHostSpeaker`.
> The actor loop selects over the 20ms tick and the command receiver, with no mutex
> on the hot path.

### 4.7 Connection Management

**StageHandler as ProtocolHandler:**

When a node connects on `STAGE_ALPN`, the handler's `accept` fires. The
handler checks the current node's role to determine behavior:

- **If host**: read `CONN_TYPE` byte — if `CONN_TYPE_SPEAKER`, wire into
  `SfuHub` + `HostMixer`; if `CONN_TYPE_LISTENER`, add to mixed stream `Fanout`
- **If relay**: add the connection to the relay's mixed stream `Fanout`
- **If speaker or listener**: reject (they do not accept audio connections)

The handler identifies the remote peer via `conn.remote_id()`. It rejects
connections from banned transport NodeIds (`banned_node_ids` HashSet) and
checks that the peer is a known participant before accepting.

**Speaker SFU connections (host side):**

When a speaker connects, the host:
1. Adds them to the mixer (`pcm_rx` → mixer PCM buffer)
2. Creates `sfu_fanout = Arc::new(Fanout::new())`
3. Locks `sfu_hub`, snapshots existing fanouts+connections, inserts new entry
4. For each existing speaker: opens a uni-stream on their connection
   subscribed to the new speaker's fanout, and opens a uni-stream on the
   new connection subscribed to their fanout
5. Opens a uni-stream on the new connection subscribed to `host_sfu_fanout`
6. Spawns `speaker_recv_sfu_loop` which reads Opus from the speaker's QUIC
   recv stream, forwards raw bytes via `sfu_fanout.send_frame()`, decodes
   PCM into `pcm_tx` for the mixer, and on exit removes from `SfuHub`

**Speaker SFU connections (speaker side):**

The speaker runs `start_speaker_pipeline` which wraps `run_speaker_once`
in an exponential backoff reconnect loop (2s → 30s cap). Each attempt:
1. Creates a fresh `(far_end_tx, far_end_rx)` pair
2. Calls `speaker_mixer.set_far_end_tx(far_end_tx)` to wire AEC reference
3. Spawns an AEC std thread + `AudioCapture` feeding the AEC capture path
4. Connects to `host_node_id` on STAGE_ALPN
5. Sends `CONN_TYPE_SPEAKER` on the bi-stream send side
6. Spawns `speaker_mic_send` to encode mic PCM and write to the send stream
7. Loops `conn.accept_uni()`: each accepted uni-stream spawns
   `speaker_stream_recv` which decodes Opus and sends PCM to `SpeakerMixer`

**Listener connections:**

When a listener receives a `RoomState` with a topology map:

1. Look up own `AudioAssignment` in the topology map
2. Connect to the assigned source (host, volunteer relay, or server relay)
3. Receive a single uni stream of pre-mixed audio
4. Decode and play back -- no client-side mixing needed

If the topology changes (relay added/dropped, rebalancing), the listener
receives an updated `RoomState` and reconnects.

**Relay side (when assigned by host):**

1. Receive `AssignRelay` from host via gossip
2. Connect to host (or upstream relay) on STAGE_ALPN
3. Receive the single mixed stream
4. Create a `Fanout` and spawn `relay_forward_loop`
5. Start accepting STAGE_ALPN connections from downstream listeners
6. When a downstream listener connects, add their stream to the fanout

### 4.8 Host Topology Management

The host runs a topology manager that decides where each listener
connects for the single mixed stream. Since there's only one stream to
distribute (not S), the topology is simpler:

```
// Each source has its own capacity (not a global constant):
//   server_relay.capacity = reported by server (e.g., 500-3000)
//   host.capacity = from user settings (default 15)
//   volunteer_relay.capacity = self-reported (default 10)

on_participant_join(pubkey, endpoint_id):
    if is_speaker(pubkey):
        // Speakers connect to host directly via SFU
        // No topology assignment needed for the mixed stream
        return

    // Listener: assign to a mixed stream source
    // Priority: server relays > host > volunteer relays
    source = sources.iter()
        .find(|s| s.downstream_count < s.capacity)

    if source:
        assign(pubkey, source)
    else:
        candidate = find_willing_relay_candidate()
        if candidate:
            promote_to_relay(candidate, capacity=candidate.relay_capacity)
            assign(pubkey, source -> candidate)
        else:
            // All at capacity, overload the host
            assign(pubkey, source -> host)

    broadcast updated RoomState with new topology

on_relay_disconnect(relay_pubkey):
    for listener in relay.downstream:
        on_participant_join(listener.pubkey, listener.endpoint_id)
    broadcast updated RoomState

on_speaker_added(speaker_pubkey):
    // New speaker connects to host via SFU QUIC connection
    // Host SfuHub wires their Fanout to all existing speaker connections
    // Host mixer adds the new speaker's PCM channel
    // No topology change needed for listeners/relays
    // (they already receive the mixed stream)
```

The topology is recalculated when:
- A listener joins or leaves
- A relay disconnects or is revoked
- A relay volunteers or is assigned

Note: speaker changes (promote/demote) do NOT affect the listener
topology. The host's mixer automatically includes/excludes speakers
from the mix. Listeners seamlessly hear the updated mix without
reconnecting.

The host broadcasts the updated topology in the periodic `RoomState`
(every 15s) and immediately on changes.

> **Implementation note (Tokio actor pattern):** `TopologyManager` in `topology.rs`
> should be implemented as a Tokio actor. Topology events arrive from two concurrent
> sources — the gossip control loop (participant `Join`/`Leave` messages) and the
> `StageActor` (relay volunteering, relay disconnection detected by the media plane).
> Wrapping `TopologyManager` in `Arc<Mutex<>>` and calling mutation methods from both
> contexts would require holding the lock during gossip broadcasts (async), risking
> starvation. Instead, the actor owns the source list and all listener assignments
> exclusively. Commands:
>
> ```rust
> enum TopologyCommand {
>     ParticipantJoined {
>         pubkey: String,
>         endpoint_id: String,
>         willing_relay: bool,
>         relay_capacity: Option<u32>,
>     },
>     ParticipantLeft(String),
>     RelayDisconnected(String),
>     RelayVolunteered { pubkey: String, capacity: u32 },
>     /// Query: caller supplies a oneshot to receive the current topology map.
>     GetTopology(oneshot::Sender<HashMap<String, AudioAssignment>>),
> }
> ```
>
> The host's gossip heartbeat loop calls `GetTopology` (non-blocking `oneshot`) to
> build the `RoomState` broadcast. All assignment mutations are sequential inside the
> actor, so rebalancing decisions are never interleaved with concurrent joins/leaves.

### 4.9 Stream Authentication (Hash Chain)

Relays forward raw Opus bytes without decoding, which means a malicious
relay could silently replace frames with different audio. To let listeners
detect tampering, the host signs a **hash chain** over the mixed audio
stream frames.

**Why not Merkle trees?** Merkle trees allow verifying individual frames
out of order, which is valuable for lossy/unordered delivery (UDP, CDN).
On our reliable ordered QUIC streams, frames always arrive sequentially,
so that property is unused. Merkle trees would add either 1 second of
latency (buffering a batch) or deferred verification (play first, verify
later), with no benefit over a hash chain. The hash chain is simpler,
zero-latency, and equally secure for ordered streams.

**Design:**

The host maintains a running SHA256 hash that chains every mixed frame.
Every N frames (default: 50, i.e., every 1 second), the host signs
the current chain hash and includes the signature in the frame.

```rust
const CHECKPOINT_INTERVAL: u64 = 50;

// Host side: maintained for the mixed stream
struct HostAuthState {
    chain_hash: [u8; 32],
    sequence: u64,
    signing_key: ed25519::SigningKey,
}

impl HostAuthState {
    fn new(host_signing_key: ed25519::SigningKey) -> Self {
        Self {
            chain_hash: [0u8; 32],  // initial seed (all zeros)
            sequence: 0,
            signing_key: host_signing_key,
        }
    }

    fn process_frame(&mut self, payload: &[u8]) -> AuthTag {
        // Extend chain: hash = SHA256(prev_hash || payload)
        let mut hasher = Sha256::new();
        hasher.update(&self.chain_hash);
        hasher.update(payload);
        self.chain_hash = hasher.finalize().into();
        self.sequence += 1;

        if self.sequence % CHECKPOINT_INTERVAL == 0 {
            let signature = self.signing_key.sign(&self.chain_hash);
            AuthTag::Checkpoint {
                chain_hash: self.chain_hash,
                signature: signature.to_bytes(),
            }
        } else {
            AuthTag::None
        }
    }
}
```

**Wire format extension:**

The existing frame format is `[len:u16][seq:u32][timestamp:u32][opus_payload]`.
We extend it with an optional auth tag:

```
Normal frame (49 out of 50):
  [len:u16][seq:u32][ts:u32][tag:u8=0x00][opus_payload]
  Overhead: 1 byte (tag byte)

Checkpoint frame (1 out of 50):
  [len:u16][seq:u32][ts:u32][tag:u8=0x01][chain_hash:32][signature:64][opus_payload]
  Overhead: 97 bytes (tag + hash + sig)

Average overhead: (49 * 1 + 1 * 97) / 50 = 2.9 bytes/frame
At 50 fps: 146 bytes/second (~3% of audio payload)
```

**Listener verification:**

```rust
struct ListenerAuthState {
    chain_hash: [u8; 32],
    expected_sequence: u64,
    host_pubkey: ed25519::VerifyingKey,
    frames_since_checkpoint: u64,
    verified: bool,  // true after first successful checkpoint
}

impl ListenerAuthState {
    fn verify_frame(&mut self, payload: &[u8], tag: &AuthTag) -> AuthResult {
        // Extend local chain hash identically to host
        let mut hasher = Sha256::new();
        hasher.update(&self.chain_hash);
        hasher.update(payload);
        self.chain_hash = hasher.finalize().into();
        self.frames_since_checkpoint += 1;

        match tag {
            AuthTag::None => AuthResult::Ok,
            AuthTag::Checkpoint { chain_hash, signature } => {
                if self.chain_hash != *chain_hash {
                    return AuthResult::TamperDetected;
                }
                match self.host_pubkey.verify(chain_hash, signature) {
                    Ok(()) => {
                        self.verified = true;
                        self.frames_since_checkpoint = 0;
                        AuthResult::Verified
                    }
                    Err(_) => AuthResult::InvalidSignature,
                }
            }
        }
    }
}

enum AuthResult {
    Ok,               // Frame accepted, no checkpoint yet
    Verified,         // Checkpoint passed -- all frames since last checkpoint are authentic
    TamperDetected,   // Chain hash mismatch -- relay modified frames
    InvalidSignature, // Signature invalid -- not from host
}
```

**Relay transparency:**

Relays forward frames byte-for-byte including the auth tag. They cannot
modify frames without breaking the chain. They cannot forge checkpoints
without the mixer's signing key. The authentication is end-to-end from
the mixer (host or co-host) to the listener, transparent to any number
of relay hops.

Co-hosts sign their own mixed streams with their own signing keys.
Listeners verify using the pubkey of whichever mixer they're assigned to
(known from the `RoomState` -- the host pubkey or a co-host pubkey from
the `co_hosts` list).

**On tamper detection:**

When a listener detects tampering (`TamperDetected` or `InvalidSignature`):
1. Emit a `stage-event` to the frontend: `AuthFailed { source }`
2. The frontend shows a persistent warning: "Audio stream could not be
   verified via [source]"
3. The listener continues playing audio -- the user decides whether to stay
4. The frontend offers a "Switch source" button that requests reassignment
   from the host (try a different relay or direct connection)
5. If the source is a relay, notify the host via gossip so the host can
   investigate or revoke the relay

The default behavior is warn-only (no auto-disconnect). A user setting
controls the response:
- **Warn** (default): show warning, keep playing, offer "Switch source"
- **Auto-switch**: automatically request reassignment to a different source
- **Disconnect**: immediately leave the Stage on auth failure

The host, seeing auth failure reports from multiple listeners, can decide
to revoke the relay.

**Late joiners:**

A listener joining mid-stream cannot verify frames until the first
checkpoint arrives (up to ~1 second). The flow:
1. Initialize chain hash to zeros, set `verified = false`
2. Play incoming frames but skip chain hash computation (hashing without
   the full history would produce a wrong chain)
3. When the first `Checkpoint` frame arrives, **adopt** the checkpoint's
   `chain_hash` as the starting point and verify the signature
4. If the signature is valid, set `verified = true` and begin normal
   chain verification from that point forward
5. All subsequent frames are verified continuously

**Mixer public key distribution:**

Listeners know the host's signing pubkey from the `StageTicket`
(`host_pubkey` field) and co-host pubkeys from the `RoomState`
(`co_hosts` list). Delegation certificates from the gossip control plane
map each pubkey to its signing key. No additional key exchange is needed.

### 4.10 Echo Cancellation

When two or more participants are in the same physical room (or using
speakers instead of headphones), audio feedback loops occur: Speaker A's
audio plays through Speaker B's speakers, gets picked up by B's mic,
and is sent back to A. Echo cancellation (AEC) removes this feedback.

**Crate: `sonora-aec3`**

Pure Rust port of Google WebRTC's AEC3 algorithm, by dignifiedquire
(iroh core contributor). SIMD-optimized (SSE2, AVX2, NEON -- Android
works via NEON). No C++ build dependencies. Uses cpal 0.17 (same as us).
Modular: `sonora-aec3` for echo cancellation, optionally add `sonora-ns`
(noise suppression) and `sonora-agc2` (automatic gain control) later.

**Fallback: `aec3-rs`** (RubyBit) -- another pure Rust AEC3 port,
v0.1.7, builder-pattern API (`VoipAec3::builder(sample_rate, ...)`).
Viable if sonora proves immature.

**How AEC works:**

The AEC needs two signals:
- **Reference (render/far-end):** the audio being played through speakers.
  This is what the AEC needs to "subtract" from the mic input.
- **Capture (near-end):** the raw microphone input, containing the
  speaker's own voice plus echoes of the reference signal.

```
Capture path:

  Microphone -> chunk to 10ms (480 samples at 48kHz)
             -> process_capture_frame()  -- AEC removes echo
             -> feed to OpusEncoder
             -> network

                     ^
                     | reference signal (what's playing through speakers)
                     |

Playback path:

  Network -> OpusDecoder -> chunk to 10ms
          -> process_render_frame()  -- feeds AEC the reference
          -> speaker output
```

The render frame must be fed before (or simultaneously with) the capture
frame so the adaptive filter can model the echo path.

**Frame alignment:**

AEC operates on 10ms frames (480 samples at 48kHz). Opus uses 20ms
frames (960 samples). Each Opus frame is exactly 2 AEC frames. The
audio pipeline processes AEC at the 10ms granularity, then batches
into 20ms for Opus encoding.

**Per-role reference signals:**

| Role | What they hear (reference) | AEC removes |
|------|---------------------------|-------------|
| Host | Mix-minus (all speakers except host) via `run_host_playback` | Echo of the mix from host's mic |
| Co-host | SFU streams from all other speakers via `SpeakerMixer` | Echo of SpeakerMixer output from co-host's mic |
| Speaker | SFU streams from all other speakers via `SpeakerMixer` | Echo of SpeakerMixer output from speaker's mic |
| Listener | Single mixed stream (playback only) | N/A -- not capturing audio |

This works naturally: whatever goes to the speakers is the reference
signal. For speakers, the `SpeakerMixer` tees each mixed chunk to
`far_end_tx` before pushing to `AudioPlayback`. For the host, the
mix-minus PCM is teed to `far_end_tx` from the host playback loop.

**Integration into shared `audio/` module:**

```rust
// audio/aec.rs — uses aec3-rs (VoipAec3 builder API)
use aec3::voip::VoipAec3;

pub struct EchoCanceller {
    inner: VoipAec3,
    render_buf: Vec<f32>,
    capture_buf: Vec<f32>,
}

impl EchoCanceller {
    pub fn new() -> Result<Self, ...> {
        let inner = VoipAec3::builder(48_000, 1, 1).build()?;
        Ok(Self { inner, render_buf: Vec::new(), capture_buf: Vec::new() })
    }

    /// Feed playback (far-end reference) samples. Call before process_capture.
    pub fn render(&mut self, samples: &[f32]) {
        self.render_buf.extend_from_slice(samples);
        while self.render_buf.len() >= 480 {
            let frame: Vec<f32> = self.render_buf.drain(..480).collect();
            let _ = self.inner.handle_render_frame(&frame);
        }
    }

    /// Process mic samples, returning echo-cancelled samples.
    pub fn process_capture(&mut self, samples: &[f32]) -> Vec<f32> {
        self.capture_buf.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.capture_buf.len() >= 480 {
            let frame: Vec<f32> = self.capture_buf.drain(..480).collect();
            let mut cleaned = vec![0.0f32; 480];
            match self.inner.process_capture_frame(&frame, false, &mut cleaned) {
                Ok(_) => out.extend_from_slice(&cleaned),
                Err(_) => out.extend_from_slice(&frame),
            }
        }
        out
    }
}
```

The `EchoCanceller` is owned exclusively by a dedicated std thread (not
shared). The capture path (AEC std thread) owns the `EchoCanceller` and
receives raw mic samples from `AudioCapture` via an mpsc channel. The
playback path feeds decoded samples to the AEC via a second mpsc channel
(`far_end_tx`). The AEC thread calls `render()` with far-end samples, then
`process_capture()` with mic samples, and forwards cleaned samples to the
Opus encoder via a third mpsc channel. No mutex on the hot path.

**User setting:**

Echo cancellation is enabled by default but can be toggled off in
settings (useful for headphone users where AEC adds unnecessary CPU).

**Applies to both 1:1 calls and Stages** -- since AEC lives in the
shared `audio/` module, both `call/` and `stage/` benefit automatically.

### 4.11 Reconnection Policy

Participants may lose connections due to network blips, mobile network
switches (wifi <-> cellular), relay crashes, or host instability. The
reconnection policy prioritizes fast recovery with minimal disruption.

**Stream liveness detection:**

Frames arrive every 20ms. If **500ms pass with no frame received**
(25 missed frames), the stream is considered dead. This is detected at
the application layer, not the QUIC layer (QUIC idle timeouts are ~30s,
far too slow for real-time audio). On detection:
1. Cancel the recv task for that stream
2. Begin reconnection immediately

**RoomState versioning:**

Each `RoomState` gossip message includes a monotonic `version: u64`
counter. This prevents split-brain when the host reconnects after a
co-host took over broadcasting:
- The host's `RoomState` always takes authority
- Co-hosts only broadcast when host heartbeats are missing
- Participants accept the `RoomState` with the highest version from
  any authorized source (host or co-host)

#### Scenario 1: Listener loses connection to source

**Cause**: Network blip, relay crash, mobile network switch.

**Policy**: Fast retry with parallel fallback.
1. Detect dead stream (500ms no-frame timeout)
2. **In parallel**:
   a. Retry current source: 3 attempts at 200ms / 500ms / 1s
   b. Check latest gossip `RoomState` for alternative sources
3. Whichever succeeds first wins -- cancel the other
4. If retries fail and `RoomState` has a different source, connect there
5. If no `RoomState` available (gossip also dead), try host directly
   from `StageTicket.bootstrap`
6. If all fails after 15s, emit `stage-event: Disconnected` to frontend
7. Keep retrying in background at 5s intervals with random jitter (0-2s)
8. Frontend shows "Reconnecting..." banner

**Thundering herd prevention**: When a relay dies, all its downstream
listeners detect the dead stream within ~500ms. To prevent them all
hitting the host simultaneously, each listener adds **random jitter of
0-3 seconds** before attempting to connect to a new source. The host's
topology manager will distribute them across available sources as they
arrive.

#### Scenario 2: Speaker loses connection to host

**Cause**: Speaker's QUIC connection to the host drops (network blip,
mobile switch, host restart).

**Policy**: Exponential backoff reconnect.
1. `run_speaker_once` returns when the connection drops
2. `start_speaker_pipeline` waits backoff (initial 2s, doubles to 30s cap)
3. Each retry creates a fresh AEC + capture, calls `set_far_end_tx` on the
   persistent `SpeakerMixer`, reconnects to `host_node_id`
4. Host's mixer removes the disconnected speaker immediately
5. When the speaker reconnects, the host's `SfuHub` opens fresh uni-streams
   to all other speakers and the speaker begins receiving audio again
6. Frontend: speaker grid shows the speaker as "reconnecting" (dimmed avatar)

#### Scenario 3: Relay loses upstream (from host or co-host)

**Cause**: Relay's connection to its upstream source drops.

**Policy**: Urgent -- all downstream listeners depend on this.
1. Detect dead stream (500ms timeout)
2. Retry current upstream: 3 attempts at 200ms / 500ms / 1s
3. If upstream is the host and it fails, try a co-host as upstream
   (relay knows co-hosts from latest `RoomState`)
4. While upstream is down, the relay's `Fanout` simply has no frames
   to send -- downstream listeners detect silence via their own 500ms
   timeout and start their own reconnection
5. If upstream is restored, downstream listeners don't need to
   reconnect -- the relay's `Fanout` resumes sending frames seamlessly
6. If upstream is gone for >10s, relay broadcasts a gossip `Heartbeat`
   with a degraded flag so the host/co-host can reassign downstream
   listeners proactively

#### Scenario 4: Host disconnects entirely

**Cause**: Host's network drops, app crashes, or device sleeps.

**Policy**: Co-host takeover with grace period.
1. Participants detect missing `RoomState` heartbeats (expected every
   15s). After **45s without a heartbeat**, enter "host unreachable" state.
2. **Co-host present**: Co-host detects missing heartbeats and begins
   broadcasting `RoomState` with incremented version and a
   `host_status: "unreachable"` flag. Note: in the SFU model, co-hosts
   do not run an independent mixer. When the host drops, the mixed stream
   and SFU both stop. The Stage is effectively paused until the host
   returns. Topology continues to be broadcast by the co-host so
   participants stay informed.
3. **No co-host**: After 60s without host heartbeat, all participants
   show "Host disconnected." Stage is considered ended. Participants
   linger for an additional 60s grace period in case the host returns.
4. **Host returns**: Host resumes broadcasting `RoomState` with a new
   (higher) version. Co-host sees the host's `RoomState` and stops its
   own broadcasts. Speakers' `start_speaker_pipeline` reconnect loops
   reconnect to the host. Host rebuilds mixer and SFU state from
   incoming speaker connections. Topology is reasserted by the host.

#### Scenario 5: Mobile network change (wifi <-> cellular)

**Policy**:
1. iroh's QUIC layer handles connection migration when possible
   (same connection survives the IP change)
2. If the connection dies despite migration, standard reconnection
   logic per role (scenarios 1-4 above)
3. On Android, the existing `network_change()` broadcast (every 30s)
   should trigger an immediate liveness check on all active Stage
   connections rather than waiting for the 500ms no-frame timeout
4. On network change, proactively verify all connections -- don't wait
   for a read timeout

#### Scenario 6: Gossip vs audio connectivity mismatch

Gossip and STAGE_ALPN use separate QUIC connections. They can fail
independently:

**Audio works, gossip lost:**
- Participant hears audio but misses control messages (promote, demote,
  kick, topology changes)
- Risk: participant could be kicked but not know it, or miss a topology
  reassignment
- Detection: no `RoomState` received for 45s while audio stream is live
- Response: attempt to reconnect gossip to bootstrap peers from
  `StageTicket`. Frontend shows "Control connection lost" warning.

**Gossip works, audio lost:**
- Participant sees control messages but hears nothing
- Detection: audio stream dead (500ms timeout) while gossip is live
- Response: standard audio reconnection (scenario 1). Use gossip's
  `RoomState` topology to find the best source immediately (no need
  for blind retries).

Track both connection states independently in `ActiveStage` and show
appropriate UI for each.

#### Summary

| Scenario | Detection | First retry | Fallback | Grace period |
|---|---|---|---|---|
| Listener loses source | 500ms no-frame | 200ms (parallel with RoomState check) | Alt source from topology, then host direct | 15s before "Disconnected" |
| Speaker loses host connection | connection drop | 2s (exponential backoff to 30s) | Keep retrying indefinitely until cancel | N/A — SpeakerMixer persists |
| Relay loses upstream | 500ms no-frame | 200ms | Try co-host if available, downstream auto-recovers | 10s before degraded flag |
| Host drops | 45s no heartbeat | Co-host takes over | Stage ends after 60s if no co-host | 120s total grace |
| Network change | OS notification | Immediate liveness check | Standard per-role reconnect | N/A |
| Gossip lost, audio ok | 45s no RoomState | Reconnect gossip to bootstrap | Warning banner | N/A |
| Audio lost, gossip ok | 500ms no-frame | Use RoomState for source | Standard reconnect | 15s |

## 5. Module Structure

### 5.1 Backend (Rust)

```
src-tauri/src/audio/                  (shared, used by both call/ and stage/)
    mod.rs
    capture.rs      -- AudioCapture: cpal input device, optional device selection
    playback.rs     -- AudioPlayback: cpal output device, optional device selection
    codec.rs        -- OpusEncoder, OpusDecoder, constants (48kHz, 20ms, mono)
    transport.rs    -- Frame read/write over QUIC streams, auth tag support
    aec.rs          -- EchoCanceller: wraps sonora-aec3 (or aec3-rs fallback),
                       10ms frame processing, shared between capture/playback

src-tauri/src/call/                   (existing, updated imports only)
    mod.rs          -- CallHandler: same logic, imports from audio::*
                       instead of local submodules

src-tauri/src/stage/                  (new)
    mod.rs          -- StageHandler (ProtocolHandler impl, holds StageActorHandle),
                       StageActor (Tokio actor, owns ActiveStage exclusively),
                       StageActorHandle (cheap-to-clone mpsc::Sender<StageCommand>),
                       StageCommand (enum covering all lifecycle, media, control,
                       and query messages)
    control.rs      -- Gossip-based control plane: subscribes to Stage TopicId,
                       deserializes StageControl messages, forwards them to
                       StageActorHandle as StageCommand::GossipEvent; also
                       drives the host heartbeat loop (sends StageCommand::Tick)
    fanout.rs       -- Fanout: thin wrapper over broadcast::Sender<Arc<AudioFrame>>;
                       NOT an actor -- send_frame() and subscribe() are both
                       non-blocking and lock-free; owned exclusively by whichever
                       actor drives it (HostMixer or RelayActor)
    mixer.rs        -- HostMixer (Tokio actor): owns all OpusDecoders, the encode
                       OpusEncoder, listener Fanout, host_sfu_fanout, HostAuthState,
                       RMS accumulators;
                       MixerCommand enum (AddSpeaker, RemoveSpeaker, GetLevels,
                       SetHostSpeaker);
                       MixerHandle (mpsc::Sender<MixerCommand>) held by StageActor
    relay.rs        -- RelayActor (Tokio actor): owns Fanout, upstream
                       CancellationToken, subscriber token list;
                       RelayCommand enum (AddDownstream, Frame, Shutdown);
                       RelayHandle (mpsc::Sender<RelayCommand>) held by StageActor
    topology.rs     -- TopologyActor (Tokio actor): owns source list and all
                       listener assignments; TopologyCommand enum
                       (ParticipantJoined, ParticipantLeft, RelayDisconnected,
                       RelayVolunteered, GetTopology);
                       TopologyHandle (mpsc::Sender<TopologyCommand>) held by StageActor
```

### 5.2 Types (shared crate)

```
crates/iroh-social-types/src/stage.rs
    STAGE_ALPN              -- b"iroh-social/stage/1"
    StageRole               -- Host, CoHost, Speaker, Relay, Listener
    StageControl            -- Control message enum (section 4.2)
    SpeakerLevel            -- Pubkey + RMS audio level (for SpeakerActivity)
    AudioAssignment         -- Per-listener mixed stream source
    StageState              -- Frontend-facing room state snapshot
    StageEvent              -- Frontend-facing event variants
    StageTicket             -- Join token (topic + bootstrap + title + host)
    StageParticipant        -- Pubkey + role + endpoint_id + muted state
    AuthTag                 -- None | Checkpoint { chain_hash, signature }
    AuthResult              -- Ok | Verified | TamperDetected | InvalidSignature

Internal to src-tauri/src/stage/ (not shared):
    -- mod.rs (StageActor owns ActiveStage; all other actors are sub-actors)
    StageActor              -- Tokio actor task; owns Option<ActiveStage>
    StageActorHandle        -- mpsc::Sender<StageCommand>; Clone; held by
                               StageHandler, Tauri command handlers, control loop
    StageCommand            -- enum: CreateStage, JoinStage, LeaveStage, EndStage,
                               IncomingConnection, GossipEvent, PromoteSpeaker,
                               DemoteSpeaker, AssignCoHost, RevokeCoHost,
                               AssignRelay, RevokeRelay, MuteSpeaker, Kick, Ban,
                               RaiseHand, LowerHand, ToggleSelfMute, SendReaction,
                               SendChat, GetState (oneshot reply)
    ActiveStage             -- plain struct, exclusively owned by StageActor;
                               holds sub-actor handles (MixerHandle, RelayHandle,
                               TopologyHandle) rather than the actors themselves
    ListenerAuthState       -- hash chain verification (owned by recv task in mod.rs)

    -- fanout.rs
    Fanout                  -- thin broadcast::Sender<Arc<AudioFrame>> wrapper;
                               NOT an actor; lock-free; owned by MixerActor or RelayActor
    AudioFrame              -- seq + timestamp + payload

    -- mixer.rs (Tokio actor)
    MixerActor              -- actor task; owns all OpusDecoders, listener OpusEncoder,
                               host_sfu OpusEncoder, listener Fanout, host_sfu_fanout,
                               HostAuthState, RMS state
    MixerHandle             -- mpsc::Sender<MixerCommand>; held by StageActor
    MixerCommand            -- AddSpeaker { pubkey, pcm_rx }, RemoveSpeaker,
                               GetLevels { reply }, SetHostSpeaker { pubkey, pcm_tx }
    HostAuthState           -- hash chain + signing state (owned by MixerActor)

    -- mod.rs (SpeakerMixer actor — speaker-side only)
    SpeakerMixerCmd         -- AddStream { id, rx }, RemoveStream(u32), SetFarEndTx(Sender)
    SpeakerMixerHandle      -- mpsc::Sender<SpeakerMixerCmd>; held by ActiveStage
    SfuHub                  -- host-only; fanouts: HashMap<String, Arc<Fanout>>;
                               connections: HashMap<String, Connection>;
                               wrapped in Arc<tokio::sync::Mutex<SfuHub>>

    -- relay.rs (Tokio actor)
    RelayActor              -- actor task; owns Fanout, upstream token, subscriber tokens
    RelayHandle             -- mpsc::Sender<RelayCommand>; held by StageActor
    RelayCommand            -- AddDownstream(SendStream), Frame { seq, ts, payload }, Shutdown

    -- topology.rs (Tokio actor)
    TopologyActor           -- actor task; owns source list and all assignments
    TopologyHandle          -- mpsc::Sender<TopologyCommand>; held by StageActor
    TopologyCommand         -- ParticipantJoined, ParticipantLeft, RelayDisconnected,
                               RelayVolunteered, GetTopology (oneshot reply)
    Source                  -- endpoint_id + kind + capacity + downstream_count
    SourceKind              -- Host | CoHost | ServerRelay | VolunteerRelay
```

### 5.3 Commands

```
src-tauri/src/commands/stage.rs
    create_stage(title: String, relay_servers: Vec<String>) -> StageTicket
    join_stage(ticket: String) -> StageState
    leave_stage()
    raise_hand()
    lower_hand()
    volunteer_relay()                              // offer to be a relay
    promote_speaker(pubkey: String)                 // host or co-host
    demote_speaker(pubkey: String)                 // host or co-host
    assign_cohost(pubkey: String)                   // host only
    revoke_cohost(pubkey: String)                  // host only
    assign_relay(pubkey: String)                   // host only
    revoke_relay(pubkey: String)                   // host only
    mute_speaker(pubkey: String)                   // host or co-host
    kick_participant(pubkey: String)                // host or co-host
    ban_participant(pubkey: String)                 // host or co-host
    toggle_self_mute() -> bool
    send_reaction(emoji: String)
    send_chat(content: String)
    add_relay_server(url: String)                   // host only, mid-Stage
    remove_relay_server(url: String)                // host only, mid-Stage
    end_stage()                                     // host only
    get_stage_state() -> StageState
```

No `stage_id` parameter needed on most commands since only one Stage can be
active at a time (like the current call model).

### 5.4 Frontend

```
src/routes/stage/[id]/+page.svelte    -- Full page Stage view
  OR
src/lib/StageOverlay.svelte           -- Floating overlay (like CallOverlay)

Components:
  src/lib/StageCard.svelte            -- Preview card shown in feed/discover
  src/lib/SpeakerGrid.svelte          -- Grid of speaker avatars with
                                         audio level indicators
  src/lib/HandRaiseQueue.svelte       -- List of raised hands (host view)
  src/lib/StageChat.svelte            -- Side panel or inline text chat
  src/lib/ReactionOverlay.svelte      -- Floating emoji animations over
                                         the speaker grid

Types addition in src/lib/types.ts:
  StageState, StageEvent, StageTicket, StageParticipant, StageRole
```

### 5.5 Frontend Events (Tauri -> Frontend)

```
"stage-state"       -- StageState snapshot (full room state)
"stage-event"       -- StageEvent (joined, left, promoted, demoted,
                       hand raised, relay assigned, stage ended)
"stage-speaker-activity" -- Per-speaker audio levels from host's mixer
                            { active: [{pubkey, level}], timestamp }
                            Used for avatar highlights, "speaking" badges,
                            and level meters in the speaker grid.
"stage-reaction"        -- Emoji reaction from a participant
                            { pubkey, emoji, timestamp }
"stage-chat"            -- Text chat message from a participant
                            { pubkey, content, timestamp }
```

## 6. State Management

### 6.1 StageHandler

```rust
pub struct StageHandler {
    gossip: Gossip,
    endpoint: Endpoint,
    identity: SharedIdentity,
    storage: Arc<Storage>,
    app_handle: AppHandle,
    active_stage: Arc<Mutex<Option<ActiveStage>>>,
}

struct ActiveStage {
    stage_id: TopicId,
    title: String,
    my_role: StageRole,
    host_pubkey: String,
    /// Stable transport NodeId of the host. Speakers always dial this directly,
    /// even after a relay assignment overwrites `listener_upstream_id`.
    host_node_id: String,

    // Participants
    participants: HashMap<String, Participant>,    // all known participants
    raised_hands: Vec<(String, u64)>,
    banned: HashSet<String>,                       // pubkeys banned from this Stage
    banned_node_ids: HashSet<String>,              // transport NodeIds for ban enforcement at connection time

    // Audio: host mixer (host only)
    // Decodes all speaker streams, mixes, re-encodes as single listener stream
    // and encodes host contribution to host_sfu_fanout for speakers.
    mixer: Option<MixerHandle>,
    listener_fanout: Option<Arc<Fanout>>,          // serves mixed stream to listeners/relays
    host_sfu_fanout: Option<Arc<Fanout>>,          // serves host voice to connected speakers
    sfu_hub: Option<Arc<tokio::sync::Mutex<SfuHub>>>,  // host-only; per-speaker fanouts + connections

    // Audio: speaker (when my_role == Speaker or Host)
    speaker_mixer: Option<SpeakerMixerHandle>,     // non-host speakers only; persists across reconnects

    // Audio: relay mode (relay only)
    relay_state: Option<RelayState>,

    // Audio: listener upstream
    listener_upstream_id: Option<String>,          // NodeId we currently recv mixed stream from

    // Audio: playback
    playback: Option<AudioPlayback>,
    recv_tasks: HashMap<String, CancellationToken>,

    // Host: topology management
    topology: Option<TopologyManager>,

    // Lifecycle
    cancel: CancellationToken,
    muted: Arc<AtomicBool>,
}

struct Participant {
    pubkey: String,
    role: StageRole,
    node_id: Option<String>,                       // transport NodeId (from Presence heartbeats)
    last_seen_ms: u64,                             // for presence expiry (45s)
    hand_raised: bool,
    self_muted: bool,
    host_muted: bool,
}

struct SpeakerState {
    endpoint_id: EndpointId,
    muted: bool,
}

struct ListenerState {
    endpoint_id: EndpointId,
    willing_relay: bool,
    /// Which source this listener is assigned to (host, relay, server).
    /// Used by topology manager for reassignment decisions.
    assigned_source: Option<EndpointId>,
}

/// Metadata about a relay, tracked by the host's topology manager.
/// Not the same as RelayState (which is the relay's own runtime state).
struct RelayInfo {
    endpoint_id: EndpointId,
    /// Per-source capacity (self-reported or server-reported).
    capacity: u32,
    /// How many downstream listeners are currently assigned.
    downstream_count: u32,
}
```

Only one active Stage at a time. A user cannot be in a Stage and a
1:1 call simultaneously.

> **Implementation note (Tokio actor pattern):** Unlike `CallHandler`, which uses
> `Arc<Mutex<Option<ActiveCall>>>` safely because `ActiveCall` is tiny (call_id,
> peer pubkey, cancel token, one AtomicBool) and no lock is ever held across an await
> point, `ActiveStage` is far too complex for that approach.
>
> `StageHandler` should hold a `StageActorHandle` (an `mpsc::Sender<StageCommand>`)
> rather than `Arc<Mutex<Option<ActiveStage>>>`. A `StageActor` task owns
> `Option<ActiveStage>` exclusively and processes all mutations sequentially:
>
> ```rust
> /// Cheap to clone — just a channel sender.
> #[derive(Clone)]
> pub struct StageActorHandle {
>     cmd_tx: mpsc::Sender<StageCommand>,
> }
>
> enum StageCommand {
>     // Lifecycle
>     CreateStage { title: String, relay_servers: Vec<String>, reply: oneshot::Sender<Result<StageTicket, AppError>> },
>     JoinStage    { ticket: StageTicket, reply: oneshot::Sender<Result<StageState, AppError>> },
>     LeaveStage,
>     EndStage,
>
>     // Media plane (from ProtocolHandler::accept)
>     IncomingConnection(Connection),
>
>     // Control plane (from gossip loop in control.rs)
>     GossipEvent(StageControl),
>
>     // Host commands (from Tauri command handlers)
>     PromoteSpeaker(String),
>     DemoteSpeaker(String),
>     AssignCoHost(String),
>     RevokeCoHost(String),
>     AssignRelay(String),
>     RevokeRelay(String),
>     MuteSpeaker(String),
>     Kick(String),
>     Ban(String),
>     RaiseHand,
>     LowerHand,
>     ToggleSelfMute { reply: oneshot::Sender<bool> },
>     SendReaction(String),
>     SendChat(String),
>
>     // Query
>     GetState(oneshot::Sender<Option<StageState>>),
> }
> ```
>
> **Why this is necessary here (and not for `CallHandler`):**
>
> 1. `ProtocolHandler::accept` is called concurrently for every incoming QUIC
>    connection (multiple speakers and listeners may connect simultaneously). Each
>    `accept` call needs to read the current role list and mutate the participant
>    map. With a mutex, these calls would serialize on every connection attempt.
>    With the actor, each `accept` simply sends `IncomingConnection(conn)` and
>    returns — the actor serializes them naturally.
>
> 2. Multi-step commands like `PromoteSpeaker` involve async work (connect to
>    the new speaker on `STAGE_ALPN`, send a `MixerCommand::AddSpeaker`, broadcast
>    gossip). A mutex cannot be held across those awaits. The actor performs
>    them in sequence without holding any lock.
>
> 3. The mixer actor and topology actor (see §4.6 and §4.8 notes) are owned by
>    the `StageActor`. Handles to them (`mpsc::Sender<MixerCommand>`,
>    `mpsc::Sender<TopologyCommand>`) live inside `ActiveStage`. There is no
>    shared mutable state: the `StageActor` drives everything via message passing.

### 6.2 Lifecycle

**Create Stage (host):**
1. Generate random `TopicId`
2. Subscribe to gossip topic
3. Start audio capture + encoder (host is always a speaker)
4. Initialize `HostMixer` (no speaker inputs yet)
5. Initialize `Fanout` for the mixed stream (empty, subscribers will connect)
6. Start mixer loop: decode speaker inputs -> mix -> encode -> fanout
7. Initialize playback buffer + playback (host hears the mix)
8. Initialize `TopologyManager`
9. For each relay server URL in `relay_servers`:
   send `POST /api/v1/stage/relay` -- servers that accept are added to
   the topology as high-capacity relay nodes
10. Broadcast initial `RoomState` via gossip (includes topology with
    server relays if any were accepted)
11. Return `StageTicket` to frontend
12. Start heartbeat loop (broadcast `RoomState` every 15s)
13. Register as accepting STAGE_ALPN connections

**Join Stage (listener):**
1. Parse `StageTicket` from string
2. Subscribe to gossip topic with bootstrap peers
3. Wait for `RoomState` from host (learn topology)
4. Look up own `AudioAssignment` in the topology:
   - If present, connect to assigned source (host or relay)
   - If absent, connect directly to host (default)
5. `endpoint.connect(source_endpoint_id, STAGE_ALPN)`
6. `conn.accept_uni()` -- receive single mixed audio stream
7. Spawn recv task: read frames -> decode Opus -> append to playback_buf
8. Start `AudioPlayback` reading from playback_buf
9. Broadcast `Join { role: Listener, willing_relay }` via gossip
10. Emit `StageState` to frontend

**Promote to Speaker:**
1. Host sends `PromoteSpeaker` via gossip (signed)
2. Promoted participant verifies host signature
3. Disconnects from mixed stream source (was a listener)
4. Spawns `SpeakerMixer` actor (persists for the duration of the speaker role)
5. Calls `start_speaker_pipeline(endpoint, host_node_id, speaker_mixer, cancel)`:
   - Connects to `host_node_id` on STAGE_ALPN (one connection)
   - Sends `CONN_TYPE_SPEAKER` on bi-stream send side
   - Spawns `speaker_mic_send` to encode mic PCM + write to send stream
   - Loops `conn.accept_uni()`: each uni-stream spawns `speaker_stream_recv`
     which decodes Opus and sends PCM to `SpeakerMixer`
   - On disconnect, exponential backoff reconnect
6. Host's `SfuHub` wires up the new speaker's fanout to all existing speaker
   connections and vice versa; host's mixer adds the new speaker
7. Listeners seamlessly hear the new speaker in the mix without reconnecting
8. Broadcasts `Presence { role: Speaker }` via gossip

**Assign as Relay:**
1. Host sends `AssignRelay` via gossip (signed)
2. Assigned participant verifies host signature
3. Connects to host (or upstream relay) on STAGE_ALPN
4. Receives the single mixed stream
5. Creates a `Fanout` and spawns `relay_forward_loop`
6. Starts accepting STAGE_ALPN connections from downstream listeners
7. Host updates topology: redirects some listeners to this relay

**Topology change (rebalancing):**
1. Host broadcasts updated `RoomState` with new topology map
2. Affected listeners compare new assignment to current connection
3. Disconnect from old source, connect to new source
4. Seamless: new recv task starts, old one cancels

**Demote to Listener:**
1. Host sends `DemoteSpeaker` via gossip (signed)
2. Demoted participant cancels `speaker_pipeline` cancel token (stops capture,
   AEC, and the QUIC connection to the host)
3. Drops `SpeakerMixer` handle (actor shuts down)
4. Reconnects to mixed stream source (host or relay, per topology)
5. Host's mixer automatically drops the demoted speaker's input;
   `SfuHub` removes the speaker's fanout and connection
6. Listeners seamlessly stop hearing them in the mix

**Revoke Relay:**
1. Host sends `RevokeRelay` via gossip (signed)
2. Relay drops fanout and upstream connection
   (downstream listeners' streams close)
3. Host reassigns displaced listeners in next topology update

**Leave Stage:**
1. Broadcast `Leave` via gossip
2. If speaker: cancel `speaker_pipeline` cancel token (stops capture,
   AEC, mic send, and the QUIC connection to the host); drop `SpeakerMixer`
3. If relay: stop forwarding, drop fanout
4. Cancel recv tasks, stop playback
5. Unsubscribe from gossip topic
6. Host rebalances topology if the leaving node was a relay

**End Stage (host only):**
1. Host broadcasts `EndStage` via gossip (signed)
2. All participants process as forced leave

## 7. Scaling Analysis

### Speaker SFU (host upload to speakers)

With S speakers, the host forwards S raw Opus streams to each of the S-1
other speakers. Each stream is 32 kbps:

| Speakers | Host SFU upload | Per-speaker download |
|:---:|:---:|:---:|
| 3 | 192 kbps | 64 kbps (2 streams) |
| 5 | 640 kbps | 128 kbps (4 streams) |
| 8 | 1.8 Mbps | 224 kbps (7 streams) |

Each speaker also uploads one stream to the host: 32 kbps upload, fixed
regardless of speaker count. Trivial.

### Mixed stream: Direct from host (no relays)

The host serves the single mixed stream to all listeners directly:
- Host upload: L * 32 kbps (plus SFU upload to S speakers)
- Listener download: 32 kbps (fixed, regardless of speaker count)

| Scenario | Host upload (mix) | Host upload (SFU) | Host total |
|----------|:---:|:---:|:---:|
| 3 speakers, 20 listeners | 640 kbps | 192 kbps | 832 kbps |
| 5 speakers, 50 listeners | 1.6 Mbps | 640 kbps | 2.2 Mbps |
| 8 speakers, 100 listeners | 3.2 Mbps | 1.8 Mbps | 5.0 Mbps |

Works well up to ~50 listeners. The bottleneck is the host's upload for
the mixed stream (same as before). The SFU overhead is proportional to
S^2 but bounded by the small speaker count.

### Mixed stream: With relay tree

Each source serves up to its own capacity. Example with host capacity=15,
volunteer relay capacity=10, server relay capacity=1000:
- Host upload (mix): 15 * 32 kbps = 480 kbps (bounded by host setting)
- Volunteer relay upload: 10 * 32 kbps = 320 kbps (bounded by self-report)
- Server relay upload: 1000 * 32 kbps = 32 Mbps (bounded by server config)

| Scenario | Relays needed | Host upload (mix) | Total capacity |
|----------|:---:|:---:|:---:|
| 5 speakers, 50 listeners | ~3 | 480 kbps | ~225 listeners |
| 5 speakers, 200 listeners | ~13 | 480 kbps | ~210 listeners |
| 8 speakers, 500 listeners | ~33 | 480 kbps | ~510 listeners |

With two tiers of relays, capacity scales to thousands of listeners,
each node capped at ~480 kbps upload.

### With server relay

A single server relay with good bandwidth (e.g., 100 Mbps) can serve:
- 100 Mbps / 32 kbps = ~3,000 listeners from one server

Combined with the host serving speakers directly and a server relay
handling all listeners, the architecture handles very large audiences
with minimal infrastructure.

**Latency impact:**
- Direct from host: ~20-50ms (one QUIC hop)
- Via volunteer relay: ~40-100ms (two QUIC hops)
- Via server relay: ~30-60ms (server likely has better connectivity)
- Via 2-tier relay: ~60-150ms (three QUIC hops)

Acceptable for live audio (broadcast radio has >1s delay).

## 8. Integration with Social Graph

### Announcing a Stage

When a host creates a Stage, broadcast via the existing gossip feed:

```rust
// New variant in GossipMessage enum (protocol.rs)
StageAnnouncement {
    stage_id: String,
    title: String,
    ticket: String,       // serialized StageTicket
    host_pubkey: String,
    started_at: u64,
}

StageEnded {
    stage_id: String,
}
```

Followers subscribed to the host's feed topic see the announcement and can
join. The frontend renders a `StageCard` in the feed with a "Join" button.

### Deep links

`iroh-social://stage/{ticket}` -- opens the app and joins the Stage.

## 9. Implementation Plan

### Phase 1: Extract shared audio module and types [COMPLETE]
1. Extract `call/audio.rs` -> `audio/capture.rs` + `audio/playback.rs`
2. Extract `call/codec.rs` -> `audio/codec.rs`
3. Extract `call/transport.rs` -> `audio/transport.rs`
   Extend frame format with auth tag byte (0x00 normal, 0x01 checkpoint).
4. Add `audio/aec.rs`: `EchoCanceller` wrapping `aec3-rs` (`VoipAec3`).
5. Add `StageRole`, `StageControl`, `StageTicket`, `StageState`,
   `StageEvent`, `StageParticipant` to `iroh-social-types/src/stage.rs`
6. Add `STAGE_ALPN`, `stage_control_topic()`, `sign_stage_control()`,
   `verify_stage_control()` to types crate
7. Add `StageAnnouncement` / `StageEnded` variants to `GossipMessage`
8. Create `src-tauri/src/stage/mod.rs` with `StageHandler` struct
   and `ProtocolHandler` impl

### Phase 2: Control plane [COMPLETE]
9. Implement `stage/control.rs`: gossip subscribe/broadcast for
   `StageControl` messages, presence tracking, signing/verification,
   heartbeat (15s interval), presence expiry sweep (45s)
10. Wire StageHandler into `setup.rs`

### Phase 3: Audio transport — Host-SFU model [COMPLETE]
11. Implement `stage/fanout.rs`: broadcast-channel based fan-out
12. Implement `stage/mixer.rs`: HostMixer decodes N speaker streams,
    mixes PCM, encodes listener stream → listener Fanout, encodes host
    contribution → host_sfu_fanout, mix-minus → host AEC/playback
13. Implement `SfuHub` + SFU wiring in `mod.rs`: per-speaker Fanout,
    per-connection uni-stream subscribers, atomic snapshot-and-insert
14. Implement `SpeakerMixer` actor in `mod.rs`: 20ms tick, per-stream
    PCM buffers, sum+clamp, drive AudioPlayback, tee to AEC far-end
15. Implement `start_speaker_pipeline` + `run_speaker_once` with
    exponential backoff reconnect (2s → 30s)
16. Wire host mixer: decode speaker inputs → mix → listener Fanout +
    host_sfu_fanout; compute per-speaker RMS levels
17. Wire listener recv: single uni stream → OpusDecoder → AudioPlayback
18. Implement create_stage, join_stage, leave_stage, end_stage
19. Implement promote/demote speaker flow (SFU join/leave + mixer
    add/remove + SpeakerMixer start/stop)
20. Implement co-host moderation, ban enforcement (banned_node_ids),
    presence sweep (SweepPresence command)
21. Add Tauri commands in `commands/stage.rs`
22. Add frontend events emission

### Phase 4: Stream authentication [COMPLETE]
23. `HostAuthState`: chain hash + periodic signing in mixer encode loop
24. `ListenerAuthState`: chain verification in recv loop
25. Tamper detection: warn frontend, offer source switch

### Phase 5: Relay tree [COMPLETE]
26. `stage/relay.rs`: RelayState with single fanout, upstream connection,
    downstream subscriber management
27. `stage/topology.rs`: TopologyManager with per-source capacity,
    assignment algorithm, rebalancing
28. `AssignRelay` / `RevokeRelay` control messages, topology map in
    `RoomState`, client-side topology following, relay commands

### Phase 6: Frontend
35. Add TypeScript types for Stage in `types.ts`
36. Build Stage page (`routes/stage/[id]/+page.svelte`) with:
    - Speaker grid with active-speaker highlighting (driven by
      `stage-speaker-activity` events: glowing border, scale, level meter)
    - "Speaking" badge on active speaker avatars
    - Listener count, relay count
    - Self controls (mute, leave, raise hand, volunteer relay)
    - Host controls (promote, demote, mute, assign co-host/relay, end)
    - Co-host controls (promote, demote, mute -- subset of host)
    - Auth status indicator (verified / unverified / tamper warning)
    - Text chat panel (StageChat) with send input
    - Reaction button bar + floating emoji overlay (ReactionOverlay)
37. Build `StageCard.svelte` for feed announcements
38. Build Stage creation flow (title input, relay server selection,
    share ticket)
39. Wire into layout event listeners

### Phase 7: Reconnection and polish
40. Implement stream liveness detection (500ms no-frame timeout)
41. Implement per-role reconnection logic (section 4.11): listener
    parallel retry + fallback, speaker SFU reconnect (backoff already
    implemented in start_speaker_pipeline), relay upstream failover,
    thundering herd jitter
42. Implement co-host takeover on host disconnect (RoomState versioning,
    45s heartbeat timeout, automatic topology reassignment)
43. Implement gossip/audio independent health tracking and UI states
44. Stale participant cleanup (missed Leave messages, 45s expiry)
45. Mobile: foreground service for host/speaker background audio,
    network change proactive liveness check
46. Test with multiple participants across network
47. Add Stage announcements to gossip feed pipeline

### Phase 8: Server-side relay

Integrate Stage relay functionality into the discovery server (`server/`),
so the community server can act as a high-capacity, always-available relay
node for popular Stages.

**Motivation:** Volunteer listener-relays are best-effort -- they may have
limited upload bandwidth, unstable connections, or leave at any time. The
discovery server has dedicated bandwidth and uptime. A single server relay
can serve hundreds of listeners per speaker, removing the need for multi-
tier volunteer relay trees in most cases.

The server supports two transport modes for downstream listeners:

- **QUIC/native (STAGE_ALPN)**: Same protocol as peer-to-peer relay.
  Desktop and mobile clients connect via iroh endpoint. Lowest latency,
  same code path as volunteer relays. Requires identity (iroh endpoint).
- **WebSocket (HTTP)**: For anonymous web listeners. No identity, no
  account, no native app required. Someone shares a Stage link, it opens
  in a browser, they hear the audio immediately. This is primarily a
  **discovery and onboarding** path -- new users experience a Stage via
  the web, then install the native app if they want to speak, host, or
  participate in the social network.

WebSocket listeners are **anonymous and listen-only**:
- No signing key, no pubkey, no gossip participation
- Cannot speak, raise hand, volunteer as relay, or send control messages
- Do not appear in the participant list or listener count (or counted
  separately as "web listeners")
- Cannot verify stream authentication (no host pubkey context) unless
  the web page embeds the host's public key for JS-side verification

Both transports serve from the same underlying `Fanout` (one mixed
stream). The only difference is the downstream write path:

```
                           [Host]
                              |
                     mixed stream via
                         STAGE_ALPN
                              |
                       [Server Relay]
                      single Fanout
                       /        |        \
              QUIC uni stream   |    WebSocket stream
              (native client)   |    (web client)
                                |
                          QUIC uni stream
                          (native client)
```

**Server-side changes (`server/src/`):**

48. Add `STAGE_ALPN` protocol handler to the server's iroh endpoint.
    The server acts as a relay-only node: it never speaks, never hosts,
    only forwards the mixed audio stream.

49. Add Stage relay API and WebSocket endpoints:
    ```
    POST /api/v1/stage/relay
      Body: { ticket: String }
      Response: { relay_endpoint_id: String, ws_url: String, capacity: u32 }
      -- Host requests the server to relay for a Stage. Server connects
         to the host on STAGE_ALPN, receives the single mixed stream,
         and begins accepting downstream connections on both transports.
      -- relay_endpoint_id: for native QUIC clients
      -- ws_url: for WebSocket clients (e.g., wss://server/ws/stage/{id})

    DELETE /api/v1/stage/relay
      Body: { stage_id: String }
      -- Host tells the server to stop relaying.

    GET /api/v1/stage/active
      Response: { stages: Vec<ActiveStageInfo> }
      -- Lists Stages currently being relayed by this server (for
         discovery page). Each entry includes both QUIC and WS endpoints.

    GET /ws/stage/{stage_id}  (WebSocket upgrade)
      -- Anonymous, no authentication required.
      -- After upgrade, server sends binary WebSocket messages with the
         same framing as STAGE_ALPN uni streams:
         [seq:u32 BE][timestamp:u32 BE][auth_tag][opus_payload]
      -- Single connection per listener (one mixed stream).
      -- Server sends a close frame when the Stage ends.

    GET /stage/{stage_id}  (HTML page)
      -- Serves a lightweight web player page (rendered via maud, like
         existing server web pages).
      -- Page includes: Stage title, speaker names/avatars (from server's
         gossip state), a play/pause button, speaker grid, read-only
         text chat feed, reaction animations, and a prominent
         "Get the app" / "Join on iroh-social" CTA.
      -- JS on the page connects to the WS endpoints, decodes Opus via
         Web Audio API + opus-decoder WASM, and plays back.
      -- Shareable URL: https://server.example/stage/{stage_id}
    ```

50. Implement server-side relay logic:
    - On `POST /stage/relay`: connect to host on STAGE_ALPN, receive
      the single mixed stream, create a `Fanout`, accept downstream
      connections on both QUIC and WebSocket.
    - The `Fanout` broadcast channel feeds both transport types. QUIC
      subscribers use the existing `add_subscriber(SendStream)` path.
      WebSocket subscribers use a new `add_ws_subscriber(WsSender)` that
      spawns a task writing binary WS messages from the same broadcast
      receiver.
    - Track active relayed Stages in server state.
    - Auto-cleanup: if the gossip topic goes silent (host ended Stage or
      all speakers left), stop relaying after a timeout.

51. WebSocket framing details:
    - Each WS message is a single binary frame containing exactly one
      mixed audio frame: `[seq:u32][ts:u32][auth_tag][opus_payload]`
    - No length prefix needed (WS messages are already length-delimited).
    - Single WS connection per listener (one pre-mixed stream).
    - Auth tags are forwarded as-is, so web clients can verify the hash
      chain identically to native clients.
    - Ping/pong for keepalive (WS standard), 30s timeout.
    - Server drops slow WS clients (if broadcast channel lags >1s) to
      avoid memory buildup.

52. Integrate server relay into host topology management:
    - The host maintains a list of **approved relay servers** for the
      Stage. This list can include:
      a. Servers the host is registered with (from `list_servers()`)
      b. Servers manually added by the host via URL (e.g., a friend's
         server, a community server with good bandwidth, or a server
         operator who offered relay capacity for the event)
    - On Stage creation, the host can select which servers to request
      relay from (multi-select from approved list + manual URL input).
    - For each approved server, the host sends `POST /api/v1/stage/relay`.
      Servers that accept are added to the topology as high-capacity
      relay nodes.
    - Each server's `relay_endpoint_id` (QUIC) and `ws_url` (WebSocket)
      are included in the `RoomState` gossip broadcasts.
    - Host assigns native listeners to server relays first (before
      volunteer relays) since servers have better bandwidth and uptime.
    - Multiple server relays can serve the same Stage for redundancy
      and geographic distribution.
    - If server relays are available, the topology simplifies to:
      ```
      [Host] --mixed stream--> [Server Relay A] --QUIC--> [native listeners]
             |                                  --WS----> [web listeners]
             |
             +--mixed stream--> [Server Relay B] --QUIC--> [native listeners]
                                                 --WS----> [web listeners]
      ```
      No volunteer relays needed unless all servers are at capacity.

53. Add server relay controls to frontend:
    - Host Stage creation flow:
      - List of registered servers with "Use as relay" toggles
      - Manual URL input to add additional relay servers
      - Auto-select registered servers if user setting is on
    - Stage page (host view):
      - Relay status per server (connected / requesting / failed)
      - Add/remove relay servers mid-Stage
      - Per-relay stats (QUIC listeners, WS listeners)
    - Settings/servers page: show which servers support Stage relay.
    - Stage page (all users): indicator showing relay source (server
      vs volunteer) and transport type (QUIC vs WS).

54. Implement web player page:
    - Server renders `/stage/{stage_id}` via maud (same approach as
      existing `server/src/web/pages.rs`).
    - Minimal JS: connect to WS endpoints, decode Opus with a WASM
      decoder (e.g., `libopus.js` or `opus-decoder`), play via Web
      Audio API (`AudioContext` + `AudioWorklet`).
    - Page shows Stage metadata from gossip: title, host name, speaker
      list, listener counts (native + web separately).
    - "Get the app" CTA with deep link: `iroh-social://stage/{ticket}`
      (opens native app if installed, falls back to download page).
    - No login, no account creation, no identity -- pure listen-only.

55. Server-side resource limits and access control:
    - Max concurrent relayed Stages per server (configurable).
    - Max QUIC listeners per Stage, max WS listeners per Stage
      (separate limits -- WS is cheaper to abuse).
    - Requesting relay (POST): requires server registration (existing
      auth from `api/auth.rs`).
    - Listening via WS: no auth, but rate-limited per IP.
    - WS connections: max per IP, max per Stage, idle timeout.

## 10. User Settings

Stage settings are added as a new section in the existing settings page
(`/settings`). Settings are persisted in the SQLite database (new
`stage_settings` table) and loaded on app start.

### 10.1 Settings Schema

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageSettings {
    // -- Listening --

    /// What to do when stream authentication fails.
    /// "warn" (default) | "auto_switch" | "disconnect"
    pub auth_failure_action: String,

    /// Automatically volunteer as a relay when joining a Stage.
    /// Default: false
    pub auto_volunteer_relay: bool,

    /// How many downstream listeners to offer when volunteering as
    /// a relay. Self-assessed based on connection quality.
    /// Default: 10
    pub volunteer_relay_capacity: u32,

    // -- Hosting --

    /// Max listeners the host will serve directly before delegating
    /// to relays.
    /// Default: 15
    pub host_relay_capacity: u32,

    /// Auto-request server relay when creating a Stage (if a server
    /// is registered and supports it).
    /// Default: true
    pub auto_request_server_relay: bool,

    // -- Audio --

    /// Input audio device name. None = system default.
    pub audio_input_device: Option<String>,

    /// Output audio device name. None = system default.
    pub audio_output_device: Option<String>,

    /// Playback buffer size in milliseconds. Higher = more resilient
    /// to jitter, lower = less latency.
    /// Default: 200, range: 50-500
    pub playback_buffer_ms: u32,

    /// Enable echo cancellation. Recommended for speaker/laptop users.
    /// Headphone users can disable to save CPU.
    /// Default: true
    pub echo_cancellation: bool,

    // -- Notifications --

    /// Show a notification when a followed user starts a Stage.
    /// Default: true
    pub notify_stage_announcements: bool,
}
```

### 10.2 Tauri Commands

```
src-tauri/src/commands/stage.rs (additions)
    get_stage_settings() -> StageSettings
    save_stage_settings(settings: StageSettings)
```

### 10.3 Storage

```sql
-- New migration: 011_stage_settings
CREATE TABLE IF NOT EXISTS stage_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

Settings stored as individual key-value rows for easy partial updates.
Loaded into memory on startup, cached in `AppState` or `StageHandler`.

### 10.4 Frontend (settings page additions)

New section in `/settings` page, between "Devices" and "Security":

```
Stage

  Audio
    Input device:    [dropdown: System default / device list]
    Output device:   [dropdown: System default / device list]
    Playback buffer: [slider: 50ms - 500ms, default 200ms]
    Echo cancellation: [toggle, default on]

  Hosting
    Max direct listeners:             [number input, default 15]
    Auto-request server relay:        [toggle, default on]

  Listening
    On authentication failure:        [dropdown: Warn / Auto-switch source / Disconnect]
    Auto-volunteer as relay:          [toggle, default off]
    Relay capacity (if volunteering):  [number input, default 10]

  Notifications
    Notify when followed user starts a Stage:  [toggle, default on]
```

The input/output device dropdowns are populated by querying available
audio devices from cpal at runtime (new command: `list_audio_devices()`).

### 10.5 Implementation Notes

- Audio device selection requires changing `AudioCapture::start()` and
  `AudioPlayback::start()` to accept an optional device name instead of
  always using the system default. Falls back to default if the saved
  device is no longer available.
- `playback_buffer_ms` replaces the hardcoded `FRAME_SIZE * 10` cap in
  the recv loop. Converted to sample count: `48000 * ms / 1000`.
- `host_relay_capacity` sets the host's `Source.capacity` in the
  `TopologyManager`.
- `auto_volunteer_relay` sets the `willing_relay` flag in the `Join`
  gossip message automatically.
- `volunteer_relay_capacity` is sent as `relay_capacity` in the `Join`
  gossip message when volunteering.
- `notify_stage_announcements` controls whether `StageAnnouncement`
  gossip messages trigger a desktop notification.
- Settings changes take effect immediately for most options. Audio device
  changes take effect on the next Stage join/create (not mid-session).

## 11. Open Questions

1. **Overlay vs full page?** The 1:1 call uses `CallOverlay` (floating).
   A Stage has more UI (speaker grid, hand queue, controls). A full page
   (`/stage/{id}`) is probably better, with a minimal floating indicator
   when navigating away.

2. **Simultaneous Stage + 1:1 call?** Currently designed as mutually
   exclusive. Could allow coexistence if audio devices support multiple
   streams, but adds complexity. Not for MVP.

3. **Stage persistence?** Store Stage metadata in DB for "recent Stages"
   or "scheduled Stages" feature later. Not needed for MVP -- Stages are
   fully ephemeral.

4. **Host transfer?** If the host wants to leave without ending the Stage,
   transfer host role to a co-host. The new host would need to start the
   SFU and mixer, and all speakers would reconnect to the new host's
   NodeId. Design the control messages to allow a `TransferHost` variant
   later. For MVP, if the host drops the Stage ends (see scenario 4).

5. **Capacity auto-tuning?** Each source's capacity is currently manual
   (host sets `host_relay_capacity`, volunteers set `volunteer_relay_capacity`,
   servers report their own). Future: auto-tune by measuring actual upload
   throughput and adjusting capacity dynamically.

6. **Authentication on STAGE_ALPN?** When a subscriber connects, how does
   the source verify they're a legitimate participant? Options:
   - Check that the remote EndpointId was seen in the gossip topic
   - Require a signed token from the host (more secure, more complex)
   - Accept anyone (simplest, works for public Stages)
   For MVP, checking gossip presence is sufficient.

7. **Relay incentives?** Why would a listener volunteer as a relay?
   Possible motivations: goodwill, host request, UI badge ("supporting
   this Stage"). No technical enforcement needed -- it's opt-in.

8. **Relay trust?** Stream authentication (section 4.9) detects frame
   tampering via hash chain + periodic signatures. A relay can still
   **drop** frames (causing audio gaps) but cannot **modify** them
   undetected. Frame dropping is detectable via sequence number gaps.
   Combined: listeners can detect both tampering and dropping, and
   request reassignment to a different relay.
