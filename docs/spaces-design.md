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
| **Host** | Yes | Yes | Yes (primary mixer) | Yes (full control) |
| **Co-host** | Yes | Yes | Yes (backup mixer) | Partial (mute, promote, demote) |
| **Speaker** | Yes | Yes | No | No (can self-mute, leave) |
| **Relay** | No | Yes | Forwards only | No (can leave, reverts to listener) |
| **Listener** | No | Yes | No | No (can raise hand, volunteer as relay, leave) |

The **host** is the user who creates the Stage. There is exactly one host.
The host is always a speaker (participates in the speaker mesh) and runs
the primary mixer. The host has full control: promote, demote, mute,
assign relays, assign co-hosts, end the Stage. If the host leaves, the
Stage ends (unless a co-host is present -- see below).

A **co-host** is a speaker that the host has delegated mixer-relay
responsibilities to. Co-hosts receive all individual speaker streams via
the mesh (like any speaker), but additionally run their own mixer and
serve a mixed stream to assigned listeners via a `Fanout`. This provides:
- **Redundancy**: if the host's connection degrades, listeners on co-host
  sources are unaffected
- **Load distribution**: fan-out is spread across host + co-hosts
- **Failover**: if the host drops, a co-host can keep the Stage alive
  (future: automatic host transfer to a co-host)

Co-hosts also have partial room control (mute speakers, promote/demote
listeners) so they can moderate if the host is busy or unreachable.

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

### 4.1 Topology: Host-Mixed Single Stream

The Stage uses a **split topology**: speakers exchange individual audio
streams in a small mesh for conversation quality, while the host mixes
all speaker audio into a **single Opus stream** that listeners and relays
subscribe to.

```
Speaker mesh (small, S*(S-1) connections):

  [Speaker A] <--individual streams--> [Co-host B]
       \                                   /
        \                                 /
         +-----> [Host] <---------------+
                   |                    |
            primary mixer         co-host mixer
              Fanout                Fanout
               |                    |
         +-----+-----+       +-----+-----+
         |     |     |       |     |     |
       [R1]  [L1]  [L2]   [L5]  [L6]  [L7]
       /|\
    [L3][L4]

Host and co-hosts each independently mix all speaker streams and serve
their own mixed stream. Listeners are assigned to one source by the
topology manager. If the host drops, co-host sources keep working.
```

**Two audio paths:**

1. **Speaker mesh** -- each speaker connects to every other speaker on
   STAGE_ALPN and exchanges individual audio streams (bidirectional, or
   one uni stream per direction). Speakers hear each other's audio
   individually and can adjust relative volumes. This is a small mesh
   (e.g., 8 speakers = 56 connections, trivial).

2. **Mixed stream** -- the host receives all speaker audio, decodes each
   stream, mixes the PCM samples together, re-encodes as a single Opus
   stream, and serves it through a `Fanout`. Listeners, relays, server
   relays, and web clients all subscribe to this one mixed stream.

**Benefits:**
- Listeners open **1 connection** instead of S (one per speaker)
- Listeners need **1 decoder** instead of S, no client-side mixing
- Relays maintain **1 fanout** instead of S
- Web clients open **1 WebSocket** instead of S
- Topology management is drastically simpler (one stream to distribute)
- Bandwidth per listener: 32 kbps (fixed, regardless of speaker count)

**Tradeoff:**
- The host does extra work: decode S-1 speaker streams + mix + re-encode
- For ~8 speakers at 48kHz mono Opus, this is negligible CPU
- The host is a single point of failure for the mix (if the host
  disconnects, the mixed stream stops and the Stage ends anyway)

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
2. Else if a co-host has capacity, assign there (independent mixer, high quality).
3. Else if the host has capacity, assign directly to host.
4. Else if a volunteer relay has capacity, assign there.
5. If nothing has capacity, ask a willing listener to become a relay.

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
        co_hosts: Vec<String>,            // pubkeys of co-hosts (also speakers + mixers)
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
    /// The co-host should start running a mixer and serving listeners.
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

Audio transport uses plain QUIC **unidirectional streams** on `STAGE_ALPN`.
This follows the same ProtocolHandler pattern as the rest of the codebase.

There are two distinct audio paths:

**Path 1: Speaker mesh (individual streams)**

Speakers exchange audio directly with each other for conversation quality.
Each speaker pair opens a connection with one uni stream per direction:

1. Speaker A connects to Speaker B: `endpoint.connect(b_addr, STAGE_ALPN)`
2. Speaker B accepts, opens a uni stream back to A
3. Both sides send their individual Opus-encoded audio
4. Each speaker decodes other speakers' streams individually

