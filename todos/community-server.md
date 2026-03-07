# Community Server - Design Document

A self-hosted, headless server binary that provides aggregation, discovery, search, and trending for the Iroh Social P2P network. Users opt in by registering with a server -- the server never scrapes or indexes without consent. The server respects a three-tier visibility model: Public users get full indexing, Listed users get profile-only presence for discoverability, and Private users have no server-side footprint at all.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Visibility Model](#visibility-model)
- [Workspace Structure](#workspace-structure)
- [Registration Protocol](#registration-protocol)
- [Post Ingestion](#post-ingestion)
- [Server Storage](#server-storage)
- [HTTP API](#http-api)
- [Trending Algorithm](#trending-algorithm)
- [Client Integration](#client-integration)
- [Server Configuration](#server-configuration)
- [Federation (Future)](#federation-future)
- [Implementation Roadmap](#implementation-roadmap)

---

## Architecture Overview

```
                         +--------------------+
                         | Community Server   |
                         | (headless binary)  |
                         |                    |
  Users opt-in           | - Iroh node        |   HTTP API
  via signed    -------> | - Gossip listener  | <-------  Clients query for
  registration           | - Sync puller      |           search, trending,
                         | - sqlx (SQLite)    |           discovery
                         |                    |
                         | - axum HTTP server |
                         +--------------------+
                                  |
                          Participates in
                          the P2P network
                          as a first-class
                          Iroh node
```

The server runs its own Iroh endpoint and joins the same gossip topics and sync protocol that regular clients use. It stores an aggregated index of Public users' posts using sqlx with SQLite and FTS5 full-text search, and stores profile-only records for Listed users. An axum HTTP API exposes this index for search, trending, user discovery, and aggregated feeds. The server respects each user's visibility setting (see [Visibility Model](#visibility-model)).

Key principle: **the server is an overlay, not a replacement**. The P2P layer remains the foundation. Users who never connect to a server lose nothing. Servers add opt-in social features that require aggregation (search, trending, discovery).

---

## Visibility Model

User profiles have a `visibility` field (replacing the old boolean `is_private`) that controls how their content is distributed and what the community server stores. Three levels:

### Visibility Levels

```rust
// In iroh-social-types/src/types.rs (shared crate)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Public,
    Listed,
    Private,
}
```

### Summary

| | Public | Listed | Private |
|---|---|---|---|
| Server stores profile | yes | yes | no |
| Server indexes posts | yes | no | no |
| Post delivery | gossip broadcast | direct push to followers | direct push to mutuals |
| Sync access | anyone | followers only | mutuals only |
| Discoverable via server | yes | yes | no |
| Follow model | open | request-based | mutual only |

### Public (default)

Full participation in the network. Posts are broadcast via gossip to anyone subscribed to the user's topic. Sync is open to any peer. If registered with a community server, the server subscribes to the gossip topic, syncs history, indexes all posts, and includes them in search, feeds, and trending.

### Listed

The "locked account" model (similar to Twitter/X private accounts). The user's profile (display name, bio, avatar) is stored by the community server and appears in user search and the directory, so people can discover and send follow requests. However:

- **No post ingestion.** The server does not subscribe to the user's gossip topic and does not sync or store any posts or interactions. The user appears in search results but with no post content.
- **Follow requests.** New followers must send a follow request that the user explicitly approves before the follower receives any content. (This makes follow requests a natural dependency of the Listed visibility tier.)
- **Direct push delivery.** Posts are not broadcast via gossip. Instead, the client iterates over approved followers and pushes posts directly via QUIC connections (similar to the DM outbox pattern). If a follower is offline, the post is queued locally and retried on a timer.
- **Sync restricted to followers.** Only approved followers can sync history. The sync handler checks that the requesting peer is in the user's follower list before responding.

### Private

Maximum privacy. No server-side presence at all:

- **No registration allowed.** The server rejects registration attempts from Private users. If a user switches from Public/Listed to Private while registered, the client automatically unregisters from all servers.
- **No gossip broadcasting.** Posts are never published to the user's gossip topic.
- **Direct push to mutuals only.** Posts are pushed directly to mutual follows (users the Private user follows who also follow them back). This is more restrictive than Listed -- a Private user must actively choose to follow someone to establish a communication channel.
- **Sync restricted to mutuals.** Only mutuals can sync history. The sync handler checks both that the requesting peer is a follower and that the user follows them back.
- **Invisible to aggregation.** The user does not appear in any server's directory, search, feed, or trending. They only exist on the P2P layer for peers who already have their node ID and a mutual relationship.

### Direct Push Protocol

Listed and Private users bypass gossip and push posts directly to their audience. A dedicated ALPN keeps this separate from sync:

```
ALPN: b"iroh-social/push/1"
```

```
Author                           Follower/Mutual
  |                                    |
  |-- [PUSH_ALPN] QUIC connection ---> |
  |-- PushMessage (length-prefixed) -->|
  |<-- PushAck ------- ---------------|
  |                                    |
```

#### Wire types

```rust
// In iroh-social-types/src/protocol.rs (shared crate)

pub const PUSH_ALPN: &[u8] = b"iroh-social/push/1";

/// Pushed from author to follower/mutual.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushMessage {
    pub author: String,
    pub posts: Vec<Post>,
    pub interactions: Vec<Interaction>,
    pub profile: Option<Profile>,       // included when profile changes
}

/// Acknowledgment from recipient.
#[derive(Debug, Serialize, Deserialize)]
pub struct PushAck {
    pub received_post_ids: Vec<String>,
    pub received_interaction_ids: Vec<String>,
}
```

The recipient validates incoming posts/interactions using the same pipeline as gossip-received content (`validate_post()`, `verify_post_signature()`, etc.) and stores them locally. The `PushAck` tells the sender which items were accepted so it can clear them from the outbox.

#### Outbox behavior

- On post creation, the client enqueues a push to each recipient (followers for Listed, mutuals for Private).
- A background task processes the outbox, attempting delivery and retrying on failure with backoff.
- Pushes are coalesced: if multiple posts queue up for an offline peer, they are delivered in a single batch when the peer comes online.
- The outbox is persisted in SQLite so pending pushes survive app restarts.

#### Client-side outbox table

```sql
CREATE TABLE IF NOT EXISTS push_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient TEXT NOT NULL,
    post_id TEXT,                    -- null for interaction-only pushes
    interaction_id TEXT,             -- null for post-only pushes
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER
);

CREATE INDEX idx_push_outbox_recipient ON push_outbox(recipient);
```

#### Push handler on recipient side

The client registers a `ProtocolHandler` for `PUSH_ALPN` alongside the existing `SyncHandler`. When a connection arrives:

1. Read the length-prefixed `PushMessage`.
2. Validate `author` matches the connection's `remote_id()`.
3. Check that the sender is an expected source (I follow them, or they're a mutual -- depending on my own visibility).
4. Validate and store each post/interaction.
5. Send `PushAck` back.
6. Emit `feed-updated` event to the frontend.

### Follow Requests (Listed visibility)

Listed users require explicit approval before a peer becomes an approved follower. A dedicated ALPN handles this:

```
ALPN: b"iroh-social/follow-request/1"
```

#### Wire types

```rust
// In iroh-social-types/src/protocol.rs (shared crate)

pub const FOLLOW_REQUEST_ALPN: &[u8] = b"iroh-social/follow-request/1";

#[derive(Debug, Serialize, Deserialize)]
pub struct FollowRequest {
    pub requester: String,          // pubkey of the person requesting to follow
    pub timestamp: u64,
    pub signature: String,          // hex-encoded ed25519 signature over { requester, timestamp }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FollowResponse {
    Approved,
    Denied,
    Pending,                        // will decide later
}
```

#### Flow

1. **Requester** opens a QUIC connection on `FOLLOW_REQUEST_ALPN` to the Listed user.
2. Sends a signed `FollowRequest`.
3. **Listed user's client** validates the signature, stores the request in a `follow_requests` table, and emits a notification.
4. Responds immediately with `FollowResponse::Pending`.
5. When the Listed user approves/denies via UI, the response is stored. On approval, the requester is added to the followers table and included in future direct pushes. Optionally, the Listed user can proactively connect to the requester on the same ALPN to deliver the `Approved` response (or wait until the requester polls).

#### Client-side follow requests table

```sql
CREATE TABLE IF NOT EXISTS follow_requests (
    pubkey TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'   -- 'pending', 'approved', 'denied'
);
```

### Visibility Changes

When a user changes their visibility level:

- **Public -> Listed**: Server deletes all stored posts and interactions for the user. Server retains profile info only. Client stops gossip broadcasting and switches to direct push. Existing followers are grandfathered in (no re-approval needed). Future follows require requests.
- **Public -> Private**: Server unregistration is triggered automatically. All server-side data is deleted. Client stops gossip, switches to direct push to mutuals only.
- **Listed -> Public**: Client resumes gossip broadcasting. Server begins ingesting posts (re-syncs history on next sync cycle).
- **Listed -> Private**: Server unregistration is triggered. Client narrows delivery from all followers to mutuals only.
- **Private -> Listed**: Client can register with a server (profile only). Delivery expands from mutuals to all followers. Follow requests enabled for new followers.
- **Private -> Public**: Client can register fully. Gossip broadcasting resumes. Server ingests posts.

**Ordering for Public -> Listed/Private**: The client must broadcast the `ProfileUpdate` via gossip *before* stopping the gossip topic, so the server (and other peers) learn of the visibility change. Sequence: (1) broadcast ProfileUpdate, (2) wait briefly for propagation, (3) stop gossip topic, (4) unregister from server if switching to Private.

The visibility change is broadcast as a `ProfileUpdate` via whatever delivery mechanism is currently active (gossip for Public, direct push for Listed/Private), so peers update their local record of the user's visibility.

---

## Workspace Structure

The repo is already a Cargo workspace. The shared types crate (`crates/iroh-social-types/`) exists and is used by the Tauri app. The server crate is the new addition.

```
iroh-social/
  Cargo.toml                      # Workspace root (existing)
  crates/
    iroh-social-types/            # Shared types and protocol definitions (existing)
      Cargo.toml
      src/
        lib.rs
        types.rs                  # Post, Profile, Visibility, MediaAttachment, Interaction, FollowEntry, FollowerEntry
        protocol.rs               # GossipMessage, SyncRequest/SyncSummary/SyncFrame, SYNC_ALPN, PUSH_ALPN, FOLLOW_REQUEST_ALPN, user_feed_topic(), PushMessage/PushAck, FollowRequest/FollowResponse
        signing.rs                # sign_post(), verify_post_signature(), sign_interaction(), verify_interaction_signature()
        validation.rs             # validate_post(), validate_interaction(), validate_profile(), constants
        dm.rs                     # DmHandshake, EncryptedEnvelope, DirectMessage, DM_ALPN, etc.
        registration.rs           # RegistrationPayload, RegistrationRequest, sign_registration(), verify_registration_signature()
    iroh-social-server/           # Server binary (new)
      Cargo.toml
      migrations/                 # sqlx migrations (SQLite)
      src/
        main.rs                   # CLI entry, config loading, startup
        config.rs                 # TOML config parsing
        node.rs                   # Iroh endpoint, gossip, sync setup
        storage.rs                # sqlx storage (SQLite)
        ingestion.rs              # Gossip subscriber + sync scheduler
        trending.rs               # Trending computation
        api/
          mod.rs                  # axum Router assembly
          server_info.rs          # GET /api/v1/info
          auth.rs                 # POST/DELETE /api/v1/register, PUT /api/v1/register (profile update)
          users.rs                # GET /api/v1/users, search, profile
          posts.rs                # GET /api/v1/posts/search
          feed.rs                 # GET /api/v1/feed
          trending.rs             # GET /api/v1/trending
  src-tauri/                      # Existing Tauri app (uses shared crate)
  src/                            # Existing Svelte frontend
```

### Shared crate status

The types crate contains: `types.rs` (Post, Profile, MediaAttachment, Interaction, FollowEntry, FollowerEntry), `protocol.rs` (GossipMessage with NewPost/DeletePost/ProfileUpdate/NewInteraction/DeleteInteraction, SyncRequest/SyncSummary/SyncFrame with Posts/Interactions variants, SYNC_ALPN `b"iroh-social/sync/3"`, user_feed_topic()), `signing.rs` (sign_post, verify_post_signature, sign_interaction, verify_interaction_signature -- `signature_to_hex` and `hex_to_signature` are currently private, need to be made `pub` for registration.rs), `validation.rs` (validate_post, validate_interaction, validate_profile, parse_mentions), and `dm.rs`.

New modules to add: `registration.rs` (registration signing/verification) and new types/ALPNs in `protocol.rs` (PushMessage/PushAck/PUSH_ALPN, FollowRequest/FollowResponse/FOLLOW_REQUEST_ALPN).

---

## Registration Protocol

### Design

Single-step signed registration over HTTP. The user signs a payload with their identity key (ed25519) to prove identity. No challenge-response needed -- the payload includes server URL and timestamp to prevent replay.

**Visibility gate**: Only Public and Listed users can register. Private users are rejected (403). If a registered user later changes to Private visibility, the client automatically sends an unregistration request and the server purges all their data.

### Registration flow

1. User constructs a `RegistrationPayload`:
   ```
   { pubkey, server_url, timestamp, visibility: "public", action: None }
   ```
2. Serializes it using canonical JSON (`serde_json::to_vec(&serde_json::json!({...}))`) -- the same pattern used for signing posts and interactions. Note: `serde_json::json!` uses `BTreeMap` internally, so keys are always alphabetically sorted. This is deterministic.
3. Signs the bytes with their ed25519 identity key using `sign_registration()` from the shared crate.
4. POSTs a `RegistrationRequest` to the server:
   ```
   { pubkey, server_url, timestamp, visibility: "public", action: null, signature }
   ```
5. Server verifies using `verify_registration_signature()` from the shared crate:
   - Timestamp within 5 minutes of server time
   - `server_url` matches the server's own URL
   - Signature is valid for the pubkey over the reconstructed payload bytes
   - `visibility` is not `"private"` (reject with 403)
6. Server stores a registration record. Behavior depends on visibility:
   - **Public**: Server subscribes to the user's gossip topic and begins ingesting posts.
   - **Listed**: Server stores profile info only. No gossip subscription, no post ingestion.

### Unregistration

Same mechanism with `action: Some("unregister")`: user constructs a `RegistrationPayload` with `action: Some("unregister".to_string())`, signs it with `sign_registration()`, and sends to `DELETE /api/v1/register`. Server verifies with `verify_registration_signature()`, stops ingesting posts (if Public), deletes all stored posts/interactions (if any), and marks user inactive.

**Auto-unregistration on visibility change to Private**: When a user changes their visibility to Private, the client iterates over all stored servers and sends an unregistration request to each. The server purges all data for that user (profile, posts, interactions).

### Listed profile updates

Listed users don't broadcast via gossip, so the server can't passively receive their profile changes. Two mechanisms:

1. **Re-registration**: The Listed user sends a new `POST /api/v1/register` with updated profile info. The server updates the existing registration record (upsert).
2. **Profile update endpoint**: `PUT /api/v1/register` accepts a signed payload containing the updated profile fields. The server verifies the signature and updates the stored profile. This avoids the overhead of full re-registration.

```
PUT /api/v1/register
Body: { pubkey, server_url, timestamp, visibility, display_name, bio, avatar_hash, signature }
Response (200): { message }
```

### Visibility updates

When the server receives a `ProfileUpdate` (via gossip for Public users), it checks the `visibility` field:

- **Public -> Listed**: Server unsubscribes from the user's gossip topic, deletes all stored posts and interactions, retains profile only.
- **Public -> Private** or **Listed -> Private**: Server processes as an unregistration -- deletes all data and marks inactive.
- **Listed -> Public**: Server subscribes to the user's gossip topic and begins ingesting posts. Triggers an immediate sync to backfill history.

Note: For Listed users, the server only learns of visibility changes via the HTTP API (re-registration or profile update endpoint), not via gossip.

### Data types

`RegistrationPayload` and `RegistrationRequest` go in the shared crate (`iroh-social-types/src/registration.rs`) since the client needs them to sign and send registration requests. `Registration` is server-only.

```rust
// In iroh-social-types/src/registration.rs (shared)
// Requires signature_to_hex() and hex_to_signature() to be made pub in signing.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationPayload {
    pub pubkey: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: String,  // "public" or "listed"
    /// None for registration, Some("unregister") for unregistration.
    pub action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationRequest {
    pub pubkey: String,
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: String,
    pub action: Option<String>,
    pub signature: String,  // hex-encoded ed25519 signature
}

// Canonical signing bytes -- keys sorted alphabetically by serde_json::json! (BTreeMap)
fn registration_signing_bytes(pubkey: &str, server_url: &str, timestamp: u64, visibility: &str, action: &Option<String>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": action,
        "pubkey": pubkey,
        "server_url": server_url,
        "timestamp": timestamp,
        "visibility": visibility,
    }))
    .expect("json serialization should not fail")
}

pub fn sign_registration(payload: &RegistrationPayload, secret_key: &SecretKey) -> String {
    let bytes = registration_signing_bytes(&payload.pubkey, &payload.server_url, payload.timestamp, &payload.visibility, &payload.action);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

pub fn verify_registration_signature(request: &RegistrationRequest) -> Result<(), String> {
    let sig = hex_to_signature(&request.signature)?;
    let pubkey: PublicKey = request.pubkey
        .parse()
        .map_err(|e| format!("invalid pubkey: {e}"))?;
    let bytes = registration_signing_bytes(&request.pubkey, &request.server_url, request.timestamp, &request.visibility, &request.action);
    pubkey
        .verify(&bytes, &sig)
        .map_err(|_| "registration signature verification failed".to_string())
}

// Server-only
struct Registration {
    pubkey: String,
    registered_at: u64,
    last_seen: u64,
    display_name: Option<String>,
    bio: Option<String>,
    avatar_hash: Option<String>,
    visibility: String,  // "public" or "listed"
    is_active: bool,
}
```

---

## Post Ingestion

### Visibility-aware ingestion

Post ingestion only applies to **Public** users. Listed users have profile-only registrations -- the server never subscribes to their gossip topic, never syncs their posts, and never stores their content. Private users cannot register at all.

### Dual-mode: gossip + sync (Public users only)

Both mechanisms are needed for completeness:

**Gossip (real-time):** When a Public user registers, the server subscribes to their gossip topic (`user_feed_topic(pubkey)`). This is the same subscription pattern used by the Tauri client in `gossip.rs`. The server receives `NewPost`, `DeletePost`, `ProfileUpdate`, `NewInteraction`, and `DeleteInteraction` messages in real time.

**Important:** Listed users do not broadcast via gossip (they use direct push to followers), so there is no gossip topic to subscribe to. The server only receives profile updates from Listed users via the HTTP API.

**Sync (historical catch-up):** Uses the same `SYNC_ALPN` (`b"iroh-social/sync/3"`) protocol and shared types (`SyncRequest`, `SyncSummary`, `SyncFrame`) from the types crate. The server implements its own sync client (it cannot reuse the Tauri-specific code directly, but the protocol is identical). The `SyncSummary` includes the user's `Profile`, which the server uses to update the registrations table. Triggered on:

- Server startup (sync all Public registered users)
- New Public user registration (sync their history immediately)
- Periodic catch-up every 15 minutes for Public users whose last gossip was >30 min ago

**Note:** Sync requests to Listed users will be rejected by their sync handler (which restricts access to approved followers). The server does not attempt post sync for Listed users.

### Architecture

```
IngestionManager
  |
  +-- GossipSubscriber (per Public registered user)
  |     Subscribes to user_feed_topic(pubkey)
  |     Processes GossipMessage variants:
  |       NewPost, DeletePost, ProfileUpdate,
  |       NewInteraction, DeleteInteraction
  |     Writes to sqlx database
  |     NOT created for Listed users
  |
  +-- SyncScheduler
        On startup: sync all Public registered users
        Every 15 min: catch-up sync for stale Public users
        On registration: immediate history pull (Public only)
        Bounded concurrency via semaphore (max 10)
```

### Validation

Same checks as the Tauri client:

- `validate_post()` (content length, media count, timestamp drift)
- `validate_interaction()` (timestamp drift)
- `validate_profile()` (display name length, bio length)
- Deduplication via `(author, id)` unique constraint
- Signature verification via `verify_post_signature()` / `verify_interaction_signature()`

**Per-variant gossip topic owner validation.** Each message received on `user_feed_topic(pubkey)` must be validated against the topic owner:

| Variant | Validation |
|---------|-----------|
| `NewPost(post)` | `post.author` must equal topic owner |
| `DeletePost { id, author }` | `author` must equal topic owner |
| `ProfileUpdate(profile)` | Accept only from topic owner (no author field on Profile -- use topic owner for attribution) |
| `NewInteraction(interaction)` | `interaction.author` must equal topic owner |
| `DeleteInteraction { id, author }` | `author` must equal topic owner |

For `DeletePost` and `DeleteInteraction`, the server verifies the stored post/interaction belongs to the claimed author before deleting, same as the Tauri client does in `gossip.rs`.

---

## Server Storage

### sqlx with SQLite

The server uses **sqlx** with SQLite for storage:

- Async-native database access fits naturally with axum
- Single file, no external service, zero config for self-hosting
- FTS5 for full-text search
- Connection pooling for concurrent HTTP request handling

### Why not rusqlite?

The Tauri client uses rusqlite because it's an embedded single-user desktop app where async provides no benefit. The server is different: axum is async, so sqlx queries compose naturally without `spawn_blocking`, and connection pooling matters for concurrent HTTP handling.

### Dependencies

```toml
[dependencies]
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "migrate"] }
```

### Schema

```sql
CREATE TABLE registrations (
    pubkey TEXT PRIMARY KEY,
    registered_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    display_name TEXT,
    bio TEXT,
    avatar_hash TEXT,
    visibility TEXT NOT NULL DEFAULT 'public',  -- 'public' or 'listed'
    is_active BOOLEAN NOT NULL DEFAULT 1
);

CREATE TABLE posts (
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    media_json TEXT,
    reply_to TEXT,
    reply_to_author TEXT,
    quote_of TEXT,
    quote_of_author TEXT,
    signature TEXT NOT NULL DEFAULT '',
    indexed_at INTEGER NOT NULL,
    PRIMARY KEY (author, id),
    FOREIGN KEY (author) REFERENCES registrations(pubkey)
);

CREATE INDEX idx_posts_timestamp ON posts(timestamp DESC);
CREATE INDEX idx_posts_author_timestamp ON posts(author, timestamp DESC);

-- NOTE: The FK constraint means only interactions from registered users are stored.
-- The server subscribes to each registered user's gossip topic, which carries that
-- user's own interactions (e.g., Alice's likes). Interactions from unregistered users
-- (e.g., an unregistered Bob liking Alice's post) arrive on Bob's topic, which the
-- server does not subscribe to. Consequently, like_count and other aggregate endpoints
-- only reflect registered users' activity. This is intentional: the server is an
-- overlay that indexes opted-in users, not a global aggregator.
CREATE TABLE interactions (
    id TEXT NOT NULL,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,          -- 'Like', etc.
    target_post_id TEXT NOT NULL,
    target_author TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    signature TEXT NOT NULL DEFAULT '',
    indexed_at INTEGER NOT NULL,
    PRIMARY KEY (author, id),
    FOREIGN KEY (author) REFERENCES registrations(pubkey)
);

CREATE INDEX idx_interactions_target ON interactions(target_author, target_post_id);
CREATE INDEX idx_interactions_author ON interactions(author, timestamp DESC);

-- Full-text search
CREATE VIRTUAL TABLE posts_fts USING fts5(
    content,
    content=posts,
    content_rowid=rowid,
    tokenize='unicode61'
);

-- Keep FTS in sync automatically
CREATE TRIGGER posts_ai AFTER INSERT ON posts BEGIN
    INSERT INTO posts_fts(rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TRIGGER posts_ad AFTER DELETE ON posts BEGIN
    INSERT INTO posts_fts(posts_fts, rowid, content) VALUES('delete', old.rowid, old.content);
END;

CREATE TABLE trending_hashtags (
    tag TEXT PRIMARY KEY,
    post_count INTEGER NOT NULL,
    unique_authors INTEGER NOT NULL,
    latest_post_at INTEGER NOT NULL,
    score REAL NOT NULL,
    computed_at INTEGER NOT NULL
);

CREATE TABLE sync_state (
    pubkey TEXT PRIMARY KEY,
    last_synced_at INTEGER NOT NULL,
    last_post_timestamp INTEGER,
    last_interaction_timestamp INTEGER,
    FOREIGN KEY (pubkey) REFERENCES registrations(pubkey)
);
```

---

## HTTP API

All endpoints under `/api/v1/`. Server also returns basic HTML at `GET /`.

### Endpoints

#### Server Info

```
GET /api/v1/info

Response: {
    name, description, version, node_id,
    registered_users, total_posts,
    uptime_seconds, registration_open
}
```

#### Registration

```
POST /api/v1/register
Body: { pubkey, server_url, timestamp, visibility, action, signature }
Response (201): { pubkey, registered_at, message }
Errors: 400 (bad sig/timestamp), 403 (closed or visibility=private), 409 (exists)

PUT /api/v1/register
Body: { pubkey, server_url, timestamp, visibility, display_name, bio, avatar_hash, signature }
For Listed users to update their profile on the server.
Response (200): { message }

DELETE /api/v1/register
Body: { pubkey, server_url, timestamp, visibility, action: "unregister", signature }
Response (200): { message }
```

#### User Directory

```
GET /api/v1/users?limit=20&offset=0
Response: { users: [...], total, limit, offset }
Users include both Public and Listed registrations.
Each user object includes a "visibility" field.

GET /api/v1/users/search?q=alice&limit=20
Response: { users: [...], total, query }
Searches both Public and Listed users by display name.

GET /api/v1/users/:pubkey
Response: { pubkey, display_name, bio, avatar_hash, visibility, registered_at, post_count, latest_post_at }
For Listed users, post_count is always 0 and latest_post_at is null.
```

#### Posts

```
GET /api/v1/users/:pubkey/posts?limit=50&before=<timestamp>
Response: { posts: [...] }
Returns empty array for Listed users (no posts stored).

GET /api/v1/posts/search?q=rust+iroh&limit=20&offset=0
Response: { posts: [...], total, query }
Only searches posts from Public users.
```

#### Interactions

```
GET /api/v1/users/:pubkey/interactions?limit=50&before=<timestamp>
Response: { interactions: [...] }
Returns empty array for Listed users (no interactions stored).

GET /api/v1/posts/:author/:post_id/interactions
Response: { interactions: [...], like_count: number }
Only returns interactions from Public registered users.
```

#### Feed (Global)

```
GET /api/v1/feed?limit=50&before=<timestamp>
Response: { posts: [...] }
Only includes posts from Public users.

GET /api/v1/feed?limit=50&before=<timestamp>&authors=<pk1>,<pk2>
Optional author filter for custom feeds. Filters to Public users only.
```

#### Trending

```
GET /api/v1/trending?limit=10
Response: { hashtags: [...], computed_at }

GET /api/v1/trending/posts?limit=20
Response: { posts: [...] }
Only includes posts from Public users.
```

### Middleware

- **Rate limiting** via `tower-governor`: registration 5/hr/IP, search 60/min/IP, reads 120/min/IP
- **CORS** enabled for all GET endpoints
- **Request logging** via `tower-http::trace`

---

## Trending Algorithm

### Hashtag extraction

Regex: `#[a-zA-Z0-9_]+`, normalized to lowercase.

### Scoring formula (per hashtag, over 24-hour window)

```
score = (post_count * author_weight * recency_factor) / age_decay

post_count    = posts containing the hashtag in the window
author_weight = sqrt(unique_authors)
recency_factor = sum(1.0 / (1.0 + hours_since_post)) for each post
age_decay     = 1.0 + (hours_since_oldest_post / 24.0)
```

- `sqrt(unique_authors)` prevents one user spamming a tag from dominating
- `recency_factor` weights newer posts higher
- `age_decay` reduces stale bursts

### Trending post score

```
post_score = (1 + min(hashtag_boost, 3)) * (1.0 / (1.0 + hours_since_post)^1.5)
```

Where `hashtag_boost` is the number of currently-trending hashtags in the post.

### Computation

Background task recomputes every 5 minutes. Results stored in `trending_hashtags` table. API reads always serve precomputed data. Only counts posts from Public users.

---

## Client Integration

### New Tauri commands

```
add_server(url)              -- Fetch /api/v1/info, store connection
remove_server(url)           -- Remove stored connection
list_servers()               -- List all stored servers with status
register_with_server(url)    -- Sign + POST /api/v1/register
unregister_from_server(url)  -- Sign + DELETE /api/v1/register
update_server_profile(url)   -- Sign + PUT /api/v1/register (Listed users)
server_search_posts(url, q)  -- Query /api/v1/posts/search
server_search_users(url, q)  -- Query /api/v1/users/search
server_get_feed(url, ...)    -- Query /api/v1/feed
server_get_trending(url)     -- Query /api/v1/trending
server_discover_users(url)   -- Query /api/v1/users
```

### New dependency

Add `reqwest` to `src-tauri/Cargo.toml` for HTTP client.

### New storage

Add tables to the client's SQLite database:

```sql
CREATE TABLE IF NOT EXISTS servers (
    url TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    is_registered INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS push_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient TEXT NOT NULL,
    post_id TEXT,
    interaction_id TEXT,
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_attempt_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_push_outbox_recipient ON push_outbox(recipient);

CREATE TABLE IF NOT EXISTS follow_requests (
    pubkey TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
);
```

### New frontend pages

**`/servers` page:**

- List connected servers with status (online/offline)
- Add server by URL
- Register/unregister with each server
- Hide "Register" button for Private visibility users

**`/discover` page (or integrated into servers):**

- Browse user directory from a selected server
- Search users by name
- "Follow" button for discovered users (sends follow request for Listed users)

**`/follow-requests` page (Listed users only):**

- List pending follow requests
- Approve/deny buttons for each request
- Show requester's pubkey (and profile if available)

**Search integration in feed:**

- When servers are configured, show search bar
- Results from server's FTS endpoint

**Trending section:**

- Display trending hashtags from connected server
- Click hashtag to search

### Visibility-aware client behavior

The client adapts its post delivery mechanism based on the user's visibility setting:

- **Public**: Broadcasts posts via gossip (current behavior). Registers with servers normally.
- **Listed**: Does not start a gossip topic. On post creation, iterates over approved followers and pushes posts directly via QUIC (PUSH_ALPN). Registers with servers as "listed" (profile only). Enables the follow requests UI. Uses `PUT /api/v1/register` to push profile changes to servers.
- **Private**: Does not start a gossip topic. On post creation, pushes directly to mutuals only via PUSH_ALPN. Cannot register with servers. Hides the "Register" button in the servers UI.

When the user changes visibility in settings, the client:

1. Updates the local profile with the new `visibility` value.
2. Broadcasts the `ProfileUpdate` via the current delivery mechanism (so peers learn of the change).
3. If changing to Private: auto-unregisters from all servers.
4. If changing from Listed/Private to Public: re-starts the gossip topic.
5. If changing from Public to Listed/Private: broadcasts ProfileUpdate via gossip first, then stops gossip broadcasting.

### New storage helper: is_mutual

The Private visibility tier needs to check mutual follows. Add to `storage/social.rs`:

```rust
pub fn is_mutual(&self, pubkey: &str) -> anyhow::Result<bool> {
    self.with_db(|db| {
        let is_follower: bool = db.query_row(
            "SELECT COUNT(*) > 0 FROM followers WHERE pubkey=?1",
            params![pubkey],
            |row| row.get(0),
        )?;
        let is_following: bool = db.query_row(
            "SELECT COUNT(*) > 0 FROM follows WHERE pubkey=?1",
            params![pubkey],
            |row| row.get(0),
        )?;
        Ok(is_follower && is_following)
    })
}
```

### New TypeScript types

```typescript
type Visibility = "public" | "listed" | "private";

interface ServerInfo {
  name: string;
  description: string;
  version: string;
  node_id: string;
  registered_users: number;
  total_posts: number;
  registration_open: boolean;
}
interface StoredServer {
  url: string;
  name: string;
  is_registered: boolean;
  added_at: number;
  status: "online" | "offline" | "unknown";
}
interface ServerUser {
  pubkey: string;
  display_name: string | null;
  bio: string | null;
  visibility: Visibility;
  post_count: number;
}
interface TrendingHashtag {
  tag: string;
  post_count: number;
  unique_authors: number;
  score: number;
}
interface FollowRequestEntry {
  pubkey: string;
  timestamp: number;
  status: "pending" | "approved" | "denied";
}
```

---

## Server Configuration

TOML config file:

```toml
[server]
name = "My Iroh Social Server"
description = "A community aggregation server"
listen_addr = "0.0.0.0:3000"
data_dir = "/var/lib/iroh-social-server"
public_url = "https://social.example.com"
registration_open = true

[limits]
max_registered_users = 1000
max_posts_per_user = 10000
rate_limit_requests_per_minute = 120

[sync]
interval_minutes = 15
startup_sync = true
max_concurrent_syncs = 10

[trending]
recompute_interval_minutes = 5
window_hours = 24
```

CLI (via `clap`):

```
iroh-social-server [OPTIONS]
  -c, --config <PATH>    Config file path (default: ./config.toml)
  --data-dir <PATH>      Override data directory
  --port <PORT>          Override listen port
```

---

## Design Notes

- `RegistrationPayload.visibility` uses the `Visibility` enum with serde string serialization, not a raw String, for type safety.
- Push protocol (`PUSH_ALPN`) needs per-peer rate limiting on the handler side to prevent spam.
- `PushMessage` has max batch sizes: 50 posts, 200 interactions per message.
- Push outbox retries are capped at 100 attempts or 7 days TTL. Old entries are pruned.
- Follow requests auto-expire after 30 days if not approved/denied.
- FTS5 triggers only handle INSERT/DELETE (not UPDATE), which is fine since posts are immutable.
- The server only stores interactions from registered users. Like counts are partial by design.
- `DELETE /api/v1/register` uses POST with `action: "unregister"` internally (some HTTP clients strip DELETE bodies).
- Listed users update server profiles via both `PUT /api/v1/register` (HTTP to server) and direct push `PushMessage` with `profile` field (to followers) -- both happen on profile change.
- Media: the HTTP API returns blob hashes and tickets. Clients fetch media via iroh-blobs directly from peers. The server does not proxy media.
- Sync ALPN stays at `sync/3` for visibility-aware restrictions (no backward compat per project rules).

---

## Federation (Future)

Planned but not in initial scope. Servers would peer over iroh QUIC with a custom ALPN:

```
ALPN: b"iroh-social/federation/1"
```

What gets shared between servers:

- Registered user lists (pubkeys + profiles + visibility level)
- Post metadata (other servers fetch full posts from users via P2P)
- Trending data

What does NOT get shared:

- Posts from Listed users (profile only, respecting their visibility)
- Media blobs (fetch from users directly)
- User credentials

Federation uses iroh's QUIC transport (not HTTP) for NAT traversal and consistent P2P architecture.

---

## Implementation Roadmap

### Phase 1: Workspace Refactor (DONE)

- [x] Create workspace root `Cargo.toml`
- [x] Create `crates/iroh-social-types/` with types extracted from `src-tauri/`
- [x] Update `src-tauri/Cargo.toml` to use workspace deps and depend on shared crate
- [x] Verify Tauri app builds and runs unchanged

### Phase 2a: Visibility Enum Migration

Purely a rename/expand of the existing `is_private: bool` to a three-tier `Visibility` enum.
No new features, just the foundational type change.

- [ ] Add `Visibility` enum (Public/Listed/Private) to `types.rs` in shared crate
- [ ] Replace `is_private: bool` with `visibility: Visibility` on `Profile` struct
- [ ] Make `signature_to_hex()` and `hex_to_signature()` pub in `signing.rs`
- [ ] Add `registration.rs` module to shared crate (RegistrationPayload, RegistrationRequest, sign/verify -- uses Visibility enum, not String)
- [ ] Add SQLite migration: rename `is_private` column to `visibility` (TEXT), convert 0->"public", 1->"private"
- [ ] Update `storage/profiles.rs`: read/write `visibility` as TEXT
- [ ] Update `storage/social.rs`: replace `is_private_profile()` with `get_visibility()` returning `Visibility`
- [ ] Update `sync.rs`: use `Visibility` enum for access control (Listed: followers only, Private: mutuals only)
- [ ] Add `is_mutual()` helper to `storage/social.rs`
- [ ] Update `commands/profile.rs`: accept `visibility: String` instead of `is_private: bool`
- [ ] Update frontend `types.ts`: replace `is_private: boolean` with `visibility: "public" | "listed" | "private"`
- [ ] Update `ProfileEditor.svelte`: replace toggle with visibility selector (radio group or dropdown)
- [ ] Update `profile/[pubkey]/+page.svelte`: replace "Private profile" badge with visibility badge
- [ ] Update `welcome/+page.svelte`: default to `visibility: "public"` instead of `isPrivate: false`

### Phase 2b: Push Protocol

New direct delivery mechanism for Listed/Private users who bypass gossip.

- [ ] Add `PushMessage`, `PushAck`, `PUSH_ALPN` to `protocol.rs`
- [ ] Add `push_outbox` table via migration (with `max_attempts` column, default 100)
- [ ] Implement push handler (`ProtocolHandler` for `PUSH_ALPN`) on client with per-peer rate limiting
- [ ] Implement push outbox background task with retry, backoff, max attempts (100), and TTL (7 days)
- [ ] Add max batch size constant for PushMessage (e.g., 50 posts, 200 interactions)
- [ ] Update `FeedManager`: Public broadcasts via gossip, Listed/Private enqueue to push outbox instead

### Phase 2c: Follow Requests

New social feature for Listed users requiring follow approval.

- [ ] Add `FollowRequest`, `FollowResponse`, `FOLLOW_REQUEST_ALPN` to `protocol.rs`
- [ ] Add `follow_requests` table via migration (with `expires_at` column, default 30 days)
- [ ] Implement follow request handler (`ProtocolHandler` for `FOLLOW_REQUEST_ALPN`) on client
- [ ] Build `/follow-requests` page for Listed users
- [ ] Auto-expire pending follow requests after 30 days

### Phase 2d: Visibility-Aware Delivery

Wire up the full visibility model across gossip, sync, and transitions.

- [ ] Update sync handler: Listed restricts to followers, Private restricts to mutuals
- [ ] Update client settings UI: handle visibility change transitions
- [ ] Handle visibility change ordering (broadcast ProfileUpdate via gossip before stopping topic)
- [ ] Auto-unregister from servers when switching to Private
- [ ] Proactively notify peers of visibility changes via available delivery mechanism

### Phase 3: Server Core

- [ ] Create `crates/iroh-social-server/` skeleton with `main.rs` and config
- [ ] Implement sqlx storage layer with migrations (SQLite + FTS5), including `visibility` column on registrations
- [ ] Implement Iroh node setup (endpoint, gossip -- no Tauri)
- [ ] Implement registration verification (ed25519 signature check) with visibility gate (reject Private, handle Listed vs Public)
- [ ] Implement `PUT /api/v1/register` for Listed profile updates
- [ ] Handle visibility changes via `ProfileUpdate` gossip: transition between Public/Listed ingestion modes, auto-purge on Private

### Phase 4: Server API + Ingestion

- [ ] Set up axum with middleware (CORS, rate limiting via tower-governor, logging via tower-http)
- [ ] Implement endpoints: `/info`, `/register` (POST/PUT/DELETE), `/users`, `/feed`, `/posts/search`, `/trending`
- [ ] Implement interaction endpoints: `/users/:pubkey/interactions`, `/posts/:author/:id/interactions`
- [ ] Ensure all API responses are visibility-aware (Listed users: profile only, no posts/interactions; search/feed/trending: Public only)
- [ ] Implement ingestion manager (gossip subscriber + sync scheduler) for Public users only
- [ ] Handle all current GossipMessage variants: `NewPost`, `DeletePost { id, author }`, `ProfileUpdate`, `NewInteraction`, `DeleteInteraction { id, author }`
- [ ] Validate each gossip variant against the topic owner (per-variant author matching)
- [ ] Implement trending computation background task (Public user posts only)

### Phase 5: Client Integration

- [ ] Add `reqwest` to Tauri app
- [ ] Add `servers` table to client SQLite storage via migration
- [ ] Implement Tauri commands for server interaction (visibility-aware: hide register for Private, profile-only for Listed)
- [ ] Build `/servers` page in Svelte
- [ ] Build `/follow-requests` page for Listed users
- [ ] Integrate search and discover into UI (Listed users appear in search with no posts)

### Phase 6: Polish

- [ ] Error handling and logging
- [ ] Server health check / metrics endpoint
- [ ] Stub federation module with reserved ALPN
- [ ] Documentation and deployment guide
