# Proscenium

A decentralized peer-to-peer social network built with [Iroh](https://iroh.computer/), [Tauri 2](https://tauri.app/), and [SvelteKit 5](https://svelte.dev/).

**Alpha software. Expect breaking changes, data loss, and rough edges. Nothing is stable yet.**

Successor to [follow](https://github.com/iohzrd/follow) and [identia](https://github.com/iohzrd/identia), rebuilt on iroh's QUIC transport with end-to-end encrypted messaging.

Every user runs their own node. Posts, profiles, and follows are stored locally. Peers exchange data directly -- no central server, no accounts, no passwords. Optional discovery servers provide search, trending, and user directories without compromising the P2P foundation.

## Iroh

[Iroh](https://iroh.computer/) is a networking library that gives every node a persistent identity (an Ed25519 keypair) and connects peers directly over QUIC, punching through NATs with the help of relay servers. Peers discover each other via DNS, mDNS on local networks, and the Mainline DHT. Iroh provides two higher-level primitives on top of raw QUIC connections: **iroh-gossip** for real-time pub/sub broadcast, and **iroh-blobs** for content-addressed data transfer. Proscenium uses all of these -- gossip for live feed updates, blobs for media, and direct QUIC connections for DMs, calls, and stage audio.

## How It Works

### Identity and Key Architecture

Each node has a three-tier key hierarchy:

- **Master key** (Ed25519) -- The permanent, unforgeable identity. Stored in `master_key.key`. Your master public key is what others follow. The master key signs delegations and key rotations but never signs content directly. On first launch, a 24-word BIP39 recovery phrase is generated for backup.
- **Transport key** (Ed25519) -- Derived from the master key via HKDF with a device-specific index. Unique per device. Used as iroh's QUIC endpoint identity (NodeId) for networking. Never used for signing content.
- **Signing key** (Ed25519) -- Derived from the master key via HKDF-SHA256. Shared across all linked devices. Signs posts, interactions, profiles, and server registrations. Rotatable by the master key if a device is compromised.
- **DM key** (X25519) -- Independently derived from the master key via HKDF with its own index. Used for Noise IK handshakes and Double Ratchet encryption. Rotatable independently of the signing key.

The master key signs a `SigningKeyDelegation` binding the signing key to the identity. Peers cache this delegation and verify content signatures against the signing key. This separation means a compromised device's signing key can be rotated without losing the permanent identity.

### Protocols

**Six protocol layers handle all communication:**

- **Gossip** -- Real-time pub/sub. When you post, it broadcasts instantly to anyone following you. Each user has a topic derived from their master public key.
- **Sync** -- Historical pull. When you follow someone, their existing posts are fetched via a custom QUIC protocol with a three-tier streaming protocol (timestamp catch-up, ID diff, or up-to-date). On startup, all followed users are synced in parallel with bounded concurrency.
- **Blobs** -- Content-addressed media storage. Images, videos, and files are stored locally and transferred peer-to-peer using iroh-blobs.
- **DM** -- End-to-end encrypted direct messaging. A Noise IK handshake over QUIC establishes a shared secret between peers, which seeds a Double Ratchet providing per-message forward secrecy with ChaCha20-Poly1305 encryption. Messages are sent directly peer-to-peer with no intermediary, and queued locally for retry when the recipient is offline.
- **Call** -- Peer-to-peer 1:1 voice calls over a dedicated QUIC protocol (`proscenium/call/1`). Audio is captured via cpal, encoded with Opus at 48kHz mono in 20ms frames, and streamed over bidirectional QUIC streams with length-prefixed framing. Call signaling (ring, accept, reject, end) is encrypted via the DM ratchet session.
- **Stage** -- Multi-participant live audio rooms over a dedicated QUIC protocol (`proscenium/stage/1`). A host creates a room and speakers stream audio to the host via QUIC unidirectional streams. The host decodes all speaker streams, mixes them, re-encodes as a single Opus stream, and fans it out to listeners. Speakers receive individual forwarded streams from the host in an SFU model (no mesh). Volunteer relays extend capacity by forwarding the mixed stream to additional listeners. Stream authentication uses SHA256 hash chains with Ed25519 checkpoint signatures.

When following a new user, an `IdentityRequest` is sent to their transport NodeId. The response contains their master pubkey, user key delegation, and profile. This is cached locally so subsequent connections can resolve the master pubkey to reachable transport NodeIds.

All data is persisted in a local SQLite database. The app works offline and syncs when peers are available.

## Features

- Create and delete posts (text + media attachments)
- Likes, reposts, and replies with real-time interaction counts
- Follow/unfollow users by Node ID
- View user profiles with their post history and media filters
- Profile page with your own post history
- Thread view with inline reply composer
- End-to-end encrypted direct messages with typing indicators and read receipts
- DM media attachments (images, videos, files)
- Offline message queuing with automatic retry
- Notifications feed (replies, likes, reposts, new followers)
- Bookmarks (private, local-only saved posts)
- First-run onboarding flow
- Inline reply context showing parent post preview
- Image lightbox for fullscreen viewing
- File downloads for non-media attachments
- Infinite scroll with cursor-based pagination
- Real-time feed updates via gossip
- Unread message badge in navigation
- 60-second auto-sync (pauses when window is hidden)
- Connection status indicator (relay + peer count)
- Confirmation dialogs for destructive actions
- Dark theme UI
- 1:1 voice calls with Opus audio over QUIC
- Stage live audio rooms (host, speakers, listeners) with SFU forwarding
- Stage hand-raising, host muting, co-host delegation, and chat
- Live stages sidebar showing active rooms from followed users
- Echo cancellation (AEC) for stage audio
- Volunteer relay support for scaling stage listener capacity
- Stream authentication with hash-chain + Ed25519 checkpoint signatures
- mDNS and Mainline DHT peer discovery
- Discovery server integration (find users, search posts, trending hashtags)
- Server management in settings (add/remove servers, register with visibility levels)

**Backend state model:** Services (`GossipService`, `DmHandler`, `CallHandler`, `StageHandler`, `PeerHandler`) are self-managing actors behind `Arc` with internal command channels. The Iroh endpoint, blob store, and database are accessed lock-free. A `TaskManager` tracks all background tasks for structured shutdown.

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- [Node.js](https://nodejs.org/) (v18+)
- [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform

## Development

```bash
npm install
npm run tauri dev
```

This starts both the Vite dev server (port 1420) and the Tauri backend.

## Building

```bash
npm run tauri build
```

Produces a native desktop application in `src-tauri/target/release/`.

## Android

### Prerequisites

1. Install [Android Studio](https://developer.android.com/studio)
2. Install the Android SDK (API level 33+) and NDK via Android Studio's SDK Manager
3. Install the Android Rust targets:
   ```bash
   rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
   ```
4. Set environment variables (add to your shell profile):
   ```bash
   export ANDROID_HOME="$HOME/Android/Sdk"
   export NDK_HOME="$ANDROID_HOME/ndk/<version>"
   ```

### Development

Run on a connected device or emulator:

```bash
npm run tauri android dev
```

To view logs:

```bash
adb logcat -s proscenium
```

Use a broader filter if needed (the tag may vary):

```bash
adb logcat | grep -i proscenium
```

### Building

```bash
npm run tauri android build
```

The APK/AAB is output to `src-tauri/gen/android/app/build/outputs/`.

### Notes

- The app uses `tauri-plugin-log` which routes to Android logcat automatically
- Network change detection does not work natively on Android with iroh -- the app polls `endpoint.network_change()` every 30 seconds to keep connectivity alive
- QR code scanning uses `tauri-plugin-barcode-scanner` which requires the CAMERA permission (declared in the manifest)
- Deep links use the `proscenium://` scheme

## Discovery Server

A self-hosted, headless server binary (`server/`) that adds opt-in aggregation, search, and trending to the P2P network. Users register with a server by signing a cryptographic proof of identity. The server subscribes to their gossip topics and indexes their posts in SQLite with FTS5, exposing an HTTP API for search, trending hashtags, user directory, and aggregated feeds.

The server is an overlay -- the P2P layer remains the foundation. Users who never connect to a server lose nothing.

### Running

```bash
cargo build --release --manifest-path server/Cargo.toml
./server/target/release/proscenium-server
```

The server listens on port 3000 by default. Configuration is via environment variables (`PROSCENIUM_PORT`, `PROSCENIUM_DB_PATH`).

### Deploying

A deploy script is provided for uploading to a remote server:

```bash
scripts/deploy-server.sh
```

This builds a static musl binary, uploads it via scp, and restarts the systemd service.

### API

- `GET /api/v1/info` -- Server info (name, version, user/post counts)
- `POST /api/v1/register` -- Register with signed cryptographic proof
- `DELETE /api/v1/register` -- Unregister
- `GET /api/v1/feed` -- Aggregated post feed
- `GET /api/v1/trending` -- Trending hashtags
- `GET /api/v1/users` -- User directory
- `GET /api/v1/users/search?q=` -- Search users
- `GET /api/v1/users/{pubkey}/devices` -- Transport NodeIds for a user's devices
- `GET /api/v1/posts/search?q=` -- Full-text post search

### Registration Visibility

Users choose a visibility level when registering:

- **Public** -- Profile and posts visible to all
- **Listed** -- Profile visible in directory, posts only to followers
- **Private** -- Registered but invisible on server

See [todos/community-server.md](todos/community-server.md) for the full design document.

## Direct Messaging

End-to-end encrypted direct messaging over a custom QUIC protocol (`proscenium/dm/1`). E2E encryption uses X25519 key exchange derived from each user's existing ed25519 identity, with a Noise IK handshake for session establishment and a Double Ratchet for per-message forward secrecy. Messages are encrypted such that only the two participants can read them -- not relay servers, not discovery servers, not anyone.

- Noise IK + Double Ratchet (Signal Protocol pattern) with ChaCha20-Poly1305
- Typing indicators (debounced, sent over encrypted channel)
- Read receipts (sent back to peer on conversation open)
- Media attachments in DMs (images, videos, files)
- Offline message queuing with background retry (60-second outbox flush)
- Delivery acknowledgment over QUIC with real-time status updates
- Conversation list with unread badges and message previews
- Start conversations from any user's profile page

See [todos/direct-messaging.md](todos/direct-messaging.md) for the original design document.

## Voice Calls

Peer-to-peer 1:1 voice calls over a dedicated QUIC protocol (`proscenium/call/1`). Call signaling (ring, accept, reject, end) is encrypted via the DM ratchet session.

- Opus audio codec at 48kHz mono, 20ms frames (960 samples)
- Bidirectional QUIC streams with length-prefixed framing
- Mute/unmute with real-time UI feedback
- Call state machine: Idle, Ringing, InCall

See [todos/voice-video-calling.md](todos/voice-video-calling.md) for the design document.

## Stage (Live Audio Rooms)

Multi-participant live audio rooms built on a dedicated QUIC protocol (`proscenium/stage/1`). Rooms support roles (Host, Co-host, Speaker, Listener) with a host-centric SFU audio architecture.

- **Control plane**: iroh-gossip on a per-room TopicId, with Ed25519-signed control messages
- **Audio plane**: Speakers stream Opus audio to the host via QUIC unidirectional streams. The host mixes all speaker streams into one Opus stream and fans it out to listeners.
- **SFU model**: Speakers receive individual forwarded streams from the host (no peer-to-peer mesh), enabling efficient bandwidth usage
- **Relay hierarchy**: Volunteer relays connect to the host (or upstream relay), receive the mixed stream, and fan it out to additional listeners. A TopologyManager assigns listeners to the host or a relay based on capacity.
- **Stream authentication**: SHA256 hash chain with Ed25519 signature checkpoints every 50 frames (~1 second), ensuring listeners can verify the audio source
- **Echo cancellation**: AEC applied to stage audio to prevent feedback loops
- **Stage announcements**: Broadcast on the host's gossip topic so followers see active rooms in the live stages sidebar
- Hand-raising, host muting, speaker promotion/demotion, co-host delegation, kick/ban, in-room chat

See [docs/spaces-design.md](docs/spaces-design.md) for the full design document.

## Linked Devices (In Progress)

Link multiple devices (phone, desktop, tablet) to a single identity. The three-tier key hierarchy (master / signing / transport) is implemented: a master key is the permanent identity, a derived signing key handles content signing and DM encryption across all devices, and per-device transport keys provide unique iroh NodeIds. The master key enables secure signing key rotation if a device is compromised without losing the identity.

Key hierarchy, identity resolution, delegation, device pairing, and cross-device data sync are implemented. Remaining work covers key rotation and revocation.

See [todos/linked-devices.md](todos/linked-devices.md) for the full design document.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).

## License

MIT
