# Proscenium

**Alpha software. Expect breaking changes, data loss, and rough edges. Nothing is stable yet.**

**Privacy notice:** Proscenium connects peers directly over QUIC. It is NOT an anonymity network. Peers you communicate with can see your IP address.

**Protocol version 2:** Wire formats, signature formats and post ids changed after 0.2.31. A version 2 node does not connect to older nodes, and content signed by older versions does not verify. Discovery servers drop their version 1 tables on upgrade; users register again.

A decentralized peer-to-peer social network built with [Iroh](https://iroh.computer/), [Tauri 2](https://tauri.app/), and [SvelteKit 5](https://svelte.dev/).

Successor to [follow](https://github.com/iohzrd/follow) and [identia](https://github.com/iohzrd/identia), rebuilt on iroh's QUIC transport with end-to-end encrypted messaging.

Every user runs their own node. Posts, profiles, and follows are stored locally. Peers exchange data directly -- no central server, no accounts, no passwords. Optional discovery servers provide search, trending, and user directories without compromising the P2P foundation.

## Iroh

[Iroh](https://iroh.computer/) is a networking library that gives every node a persistent identity (an Ed25519 keypair) and connects peers directly over QUIC, punching through NATs with the help of relay servers. Peers discover each other via DNS, mDNS on local networks, and the Mainline DHT. Iroh provides two higher-level primitives on top of raw QUIC connections: **iroh-gossip** for real-time pub/sub broadcast, and **iroh-blobs** for content-addressed data transfer. Proscenium uses all of these -- gossip for live feed updates, blobs for media, and direct QUIC connections for DMs, calls, and stage audio.

## How It Works

### Identity and Key Architecture

Each user has one master key, and each device its own transport key:

- **Master key** (Ed25519) -- The identity. The master public key is what others follow. The master key signs all of the user's content: posts, interactions, profiles, deletions, device lists, follow requests, server registrations and Stage messages. Stored in `master_key.key` (mode 0600) on every device of the user. A 24-word BIP39 recovery phrase encodes it.
- **Transport key** (Ed25519) -- Generated at random on each device and stored in `transport_key.key` (mode 0600). It is the iroh NodeId of that device, and its X25519 form decrypts the DMs sent to that device. Never signs content. Recovery from the phrase generates a new one.

Content is verified against the public key in its `author` field, so it verifies without other cached data. There is no key rotation: a leaked master key can only be replaced by a new identity.

A user's devices are listed in a `LinkedDevicesAnnouncement` signed by the master key. An identity response is accepted only if the device list verifies and contains the NodeId that sent the response. A NodeId is therefore bound to a master key by signature, not by claim.

Every signed byte string is a JSON array that starts with a type tag (`proscenium/<type>/2`), so a signature made for one message type does not verify as another. Post ids are a SHA-256 hash over the post's other fields, including the author. Profiles carry a signed `updated_at`; an older profile does not replace a newer one.

### Protocols

All ALPNs are version 2 (`proscenium/{peer,dm,call,stage}/2`).

- **Gossip** -- Real-time pub/sub (iroh-gossip, 64 KiB frames). Each user has a topic derived from the master public key. Followers receive posts, deletions, profile updates, device lists and Stage announcements. A message is stored only if the topic owner is its author and signed it.
- **Sync** -- Historical pull over the peer ALPN. A node serves only its own posts, and only to the user's other devices and to requesters its visibility allows (Public: anyone; Listed: approved followers; Private: mutuals). The requester sends counts and a digest of the post ids it holds. The responder answers up-to-date, sends newer posts, or asks for the id list and sends the missing posts. Signed deletion records (tombstones) are part of the exchange, so a deleted post stays deleted and deletions reach followers that were offline. The responder's signed device list travels with every sync and push, so a follower learns device list changes it missed.
- **Push** -- Listed and Private users deliver new content directly to their followers. A node accepts a push only from a user it follows.
- **Blobs** -- Content-addressed media (iroh-blobs). The size of a remote blob is checked before download; blobs over 50 MiB are rejected. Media over 10 MiB is fetched only after a click.
- **DM** -- End-to-end encrypted direct messages. See "Direct Messaging".
- **Call** -- 1:1 voice calls. Opus at 48 kHz mono in 20 ms frames over a QUIC bi-stream. Signaling travels as DMs. The audio connection is accepted only after the local user accepted the call, and both sides prove knowledge of the call id before audio starts.
- **Stage** -- Live audio rooms. See "Stage (Live Audio Rooms)".

When a user follows a NodeId, the node asks that NodeId for its identity, verifies the response as described above, and caches it. The caller can pass the expected master public key; a mismatch is an error.

All data is persisted in a local SQLite database. The app works offline and syncs when peers are available.

## Features

- Create and delete posts (text + media attachments)
- Likes, reposts, and replies with real-time interaction counts
- Follow/unfollow users by Node ID, with the expected identity checked
- Follow requests for Listed and Private profiles. A Private user approves requests from users they follow, then sends a follow request back, so two Private users who follow each other become mutuals in either order.
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
- Automatic sync request every 10 minutes while the window is visible, and when it becomes visible
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

**Backend state model:** Services (`GossipService`, `DmHandler`, `CallHandler`, `StageHandler`, `PeerHandler`) are cloneable handles. The network stack (endpoint, router, services) can be stopped and rebuilt in place when the identity changes (recovery, device linking). One cancellation token stops the background loops; the router is shut down on exit so peers receive a QUIC close.

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- [Node.js](https://nodejs.org/) (v18+)
- [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform

### Linux (GNOME)

The system tray icon requires `libayatana-appindicator`. GNOME does not display tray icons by default -- you need the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/):

```bash
# Arch
pacman -S gnome-shell-extension-appindicator
# Ubuntu/Debian
apt install gnome-shell-extension-appindicator
# Fedora
dnf install gnome-shell-extension-appindicator
```

Enable and restart your session:

```bash
gnome-extensions enable appindicatorsupport@rgcjonas.gmail.com
# Log out and back in
```

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

A self-hosted server binary (`server/`) that adds opt-in search, trending and a user directory. A user registers with a request signed by the master key; the server verifies the signature, the timestamp, and that the request names this server. The server then subscribes to the user's gossip topic, syncs the user's posts, and indexes them in SQLite with FTS5. It stores and serves content only for users whose registration and profile are both Public.

The server is an overlay. Users who never connect to a server lose nothing. Clients verify the signatures of posts a server returns and discard posts that do not verify.

### Running

```bash
cargo build --release --manifest-path server/Cargo.toml
./server/target/release/proscenium-server --config config.toml
```

A `config.toml` is required; unknown keys are errors. `[server] public_url` is mandatory and must equal the URL users add in the client (compared without a trailing slash). Flags: `--config`, `--data-dir`, `--port`.

| Section | Keys |
| --- | --- |
| `[server]` | `name`, `description`, `listen_addr`, `data_dir`, `public_url`, `registration_open`, `trust_forwarded_for` |
| `[limits]` | `max_registered_users`, `max_posts_per_user`, `max_interactions_per_user`, `retention_days`, `max_connections`, `max_concurrent_requests`, `register_requests_per_minute` |
| `[sync]` | `interval_minutes`, `startup_sync`, `max_concurrent_syncs` |
| `[trending]` | `recompute_interval_minutes`, `window_hours` |

The server speaks plain HTTP/1. Terminate TLS in a reverse proxy and set `trust_forwarded_for = true` so the per-address rate limit sees client addresses.

### Deploying

```bash
DEPLOY_HOST=user@host DEPLOY_HEALTH_URL=https://example.org/api/v1/info scripts/deploy-server.sh
```

Builds a static musl binary, uploads it to a temporary directory on the host, verifies its checksum, replaces the binary, restarts the systemd service, and restores the previous binary if the health check fails. Optional: `DEPLOY_SSH_KEY`, `DEPLOY_DIR`, `DEPLOY_SERVICE`.

### API

- `GET /api/v1/info` -- Server info
- `POST /api/v1/register` -- Register (signed)
- `PUT /api/v1/register` -- Replace the stored profile (signed profile inside a signed request)
- `DELETE /api/v1/register` -- Unregister (signed)
- `GET /api/v1/feed` -- Aggregated post feed
- `GET /api/v1/trending` -- Trending hashtags
- `GET /api/v1/users` -- User directory
- `GET /api/v1/users/search?q=` -- Search users
- `GET /api/v1/users/{pubkey}` -- One user
- `GET /api/v1/users/{pubkey}/posts` -- Posts of one user
- `GET /api/v1/users/{pubkey}/devices` -- Transport NodeIds of a registered user (404 without an active registration)
- `GET /api/v1/posts/search?q=` -- Full-text post search
- `GET /api/v1/posts/{author}/{id}/interactions` -- Interactions of one post
- HTML pages: `/`, `/users`, `/user/{pubkey}`, `/post/{author}/{id}`, `/trending`

Signed requests are rejected if the timestamp is more than 5 minutes from the server clock or not greater than the last accepted timestamp of that user.

### Registration Visibility

- **Public** -- Profile and posts are stored and served.
- **Listed** -- Profile is in the directory; posts are not stored.
- **Private** -- Rejected. A Private profile received later removes the registration.

## Direct Messaging

End-to-end encrypted direct messages over a QUIC protocol (`proscenium/dm/2`). Only the two participants' devices can read them.

### Encryption Scheme

Every payload (message, receipt, typing signal, call signal) is encrypted separately to each device it is sent to. There is no session state.

- **Keys**: the sending and receiving keys are the X25519 forms of the two devices' transport keys: the scalar of the Ed25519 seed, and the Montgomery form of the NodeId.
- **Encryption**: HPKE (RFC 9180) in Auth mode with DHKEM(X25519, HKDF-SHA256), HKDF-SHA256 and ChaCha20-Poly1305. Each envelope has its own ephemeral key. The associated data covers the sender, the sending device, the recipient and the receiving device.
- **Envelope**: the ciphertext, the encapsulated key, the four sender and recipient fields, and the sender's signed device list.
- **Checks**: the receiving device accepts an envelope only if the QUIC connection comes from the sending device, the recipient fields name the local user and this device, the device list verifies, belongs to the sender and lists the sending device, the sender is not blocked, and the ciphertext decrypts. The device list is then cached.

**At rest.** Message text is stored unencrypted in the local database. Key files are created with mode 0600 in a 0700 directory; there is no passphrase.

### Delivery and Queuing

Messages, receipts and typing signals go to every device in the recipient's cached device list at once. Each device answers with an acknowledgement; the sender marks a message delivered when one device acknowledges it. Call signals go to one device: the offer to the first device that answers, every later signal to the device of the call.

Undelivered messages are queued as plaintext payloads and encrypted at send time. Retries back off from 30 seconds to 15 minutes; entries older than 7 days are dropped. The message view can request an immediate retry.

Device sync copies messages between the devices of one user, so a device that was offline when a message arrived, or that did not send it, receives it from a sibling device.

### Features

- Typing indicators and read receipts
- Media attachments (images, videos, files)
- Delivery state from the acknowledgement
- Conversation list with unread badges and previews
- Blocked users are rejected by transport NodeId and again by identity after the envelope is checked

## Voice Calls

1:1 voice calls over a QUIC protocol (`proscenium/call/2`).

- Signaling (offer, answer, reject, hangup) travels as DMs. The offer goes to the first device of the callee that answers; the answer, and every later signal, come from and go to the two devices of the call, and the caller opens the audio connection to the device that answered.
- Call phases: Ringing (caller), Incoming, Connecting, Active. The audio connection is served only for a call the local user accepted. Microphone capture starts in phase Active.
- Each side sends a proof derived from the call id as the first bytes of the audio stream; audio starts only after the other side's proof verified.
- Timeouts: 30 seconds ringing, 15 seconds for the audio connection after the answer.
- Opus at 48 kHz mono, 20 ms frames, echo cancellation, mute.

## Stage (Live Audio Rooms)

Multi-participant audio rooms over a QUIC protocol (`proscenium/stage/2`) with roles Host, Co-host, Speaker and Listener.

- **Identity**: participants are identified by master public key, and the master key signs their control messages. A control message from a signer that has not sent a verified presence message is discarded.
- **Roles**: only messages signed by the host (or, for promote, demote, mute and kick, by a co-host the host appointed) change roles. The host broadcasts a signed room-state snapshot periodically and on every change. A promotion starts the microphone only if the local user raised a hand.
- **Control plane**: iroh-gossip on a per-room topic. Signatures cover the stage id and a timestamp that must increase per signer.
- **Audio plane**: speakers stream Opus to the host. The host accepts a speaker connection only from the NodeId of a participant with a speaking role, mixes the streams and fans the mix out to listeners. Speakers receive individual forwarded streams.
- **Relays**: a speaker or co-host can relay the mixed stream to additional listeners.
- **Stream authentication**: a SHA-256 hash chain over frames with signed checkpoints that cover the stage id and a sequence number. A listener stops playback from a source that sends too many frames without a verifiable checkpoint, and does not return to a relay that failed verification.
- **Moderation**: hand raising, promote and demote, host mute, kick, ban, in-room chat. Demotion stops the speaker's capture.
- **Announcements**: signed by the host and broadcast on the host's feed topic.

## Linked Devices

Several devices can share one identity.

- **Pairing**: the existing device shows a QR code (or its text form for a device without a camera) that holds its NodeId and a one-time secret valid for 60 seconds. The new device dials that NodeId, so QUIC authenticates the existing device, and sends the secret and its device name. The existing device compares the secret in constant time, sends the link bundle, and registers the NodeId of the connection as the new device after the new device reported that it stored the bundle.
- **Bundle**: the master key, the signed device list, profile, follows, moderation entries, bookmarks and the device lists of followed users. Linking replaces the identity and all local data of the new device, after confirmation in a native dialog. The new device keeps its transport key.
- **Keys**: every device holds the master key and its own random transport key. A device cannot be removed from the identity.
- **Device sync**: each side sends its device list signed by the master key, and the other side continues only if the list is signed by its own master key and contains the NodeId of the connection. Both sides store the devices of the other's list. Devices exchange follows, moderation entries and bookmarks (last writer wins), direct messages (by id, among the newest 50,000) and deletions of own interactions. After that, each side fetches the other's own posts, interactions and post deletions with the sync protocol; a device serves that sync to its other devices whatever its visibility.
- **Not replicated**: read state of direct messages.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).

## License

MIT
