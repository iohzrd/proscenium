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
- **Transport key** (Ed25519) -- Generated at random on each device and stored in `transport_key.key` (mode 0600). It is the iroh NodeId of that device. Never signs content. Recovery from the phrase and linking generate a new one.

Content is verified against the public key in its `author` field, so it verifies without other cached data. There is no key rotation: a leaked master key can only be replaced by a new identity.

A user's devices, and the NodeIds of removed devices, are listed in a `LinkedDevicesAnnouncement` signed by the master key. The signature proves only that the master named those NodeIds, not that the devices agreed, so a NodeId is mapped to a master (for blocking, push, sync authorization and calls) only when that NodeId itself delivered a list of that master that lists it, over a connection QUIC authenticates. Lists from gossip or a link bundle are used to dial a user's devices, never to identify an incoming connection. A list's version is its creation time, raised above the highest version of the user's own lists that the device has seen. A version more than 5 minutes after the receiver's clock is ahead of it: such a list still authenticates its device and is cached when no current list is, but never replaces a current list, and any current list replaces it. Otherwise the higher version wins, and between equal versions the greater signature string, so two devices that sign different lists with the same version leave every cache with the same list whatever the arrival order.

Every signed byte string is a JSON array that starts with a type tag (`proscenium/<type>/2`), so a signature made for one message type does not verify as another. Post ids are a SHA-256 hash over the post's other fields, including the author. Profiles carry a signed `updated_at`; a profile replaces the stored one only if its `(updated_at, signature)` is greater, so an older profile never replaces a newer one and two profiles with the same `updated_at` leave the same stored profile in any arrival order. Tests in `src-tauri/src/ingest.rs` and `server/src/storage/tests.rs` apply posts, deletions, likes, profiles and device lists in every order, with duplicates, and require one stored state.

### Protocols

The ALPNs are `proscenium/peer/2`, `proscenium/dm/3`, `proscenium/call/3` and `proscenium/stage/2`.