This is a small mesh (S speakers = S*(S-1) uni streams). For 8 speakers,
that's 56 streams -- trivial.

**Path 2: Host-mixed stream (listeners and relays)**

The host produces a single mixed stream for all non-speaker participants:

1. Host receives individual streams from all speakers (part of the mesh)
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
Microphone -> AudioCapture (cpal) -> AEC capture -> OpusEncoder -> speaker mesh uni streams
```
Each speaker encodes their own audio and sends it to every other speaker
via the speaker mesh. Same as the current 1:1 call send loop, but to
S-1 peers instead of 1. AEC processes the capture before encoding.

**Pipeline 2: Speaker receive + local mix + playback (non-host speakers):**
```
Mesh stream from Speaker A -> OpusDecoder A -> \
Mesh stream from Speaker B -> OpusDecoder B ->  } local mix (sum + clamp)
Mesh stream from Host      -> OpusDecoder H -> /        |
                                                  AEC render (reference)
                                                        |
                                                  playback_buf -> AudioPlayback (cpal)
```
Non-host speakers receive individual streams from every other speaker
(including the host) via the mesh. They decode each stream, mix locally
(additive: sum f32 samples, clamp to -1.0..1.0), feed the mix to AEC
as the render reference, and play back. This gives speakers per-peer
audio (they could adjust individual volumes in the future).

**Pipeline 3: Host mixing + broadcast (host and co-hosts):**
```
Speaker A recv stream -> OpusDecoder A -> \
Speaker B recv stream -> OpusDecoder B ->  } mix (sum + clamp) -> OpusEncoder -> Fanout
Speaker C recv stream -> OpusDecoder C -> /        |                               |
                                             AEC render               broadcast channel (50 frames)
                                                   |                     /    |    \
                                             playback_buf          [sub task] [sub task] [sub task]
                                                   |                stream A   stream B   stream C
                                          AudioPlayback (cpal)     (listener) (relay R1) (server relay)
```
The host (and co-hosts) decode all incoming speaker streams, mix the PCM
samples, and do two things with the mix: (1) re-encode as a single Opus
stream and feed into the `Fanout` for listeners/relays, and (2) feed to
their own AEC render + playback buffer so they hear the other speakers.

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

> **Implementation note (Tokio actor pattern):** `HostMixer` in `mixer.rs` should be
> implemented as a Tokio actor. The mixer owns mutable per-speaker decoder state and
> drives a 20ms encode loop via `tokio::time::interval`. Multiple concurrent speaker
> recv tasks need to deliver decoded PCM to the mixer simultaneously. Sharing the mixer
> under `Arc<Mutex<>>` would cause recv tasks to block each other on every decoded frame
> (50 times per second per speaker) and would couple the 20ms mix interval to lock
> availability — a recipe for audio glitches. Instead, each speaker recv task sends
> decoded PCM over a per-speaker `mpsc::Sender<Vec<f32>>` to the mixer actor. Commands:
>
> ```rust
> enum MixerCommand {
>     /// New speaker joined — register their PCM channel with the mixer.
>     AddSpeaker { pubkey: String, pcm_rx: mpsc::Receiver<Vec<f32>> },
>     /// Speaker left or was demoted — remove their input from the mix.
>     RemoveSpeaker(String),
>     Shutdown,
> }
> ```
>
> The actor loop selects over the `tokio::time::interval` tick (mix + encode + fanout
> send) and the `MixerCommand` receiver, with no mutex anywhere on the hot path.
> An `mpsc::Sender<MixerCommand>` handle is held by the `StageActor` and passed to
> speaker recv tasks when they are created.

### 4.7 Connection Management

**StageHandler as ProtocolHandler:**

When a node connects on `STAGE_ALPN`, the handler's `accept` fires. The
handler checks the current node's role to determine behavior:

- **If host**: check if remote is a speaker (add to speaker mesh) or a
  listener/relay (add to mixed stream `Fanout`)
- **If speaker**: check if remote is another speaker (exchange individual
  streams) -- speakers don't serve listeners directly
- **If relay**: add the connection to the relay's mixed stream `Fanout`
- **If listener**: reject (listeners don't accept audio connections)

The handler identifies the remote peer via `conn.remote_id()` and resolves
to a pubkey. It rejects connections from banned pubkeys and checks that
the peer is a known participant (seen in gossip) before accepting.

**Speaker mesh connections:**

When a speaker joins, they connect to every other speaker (including the
host) on STAGE_ALPN. Each pair exchanges individual audio via uni streams.
The host uses these individual streams as input for the mixer.

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
        // Speakers join the mesh, connect to host directly
        // No topology assignment needed
        return

    // Listener: assign to a mixed stream source
    // Priority: server relays > co-hosts > host > volunteer relays
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
    // New speaker joins the mesh and sends audio to host
    // Host automatically includes them in the mix
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
| Host | Local mix of all speakers (own mixer output) | Echo of the mix from host's mic |
| Co-host | Local mix of all speakers (own mixer output) | Echo of the mix from co-host's mic |
| Speaker | Local mix of individual mesh streams | Echo of the local mix from speaker's mic |
| Listener | Single mixed stream (playback only) | N/A -- not capturing audio |

This works naturally: whatever goes to the speakers is the reference
signal. No special handling per role -- just feed playback audio to
`process_render_frame()` and mic audio to `process_capture_frame()`.

**Integration into shared `audio/` module:**

```rust
// audio/aec.rs (new file in shared audio module)
use sonora_aec3::{Aec3, Aec3Config};  // or aec3_rs::VoipAec3

