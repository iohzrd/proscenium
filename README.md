# Iroh Social (working title)

A decentralized peer-to-peer social network built with [Iroh](https://iroh.computer/), [Tauri 2](https://tauri.app/), and [SvelteKit 5](https://svelte.dev/).

Successor to [follow](https://github.com/iohzrd/follow) and [identia](https://github.com/iohzrd/identia), rebuilt on iroh's QUIC transport with end-to-end encrypted messaging.

Every user runs their own node. Posts, profiles, and follows are stored locally. Peers exchange data directly -- no central server, no accounts, no passwords. Optional community servers provide discovery, search, and trending without compromising the P2P foundation.

## How It Works

Each node gets a permanent cryptographic identity (stored in `identity.key`). Your public key is your Node ID -- share it with others so they can follow you.

**Four protocol layers handle all communication:**

- **Gossip** -- Real-time pub/sub. When you post, it broadcasts instantly to anyone following you. Each user has a topic derived from their public key.
- **Sync** -- Historical pull. When you follow someone, their existing posts are fetched via a custom QUIC protocol with a three-tier streaming protocol (timestamp catch-up, ID diff, or up-to-date). On startup, all followed users are synced in parallel with bounded concurrency.
- **Blobs** -- Content-addressed media storage. Images, videos, and files are stored locally and transferred peer-to-peer using iroh-blobs.
- **DM** -- End-to-end encrypted direct messaging. A Noise IK handshake over QUIC establishes a shared secret between peers, which seeds a Double Ratchet providing per-message forward secrecy with ChaCha20-Poly1305 encryption. Messages are sent directly peer-to-peer with no intermediary, and queued locally for retry when the recipient is offline.

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
- Community server integration (discover users, search posts, trending hashtags)
- Server management in settings (add/remove servers, register with visibility levels)

**Backend state model:** Only the `FeedManager` (which manages gossip subscriptions) is behind a mutex. All other state -- the Iroh endpoint, blob store, database -- is accessed lock-free, so blob fetches and feed queries never block each other.

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
adb logcat -s iroh-tauri-app
```

Use a broader filter if needed (the tag may vary):

```bash
adb logcat | grep -i iroh
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
- Deep links use the `iroh-social://` scheme

## Community Server

A self-hosted, headless server binary (`server/`) that adds opt-in aggregation, discovery, full-text search, and trending to the P2P network. Users register with a server by signing a cryptographic proof of identity. The server subscribes to their gossip topics and indexes their posts in SQLite with FTS5, exposing an HTTP API for search, trending hashtags, user directory, and aggregated feeds.

The server is an overlay -- the P2P layer remains the foundation. Users who never connect to a server lose nothing.

### Running

```bash
cargo build --release --manifest-path server/Cargo.toml
./server/target/release/iroh-social-server
```

The server listens on port 3000 by default. Configuration is via environment variables (`IROH_SOCIAL_PORT`, `IROH_SOCIAL_DB_PATH`).

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
- `GET /api/v1/posts/search?q=` -- Full-text post search

### Registration Visibility

Users choose a visibility level when registering:

- **Public** -- Profile and posts visible to all
- **Listed** -- Profile visible in directory, posts only to followers
- **Private** -- Registered but invisible on server

See [todos/community-server.md](todos/community-server.md) for the full design document.

## Direct Messaging

End-to-end encrypted direct messaging over a custom QUIC protocol (`iroh-social/dm/1`). E2E encryption uses X25519 key exchange derived from each user's existing ed25519 identity, with a Noise IK handshake for session establishment and a Double Ratchet for per-message forward secrecy. Messages are encrypted such that only the two participants can read them -- not relay servers, not community servers, not anyone.

- Noise IK + Double Ratchet (Signal Protocol pattern) with ChaCha20-Poly1305
- Typing indicators (debounced, sent over encrypted channel)
- Read receipts (sent back to peer on conversation open)
- Media attachments in DMs (images, videos, files)
- Offline message queuing with background retry (60-second outbox flush)
- Delivery acknowledgment over QUIC with real-time status updates
- Conversation list with unread badges and message previews
- Start conversations from any user's profile page

See [todos/direct-messaging.md](todos/direct-messaging.md) for the original design document.

## Voice/Video Calls (Planned)

Peer-to-peer voice and video calls, with call signaling encrypted via the DM ratchet session.

- Voice calls with Opus audio codec over multiplexed QUIC streams
- Video calls with VP9 codec and adaptive bitrate

See [todos/voice-video-calling.md](todos/voice-video-calling.md) for the design document.

## Linked Devices (Planned)

Link multiple devices to a single identity, similar to Signal's linked devices. A primary device holds the master keypair and authorizes secondaries via QR code pairing over an encrypted channel. Linked devices share the social graph, message history, and profile.

See [todos/linked-devices.md](todos/linked-devices.md) for the design document.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).

## License

MIT