- **Gossip** -- Real-time pub/sub (iroh-gossip, 64 KiB frames). Each user has a topic derived from the master public key. Followers receive posts, deletions, profile updates, device lists and Stage announcements. A message is stored only if the topic owner is its author and signed it.
- **Sync** -- Historical pull over the peer ALPN. A node serves its own posts to the user's other devices and to requesters its visibility allows (Public: anyone; Listed: approved followers; Private: mutuals). The requester sends its signed device list, counts and a digest of the post ids it holds, and the earliest timestamp it stores (`since`); the responder considers only content at or after `since`. One sync per NodeId is served at a time, for at most 5 minutes. Connections and syncs from NodeIds that are neither the user's devices (including a device it linked that has not synced yet) nor devices of users the local user follows or approved as followers have their own limits (32 connections, 4 syncs, 60 s per sync), so generated NodeIds cannot take the slots of known peers and hold the slots of their own class only briefly. A profile view of a user the local user does not follow is recorded before its sync and trimmed after it, also after a failed sync, and at the next start. A blocked user is refused at any visibility, and blocking withdraws its approval as follower. Syncing the profile of a user the local user does not follow keeps that user's newest 2,000 posts and interactions, for at most 50 such users and 7 days. The responder answers up-to-date, sends newer posts, or asks for the id list and sends the missing posts. Signed deletion records (tombstones) are part of the exchange, so deletions reach followers that were offline, and a deleted post is not stored again while its deletion is among the 10,000 newest of its author. The responder's signed device list travels with every sync and push, so a follower learns device list changes it missed.
- **Mirrors** -- When none of an author's devices answers a sync of a followed author whose stored profile is Public, the requester asks up to 3 of its neighbors on the author's gossip topic (other followers that are online; stored discovery servers are skipped), each for at most 60 s, and sends them no device list. Such a node serves the author's stored posts, post deletions and signed profile only if its own profile is Public and it shares its follow list (serving reveals that it follows the author), it follows the author and has not muted or blocked the author, it holds the author's profile as Public and a device of the author sent that Public profile in a sync within the last 7 days, and the requester is not mapped to a user it blocked. The author signs all of it, so the requester verifies it as it verifies the author's own response. A mirror sends no interactions (interaction deletions are not signed records it could relay; the requester discards any it receives), no device list and no stage announcement. The requester applies a mirror's signed post deletions and deletes nothing else because of its response. Every push carries the author's current profile, stored before the pushed posts, so a follower that holds non-Public posts also holds the non-Public profile. A follower from the author's Public period that missed a switch to Listed or Private receives neither push nor sync from the author, and stops serving it within 7 days.
- **Push** -- Listed and Private users deliver new content directly to their followers. A node accepts a push only from a user it follows, and a Private node only from a mutual.
- **Blobs** -- Content-addressed media (iroh-blobs). The size of a remote blob is proven by the provider before download; blobs over 50 MiB are rejected. Media and avatars load without a click only if the declared size and the proven size are at most 10 MiB. Such automatic loads and the loads the user requests have separate limits of 4 transfers each, and a load connects to the provider only once it holds a transfer slot; at most 32 automatic loads wait or run, and a further one fails at once and shows its Load button. An automatic load ends after 2 minutes, a requested one after 10, and either ends when fewer than 64 KiB arrive within 30 s. A provider that does not connect, does not answer the size request in time, or sends fewer than 64 KiB within 30 s is not dialed again by automatic loads for a minute; a load the user requests dials it anyway. A transfer that ends at the time limit while it still receives data does not count against the provider. The webview passes a file to the blob store in chunks of 1 MiB (raw on desktop; base64 on Android, whose IPC carries bytes as JSON), a pasted image as PNG, and "Save as" leaves the writing of the file to the backend, so no IPC message holds a whole file.
- **DM** -- End-to-end encrypted direct messages. See "Direct Messaging".
- **Call** -- 1:1 voice calls. Opus at 48 kHz mono in 20 ms frames over a QUIC bi-stream. Signaling travels as DMs. The audio connection is accepted only after the local user accepted the call, and both sides prove knowledge of the call id before audio starts.
- **Stage** -- Live audio rooms. See "Stage (Live Audio Rooms)".

When a user follows a NodeId, the node asks that NodeId for its identity, verifies the response as described above, and caches it. The caller can pass the expected master public key; a mismatch is an error.

All data is persisted in a local SQLite database. The app works offline and syncs when peers are available.

## Features