pub struct EchoCanceller {
    processor: Aec3,
    /// Buffer to accumulate samples into 10ms chunks.
    render_buf: Vec<f32>,
    capture_buf: Vec<f32>,
}

impl EchoCanceller {
    pub fn new(sample_rate: u32) -> Self {
        let config = Aec3Config::default();
        Self {
            processor: Aec3::new(sample_rate, config),
            render_buf: Vec::with_capacity(480),
            capture_buf: Vec::with_capacity(480),
        }
    }

    /// Feed playback audio as reference. Call from playback path.
    pub fn process_render(&mut self, samples: &[f32]) {
        self.render_buf.extend_from_slice(samples);
        while self.render_buf.len() >= 480 {
            let chunk: Vec<f32> = self.render_buf.drain(..480).collect();
            self.processor.handle_render_frame(&chunk);
        }
    }

    /// Process captured mic audio, removing echo. Call from capture path.
    pub fn process_capture(&mut self, samples: &[f32]) -> Vec<f32> {
        self.capture_buf.extend_from_slice(samples);
        let mut output = Vec::new();
        while self.capture_buf.len() >= 480 {
            let chunk: Vec<f32> = self.capture_buf.drain(..480).collect();
            let mut out = vec![0.0f32; 480];
            self.processor.process_capture_frame(&chunk, false, &mut out);
            output.extend_from_slice(&out);
        }
        output
    }
}
```

The `EchoCanceller` is shared between capture and playback paths via
`Arc<Mutex<EchoCanceller>>`. In the capture callback, mic samples are
processed through `process_capture()` before being sent to the encoder.
In the playback callback (or recv loop), decoded samples are fed through
`process_render()` before being written to the output device.

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

#### Scenario 2: Speaker loses connection to mesh peer

**Cause**: Same as above, but a speaker-to-speaker or speaker-to-host
connection drops.

**Policy**: Per-peer reconnect.
1. Each mesh connection is independent -- losing one doesn't affect others
2. Retry the dropped peer: 200ms / 500ms / 1s / 2s / 4s (5 attempts)
3. **If the lost peer is the host**: escalate -- this means the host
   can't mix this speaker's audio. Try 10 attempts over 15s.
4. Host's mixer **fades the disconnected speaker to silence over 100ms**
   (avoids audible pop/click from hard cut)
5. When the speaker reconnects, the host fades them back in
6. Frontend: speaker grid shows the peer as "reconnecting" (dimmed avatar)

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
   `host_status: "unreachable"` flag. Topology continues to function.
   Listeners on the host's direct fanout lose audio and reconnect to
   co-host sources via updated topology. Speaker mesh connections to
   the host drop -- co-hosts still have direct mesh connections to
   other speakers, so their mix continues (minus the host's own audio).
3. **No co-host**: After 60s without host heartbeat, all participants
   show "Host disconnected." Stage is considered ended. Participants
   linger for an additional 60s grace period in case the host returns.
4. **Host returns**: Host resumes broadcasting `RoomState` with a new
   (higher) version. Co-host sees the host's `RoomState` and stops its
   own broadcasts. Speakers reconnect to the host mesh. Host rebuilds
   mixer state from incoming speaker streams. Topology is reasserted
   by the host.

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
| Speaker loses mesh peer | 500ms no-frame | 200ms | Keep trying 15s, mixer fades to silence | 15s (30s if lost peer is host) |
| Relay loses upstream | 500ms no-frame | 200ms | Try co-host, downstream auto-recovers | 10s before degraded flag |
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
                       OpusEncoder, Fanout, HostAuthState, RMS accumulators;
                       MixerCommand enum (AddSpeaker, RemoveSpeaker, Shutdown);
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
    MixerActor              -- actor task; owns all OpusDecoders, OpusEncoder,
                               Fanout, HostAuthState, RMS state
    MixerHandle             -- mpsc::Sender<MixerCommand>; held by StageActor
    MixerCommand            -- AddSpeaker { pubkey, pcm_rx }, RemoveSpeaker, Shutdown
    HostAuthState           -- hash chain + signing state (owned by MixerActor)

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

    // Participants
    co_hosts: HashSet<String>,                     // pubkeys of co-hosts
    speakers: HashMap<String, SpeakerState>,
    relays: HashMap<String, RelayInfo>,
    listeners: HashMap<String, ListenerState>,
    raised_hands: Vec<(String, u64)>,
    banned: HashSet<String>,                       // cannot rejoin this Stage

    // Audio: own capture (speakers, co-hosts, and host)
    capture: Option<AudioCapture>,
    encoder: Option<OpusEncoder>,

    // Audio: mixer (host and co-hosts only)
    // Decodes all speaker streams, mixes, re-encodes as single stream
    mixer: Option<HostMixer>,
    mixed_fanout: Option<Fanout>,          // serves mixed stream to listeners/relays

    // Audio: speaker mesh (speakers only, not host-specific)
    // Individual streams to/from other speakers
    speaker_streams: HashMap<String, CancellationToken>,

    // Audio: relay mode (relay only)
    relay_state: Option<RelayState>,

    // Audio: playback
    // Host/Co-host: hears local mix of all speakers (Pipeline 3)
    // Speaker: hears local mix of individual mesh streams (Pipeline 2)
    // Listener: hears single mixed stream from host/relay (Pipeline 4)
    playback_buf: Arc<Mutex<Vec<f32>>>,
    playback: Option<AudioPlayback>,
    recv_tasks: HashMap<String, CancellationToken>,

    // Host: topology management
    topology: Option<TopologyManager>,

    // Lifecycle
    cancel: CancellationToken,
    muted: Arc<AtomicBool>,
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
4. Starts audio capture + encoder
5. Connects to every other speaker (including host) on STAGE_ALPN for
   individual audio exchange (speaker mesh)
6. Host's mixer automatically picks up the new speaker's stream
7. Listeners seamlessly hear the new speaker in the mix without
   reconnecting
8. Broadcasts `Join { role: Speaker }` via gossip (with endpoint_id)

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
2. Demoted participant stops capture + encoder
3. Disconnects from speaker mesh
4. Reconnects to mixed stream source (host or relay, per topology)
5. Host's mixer automatically drops the demoted speaker's input
6. Listeners seamlessly stop hearing them in the mix

**Revoke Relay:**
1. Host sends `RevokeRelay` via gossip (signed)
2. Relay drops fanout and upstream connection
   (downstream listeners' streams close)
3. Host reassigns displaced listeners in next topology update

**Leave Stage:**
1. Broadcast `Leave` via gossip
2. If speaker: stop capture/encoder, disconnect from speaker mesh
3. If relay: stop forwarding, drop fanout
4. Cancel recv tasks, stop playback
5. Unsubscribe from gossip topic
6. Host rebalances topology if the leaving node was a relay

**End Stage (host only):**
1. Host broadcasts `EndStage` via gossip (signed)
2. All participants process as forced leave

## 7. Scaling Analysis

### Speaker mesh (always present)

With S speakers, the mesh has S*(S-1) uni streams. This is always small:

| Speakers | Mesh connections | Total mesh bandwidth |
|:---:|:---:|:---:|
| 3 | 6 | 192 kbps total |
| 5 | 20 | 640 kbps total |
| 8 | 56 | 1.8 Mbps total |

Per-speaker upload in the mesh: (S-1) * 32 kbps. For 8 speakers, that's
224 kbps each. Trivial.

### Mixed stream: Direct from host (no relays)

The host serves the single mixed stream to all listeners directly:
- Host upload: L * 32 kbps (plus mesh upload to S-1 speakers)
- Listener download: 32 kbps (fixed, regardless of speaker count)

| Scenario | Host upload (mix) | Host upload (mesh) | Host total |
|----------|:---:|:---:|:---:|
| 3 speakers, 20 listeners | 640 kbps | 64 kbps | 704 kbps |
| 5 speakers, 50 listeners | 1.6 Mbps | 128 kbps | 1.7 Mbps |
| 8 speakers, 100 listeners | 3.2 Mbps | 224 kbps | 3.4 Mbps |

Works well up to ~50 listeners. The bottleneck is the host's upload for
the mixed stream.

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

### Phase 1: Extract shared audio module and types
1. Extract `call/audio.rs` -> `audio/capture.rs` + `audio/playback.rs`
   Modify `AudioCapture::start()` and `AudioPlayback::start()` to accept
   an optional device name (for user settings). Update `call/mod.rs` to
   import from `audio::` instead of `self::audio`.
2. Extract `call/codec.rs` -> `audio/codec.rs`
   (`OpusEncoder`, `OpusDecoder`, constants). No changes needed.
3. Extract `call/transport.rs` -> `audio/transport.rs`
   Extend frame format with auth tag byte (0x00 normal, 0x01 checkpoint).
   Keep backward compat for 1:1 calls (tag byte 0x00 always).
4. Add `audio/aec.rs`: `EchoCanceller` wrapping `sonora-aec3` (with
   `aec3-rs` as fallback). Integrate into capture/playback paths.
   Gated by `echo_cancellation` user setting.
5. Verify `call/` still works with the new imports + AEC. Run tests.
6. Add `StageRole`, `StageControl`, `StageTicket`, `StageState`,
   `StageEvent`, `StageParticipant`, `AudioAssignment` to
   `iroh-social-types/src/stage.rs`
7. Add `STAGE_ALPN` constant
8. Add `StageAnnouncement` / `StageEnded` variants to `GossipMessage`
9. Create `src-tauri/src/stage/mod.rs` with `StageHandler` struct
   and `ProtocolHandler` impl

### Phase 2: Control plane
10. Implement `stage/control.rs`: gossip subscribe/broadcast for
    `StageControl` messages
11. Implement host command signing and verification
12. Implement presence tracking with heartbeat (15s interval, 45s expiry)
13. Wire StageHandler into `setup.rs` (register ALPN, construct handler)

### Phase 3: Audio transport (direct from host, no relays)
14. Implement `stage/fanout.rs`: broadcast-channel based fan-out
15. Implement `stage/mixer.rs`: HostMixer that decodes N speaker
    streams (using `audio::codec`), mixes PCM samples, re-encodes as
    single Opus stream, feeds Fanout
16. Wire speaker mesh: speakers connect to each other + host on
    STAGE_ALPN, exchange individual audio via uni streams
    (using `audio::capture`, `audio::codec`, `audio::transport`)
17. Wire non-host speaker local mix: decode individual mesh streams,
    sum + clamp, feed to AEC render + playback (Pipeline 2)
18. Wire host mixer: decode speaker inputs -> compute per-speaker RMS
    levels -> mix -> encode -> fanout
19. Broadcast `SpeakerActivity` via gossip every ~200ms from mixer
20. Wire listener recv: single uni stream -> `audio::codec::OpusDecoder`
    -> `audio::playback` -> speaker output
21. Implement create_stage, join_stage, leave_stage, end_stage
22. Implement promote/demote speaker flow (mesh join/leave + mixer
    input add/remove + speaker local mix start/stop)
23. Implement raise_hand / lower_hand, toggle_self_mute, mute_speaker
24. Add Tauri commands in `commands/stage.rs`
25. Add frontend events emission (including `stage-speaker-activity`)

### Phase 4: Stream authentication
26. Implement `HostAuthState`: chain hash + periodic signing in the
    host's mixer encode loop (auth tag byte already added in Phase 1
    transport refactor)
27. Implement `ListenerAuthState`: chain verification in recv loop
    using host's public key
28. Handle tamper detection: warn frontend, offer source switch

### Phase 5: Relay tree
29. Implement `stage/relay.rs`: RelayState with single fanout for
    mixed stream, upstream connection, downstream subscriber management
30. Implement `stage/topology.rs`: host-side TopologyManager with
    per-source capacity, assignment algorithm, rebalancing
31. Add `AssignRelay` / `RevokeRelay` control messages
32. Add topology map to `RoomState` broadcasts
33. Implement client-side topology following: parse AudioAssignment,
    connect to assigned source, reconnect on changes
34. Add `volunteer_relay` / `assign_relay` / `revoke_relay` commands

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
    parallel retry + fallback, speaker mesh reconnect with fade,
    relay upstream failover, thundering herd jitter
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
   transfer host role to a co-host. Co-hosts already run mixers and have
   partial control, so promotion to full host is natural. Design the
   control messages to allow a `TransferHost` variant later. For MVP,
   if a co-host exists when the host drops, the Stage could continue
   with the co-host serving all listeners (automatic failover).

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