- Create and delete posts (text + media attachments)
- Likes, reposts, and replies with real-time interaction counts
- Follow/unfollow users by Node ID, with the expected identity checked
- Follow requests for Listed and Private profiles. The follow is stored when the request is sent and removed if the request is denied, so content arrives once the request is approved. A Private user approves requests from users they follow, then sends a follow request back; a request from a user they do not follow is stored as pending and shown, and following that user later approves it, so a Private user and anyone who follows them become mutuals in either order. The follow and the request can be on different devices of the user: after a device sync merges either, the pending requests of followed users are approved. The approval is made only once the user's own follow request was not denied. A node returns a Listed or Private profile only to approved followers (Private: mutuals) and the user's other devices. At most 500 pending requests are kept; a new request replaces the oldest. At most 20 new pending requests per hour are stored from users the local user does not follow, so generated identities take 25 hours to replace the stored requests; beyond that the requester is answered "rate limited", nothing is stored, and its follow is removed.
- View user profiles with their post history and media filters
- Profile page with your own post history
- Thread view with inline reply composer
- End-to-end encrypted direct messages with typing indicators and read receipts
- DM media attachments (images, videos, files)
- Offline message queuing with automatic retry
- Notifications feed (replies, likes, reposts, new followers)
- Bookmark state on posts (private, copied between the user's devices; there is no list view yet)
- First-run onboarding flow
- Inline reply context showing parent post preview
- Image lightbox for fullscreen viewing
- File downloads for non-media attachments
- Media downloads: at most 4 automatic and 4 requested transfers at once, each started after the provider connected; a provider that could not be reached is not dialed again by automatic loads for a minute
- Desktop: closing the window keeps the app running; starting it again shows the window; quit from Preferences or the tray
- Infinite scroll with cursor-based pagination
- Real-time feed updates via gossip
- Unread message badge in navigation
- Automatic sync request every 10 minutes while the window is visible, and when it becomes visible
- Connection status indicator (relay + peer count)
- Confirmation dialogs for destructive actions
- Dark theme UI
- 1:1 voice calls with Opus audio over QUIC
- Stage live audio rooms (host, speakers, listeners) with SFU forwarding
- Stage hand-raising, host muting, and chat
- Live stages sidebar showing active rooms from followed users
- Echo cancellation (AEC) for stage audio
- Relay support in the Stage protocol for scaling listener capacity (the app offers no control to volunteer as a relay yet)
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
2. Install the Android SDK (API level 36) and NDK via Android Studio's SDK Manager
3. Install the Android Rust targets:
   ```bash
   rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
   ```
4. Set environment variables (add to your shell profile):
   ```bash
   export ANDROID_HOME="$HOME/Android/Sdk"
   export NDK_HOME="$ANDROID_HOME/ndk/<version>"
   export ANDROID_NDK_HOME="$NDK_HOME"  # read by the opusic-sys build script
   # Gradle 9.6 runs on JDK 17 to 25; Android Studio bundles one.
   export JAVA_HOME=/opt/android-studio/jbr
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
npm run tauri android build -- --aab
```

The bundle is written to `src-tauri/gen/android/app/build/outputs/bundle/universalRelease/app-universal-release.aab`.

Release builds are signed with the upload key named in `src-tauri/gen/android/keystore.properties` (gitignored). Without that file, release builds are unsigned. To create the key:

```bash
keytool -genkeypair -v -keystore ~/.android/proscenium-upload.jks -storetype PKCS12 \
  -keyalg RSA -keysize 4096 -validity 10000 -alias upload -dname "CN=Proscenium"
```

and write `keystore.properties`:

```properties
keyAlias=upload
password=<keystore password>
storeFile=/home/<user>/.android/proscenium-upload.jks
```

Google Play re-signs the app with its app signing key; the upload key only authenticates uploads, and Play can replace a lost upload key.

### Notes

- The app uses `tauri-plugin-log` which routes to Android logcat automatically
- Network change detection does not work natively on Android with iroh -- the app polls `endpoint.network_change()` every 30 seconds to keep connectivity alive
- QR code scanning uses `tauri-plugin-barcode-scanner` which requires the CAMERA permission (declared in the manifest)
- Deep links use the `proscenium://` scheme
- App data, which holds the master and transport keys, is excluded from cloud backup and device-to-device transfer (`android:allowBackup="false"` and `res/xml/data_extraction_rules.xml`)
- The application ID is `com.proscenium.app`

## Terms of Use and Privacy Policy

[docs/terms-of-use.md](docs/terms-of-use.md) and [docs/privacy-policy.md](docs/privacy-policy.md) are bundled into the app and shown from Preferences. The app shows them over every screen until they are accepted; the acceptance is stored with the terms' "Last updated" date, so a new date asks again, and "Delete all data" removes it. The app renders a Markdown subset (`#` and `##` headings, paragraphs, `- ` lists, no inline formatting); keep both files to that subset.

## Discovery Server

A self-hosted server binary (`server/`) that adds opt-in search, trending and a user directory. A user registers with a request signed by the master key; the server verifies the signature, the timestamp, and that the request names this server. The server then subscribes to the user's gossip topic, syncs the user's posts, and indexes them in SQLite with FTS5. It stores and serves content only for users whose registration and profile are both Public. On each subscribed topic, each sender (the NodeId that delivered the message) may deliver 60 messages at once and 1 per second on average; further messages of that sender are discarded unread, and the server's next sync of the author fetches their content. The author's devices always have a budget of their own; of the other senders, 16 per topic have one and the rest share one. Every `interval_minutes` the server syncs each Public user, at most `max_concurrent_syncs` at once for users whose last sync completed, and at most a quarter of that (at least one) for users whose last sync failed, who never completed one in their registration, or whose sync exceeded its 10-minute limit within the last 7 days. After a failed sync a user is not synced again for 5 minutes, doubling per further failure up to 6 hours, so registrations whose devices do not answer, or answer slowly, cannot delay the syncs of other users. The profile setup screen offers to add and register with `https://discovery.identia.app`; the option starts switched on for Public and Listed profiles and is unavailable for Private ones, which servers refuse.

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

### Provisioning

```bash
PROVISION_HOST=root@host PUBLIC_HOSTNAME=discovery.example.org scripts/provision-server.sh
```

Prepares a fresh Debian or Ubuntu host once: installs Caddy from its package repository as the TLS reverse proxy (it obtains the certificate when the name resolves to the host), opens 80, 443 and UDP in ufw if ufw is active, creates the `proscenium` user, `/opt/proscenium` with a config whose `public_url` is `https://$PUBLIC_HOSTNAME` and whose server listens on `127.0.0.1:3000`, and the systemd unit, then installs and starts the server and checks that it answers. An existing config, binary or database is kept. Optional: `PROVISION_SSH_KEY`, `SERVER_NAME`, `SERVER_DESCRIPTION`, `DEPLOY_DIR`, `DEPLOY_SERVICE`, `SERVER_PORT`.

### Deploying

```bash
DEPLOY_HOST=user@host DEPLOY_HEALTH_URL=https://example.org/api/v1/info scripts/deploy-server.sh
```

Builds a static musl binary, uploads it to a temporary directory on the host, verifies its checksum, stops the systemd service, copies the database files, replaces the binary and starts the service. If the service does not start or the health check fails, the previous binary and the copied database are restored. Optional: `DEPLOY_SSH_KEY`, `DEPLOY_DIR`, `DEPLOY_DATA_DIR` (default `$DEPLOY_DIR/data`), `DEPLOY_SERVICE`.

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

- **Public** -- Profile and posts are stored and served. At `max_posts_per_user` the newest posts are kept: the server requests only posts at or after its oldest kept post and its retention floor. Signed deletions of posts and interactions are kept, so a replayed original is not stored again.
- **Listed** -- Profile is in the directory; posts are not stored.
- **Private** -- Rejected. A Private profile received later removes the registration.

## Direct Messaging

Direct messages over a QUIC protocol (`proscenium/dm/3`), sent from one device to another. Only the two participants' devices can read them.

### Transport and Checks

Every payload (message, receipt, typing signal, call signal) is sent on its own QUIC connection to each device it is for. There is no session state and no encryption layer of its own: the QUIC connection (TLS 1.3 with the NodeIds of both devices, ephemeral key exchange) encrypts the payload and authenticates both devices, and no server or other device stores or forwards it.

- **Envelope**: the recipient's master pubkey, the sender's signed device list and the payload.
- **Checks**: the receiving device accepts an envelope only if the recipient is the local user, the device list verifies, is not the local user's, and lists the NodeId of the connection, the sender (the master of that list) is not blocked, and the payload is valid. The device list is cached after a stored message, or for another payload if a conversation with the sender exists or the sender is followed.
- **Unknown senders**: a sender the local user does not follow, has not approved as follower and has not written to is a stranger. A stranger's conversation holds at most 50 of its messages; further ones are refused. At most 100 stranger conversations are kept: a new one deletes the one with the oldest last message. At most 20 new stranger conversations are accepted per hour over all senders, so one sender with many generated identities cannot replace them all at once. A first message beyond that limit is answered "deferred": the sender keeps it in its outbox and sends it again with its usual backoff. A sender with many generated identities can take each slot as it frees, so a deferred message can wait until its outbox drops it after 7 days. Messages that device sync copies from another device of the user are subject to the same limits.
- **Notifications**: each message of a contact raises an OS notification with its text. Of a stranger, only the first message of a new conversation raises one, at most 3 per hour, without its text ("New message request").
- **Connections**: at most 64 incoming connections from devices of contacts and 16 from other NodeIds are handled at once, and 4 per NodeId. A NodeId is a contact's device if the cached device list of a user who is no stranger lists it, so generated NodeIds cannot take the connections of contacts.

**At rest.** Message text is stored unencrypted in the local database. Key files are created with mode 0600 in a 0700 directory; there is no passphrase.

### Delivery and Queuing

Messages, receipts and typing signals go to every device in the recipient's cached device list at once. Each device answers with an acknowledgement; the sender marks a message delivered when one device acknowledges it. Call signals go to one device: the offer to the first device that answers, every later signal to the device of the call.

Undelivered messages are queued with their payload. Retries back off from 30 seconds to 15 minutes; entries older than 7 days are dropped. The message view can request an immediate retry.

Device sync copies messages between the devices of one user, so a device that was offline when a message arrived, or that did not send it, receives it from a sibling device.

### Features

- Typing indicators and read receipts. A read receipt names the newest message read and covers every earlier one, so opening a conversation sends one receipt.
- Media attachments (images, videos, files)
- Delivery state from the acknowledgement
- Conversation list with unread badges and previews
- Blocked users are rejected by transport NodeId and again by identity after the envelope is checked

## Voice Calls

1:1 voice calls over a QUIC protocol (`proscenium/call/3`).

- An offer rings only if the caller is no stranger (the local user follows the caller, approved them as follower or has written to them); any other offer is ignored without a reply.
- Signaling (offer, answer, reject, hangup) travels as DMs. The offer goes to the first device of the callee that answers; the answer, and every later signal, come from and go to the two devices of the call, and the caller opens the audio connection to the device that answered.
- Call phases: Ringing (caller), Incoming, Connecting, Active. The audio connection is served only for a call the local user accepted. Microphone capture starts in phase Active.
- The callee serves the audio connection only from the device that sent the offer, and only for the call it accepted; the caller dials the device that answered. QUIC authenticates both NodeIds, and only one call is active at a time, so the connection belongs to that call without a further check.
- Timeouts: 30 seconds ringing, 15 seconds for the audio connection after the answer.
- Opus at 48 kHz mono, 20 ms frames, echo cancellation, mute.

## Stage (Live Audio Rooms)

Multi-participant audio rooms over a QUIC protocol (`proscenium/stage/2`) with roles Host, Co-host, Speaker and Listener.

- **Identity**: participants are identified by master public key, and the master key signs their control messages. A control message from a signer that has not sent a verified presence message is discarded.
- **Roles**: only messages signed by the host (or, for promote, demote, mute and kick, by a co-host the host appointed) change roles. The app has no command that appoints a co-host yet, so in practice only the host changes roles. The host broadcasts a signed room-state snapshot periodically and on every change. A promotion starts the microphone only if the local user raised a hand.
- **Control plane**: iroh-gossip on a per-room topic. Signatures cover the stage id and a timestamp that must increase per signer.
- **Audio plane**: speakers stream Opus to the host. The host accepts a speaker connection only from the NodeId of a participant with a speaking role, mixes the streams and fans the mix out to listeners. Speakers receive individual forwarded streams.
- **Relays**: the protocol lets a speaker or co-host relay the mixed stream to additional listeners, one relay per identity; the app has no control that offers it yet.
- **Stream authentication**: a SHA-256 hash chain over frames with a checkpoint every 10 frames (200 ms), signed over the stage id, a sequence number and the host time. A listener plays a frame only after a checkpoint that extends a verified chain covers it, so frames after a sequence gap are not played until the chain is verified again; this adds up to 200 ms of delay. A checkpoint more than 60 s from the listener's clock, with an earlier time than the previous one, or below a sequence already verified is rejected as a replay. A listener stops playback from a source that sends too many frames without a verifiable checkpoint, and does not return to a relay that failed verification or answered with the ban code.
- **Moderation**: hand raising, promote and demote, host mute, kick, ban, in-room chat. Demotion stops the speaker's capture. A kicked identity is refused for 10 minutes, a banned one for the session. A node discards control messages signed by its own identity, which only a replay by another node delivers.
- **Microphone**: a speaker whose microphone cannot be opened stays connected, hears the other speakers and sends one frame of silence per second, so the host keeps the connection; the room shows a warning. On Android the app requests the microphone permission before a call or before speaking.
- **Admission**: when the participant or sender table is full, a newcomer replaces the least recently seen entry that has no speaking role and no audio connection. The host keeps 16 of its 80 listener connections for identities with a speaking role (relays).
- **Announcements**: signed by the host and broadcast on the host's feed topic every 30 s. A host that is not Public has no feed topic: it pushes the announcement to its push recipients (approved followers; for Private, mutuals) every 90 s and pushes the end of the stage, each to up to 4 devices of every recipient (other pushes stop at the first device that acknowledges, as device sync copies their content). The app lists an announced stage for 120 s after the last announcement.

## Linked Devices

Several devices can share one identity.

- **Pairing**: the existing device shows a QR code (or its text form for a device without a camera) that holds its NodeId and a one-time secret valid for 60 seconds. The new device generates a new transport key, dials that NodeId, so QUIC authenticates the existing device, and sends the secret, the NodeId of the new key and its device name. The existing device compares the secret in constant time.
- **Link code**: both devices then show a 6-digit code computed from the existing NodeId and the new NodeId (SHA-256). The existing device sends the link bundle only after the user confirmed in a native dialog that the codes match. It does not store the new NodeId then: it stores it when the new device first device-syncs with it, so a new device that fails to install the identity leaves no device entry behind. The new NodeId is generated after the QR code was read and travels only inside the QUIC connection, so a device that also read the QR code cannot produce the same code.
- **Bundle**: the master key, the signed device list (with the removed NodeIds), profile, follows, moderation entries, bookmarks and the device lists of followed users. Linking replaces the identity, the transport key and all local data of the new device, after confirmation in a native dialog.
- **Keys**: every device holds the master key and its own random transport key.
- **Removal**: removing a device records its NodeId as removed; each device keeps the 256 newest removed NodeIds and does not store them as devices again, and the device list carries the 16 newest. Followers stop dialing it when they receive the newer list. Removals are accepted only from a device that is already stored as one of the user's devices. When a device sync shows a device a list that removes it, it asks in a native dialog whether to delete its data, key files included, and restart with a new identity; any holder of the master key can sign a removal, so nothing is deleted without that confirmation. A device that is not connected keeps the identity until then, and a device that someone else controls can decline: only a new identity protects against a stolen device.
- **Device sync**: each side sends its device list signed by the master key, and the other side continues only if the list is signed by its own master key, contains the NodeId of the connection, and that NodeId was not removed and is a stored device or one this device linked (recorded when it sent the link bundle). Every device holds the master key, so a removed device that generates a new NodeId is refused until a stored device's list names it. A device learned through device sync is stored with the time it was learned: the own device list holds the 16 devices stored longest, so a removed device that syncs with a device that has not merged its removal cannot push stored devices out of that list. Both sides store the devices of the other's list, and its removed NodeIds if the other side is a stored device. The responder sends its list before it checks the requester, so a removed requester receives the list that removes it. Devices exchange follows with the device lists of the followed users, moderation entries, bookmarks and follow requests (last writer wins; for a follow request, the later approval or denial), direct messages (by id, among the newest 50,000) and deletions of own interactions. After that, each side fetches the other's own posts, interactions and post deletions with the sync protocol; a device serves that sync to its other devices whatever its visibility.
- **Not replicated**: read state of direct messages.

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).

## License

MIT
