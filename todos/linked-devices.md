# Linked Devices - Design Document

Link multiple devices (phone, desktop, tablet) to a single identity. A three-tier key hierarchy separates concerns: a **master key** (permanent identity, cold storage), a **signing key** (derived from master, shared across devices for signing and DM), and per-device **transport keys** (iroh networking). The master key enables secure signing key rotation if a device is compromised, without losing the identity.

## Table of Contents

- [Design Principles](#design-principles)
- [Prior Art](#prior-art)
- [Key Architecture](#key-architecture)
- [Identity Model](#identity-model)
- [Device Registry](#device-registry)
- [Identity Resolution](#identity-resolution)
- [Pairing Protocol](#pairing-protocol)
- [Signing and Verification](#signing-and-verification)
- [DM Multi-Device](#dm-multi-device)
- [Device Sync](#device-sync)
- [Device Revocation](#device-revocation)
- [Storage](#storage)
- [Client Integration](#client-integration)
- [Discovery Server Considerations](#discovery-server-considerations)
- [Provenance Log (Optional)](#provenance-log-optional)
- [Implementation Roadmap](#implementation-roadmap)

---

## Design Principles

1. **Three-tier key hierarchy** -- A master key (Ed25519) is the permanent identity, kept in cold storage after setup. A signing key (derived from master via hardened derivation) handles day-to-day signing and DM encryption, shared across all linked devices. The transport key is also derived from the master key via HKDF (separate derivation path) and passed to iroh for QUIC networking. In the multi-device future, each device will derive its own transport key using a device-specific index.
2. **Simple verification** -- Peers verify content signatures against the signing key's public key. The master key is only involved during key delegation and rotation -- not on every post. Peers cache the master-signed delegation and verify content against the current signing key.
3. **Survivable compromise** -- If a device holding the signing key is compromised, the master key (on a secure device) derives a new signing key and publishes a signed rotation. The master public key is the permanent identity; the signing key is rotatable. This is the key advantage over a flat shared-key model.
4. **Shared DM sessions** -- All devices share the same X25519 static key (derived from the signing key). A peer needs only one DM ratchet session per user identity, regardless of how many devices that user has. Ratchet state must be synchronized between devices.
5. **Device registry for routing** -- A signed device announcement lists all linked devices' transport keys so peers and servers can reach any device for sync and DM delivery.
6. **No server required** -- Pairing, discovery, and sync happen directly between devices over QUIC (iroh transport). No relay or cloud service stores keys.

---

## Prior Art

This design draws from production multi-device systems, with analysis of why each approach was adopted or rejected.

### Signal (Sesame Algorithm)

Signal shares the identity key across all devices. Each device gets its own prekeys and independent Double Ratchet sessions. The Sesame algorithm manages per-device-pair sessions. Message fanout is client-side: the sender encrypts separately for each of the recipient's devices.

**What we adopt**: Signal's model of sharing the identity key across devices. All devices are the same identity cryptographically.

**What we differ on**: Signal uses per-device DM sessions (each device has its own X25519 key). We share the X25519 key (derived from the shared signing key) so peers need only one ratchet session per user. This trades DM session simplicity for ratchet state synchronization complexity.

### Matrix (Cross-Signing)

Matrix uses a three-level key hierarchy: master key (root of trust) signs a self-signing key, which signs individual device keys. Each device has its own Curve25519 + Ed25519 keypair. Megolm sessions handle group encryption with per-device key distribution via Olm.

**What we reject**: The entire model. Three-tier hierarchies, cross-signing ceremonies, and per-device key verification are too complex for our use case. Every peer must understand certificate chains. We want peers to verify one key, period.

### Keybase (Sigchain + Per-User Key)

Keybase gave each device its own NaCl keypair (never shared). An append-only sigchain recorded device additions and revocations. A Per-User Key (PUK) provided account-level encryption, rotated on device revocation.

**What we adopt**: The concept of a shared-purpose key (PUK) for user-level operations. Our "signing key" is analogous to Keybase's PUK but used for signing as well as encryption.

**What we differ on**: We do not use per-device signing keys. Keybase's sigchain was valuable for auditability in a centralized context; in P2P gossip, a lightweight device announcement suffices.

### Secure Scuttlebutt (Fusion Identity)

In SSB, each device is a separate feed with its own Ed25519 key. The Fusion Identity spec attempted to link feeds by having members exchange a shared fusion identity key. SSB's append-only log model makes key sharing dangerous: two devices using the same key can create conflicting sequence numbers, corrupting the feed.

**What we reject**: SSB's per-device-feed model. It forces peers to understand composite identities and resolve multiple feeds per user. Our model (one shared signing key, one identity) avoids this entirely.

**Lesson learned**: Shared keys require coordination to avoid state conflicts. Our ratchet synchronization between devices must handle this carefully.

### Nostr (NIP-46 Remote Signing / NIP-26 Delegation)

NIP-46 keeps the private key on a dedicated "signer" device. Client apps request signatures remotely over Nostr relays. NIP-26 defined delegated event signing where a master key signs a token authorizing a sub-key; it was deprecated due to no revocation mechanism.

**What we reject**: Remote signing (NIP-46) requires the signer online for every action. Delegation tokens (NIP-26) lack revocation. Both are poor fits for offline-first P2P.

**What we adopt**: The simplicity principle -- Nostr's identity model is just a keypair. We keep that simplicity by sharing the key rather than layering delegation on top.

### Hierarchical Deterministic Key Derivation (BIP32 / SLIP-0010)

HD derivation generates a tree of child keys from a master seed. Used in cryptocurrency wallets (BIP32 for secp256k1, SLIP-0010 for Ed25519). Each derivation path produces a unique keypair.

**What we adopt**: Hardened derivation for deriving signing keys from the master key. `signing_secret[i] = HKDF(master_secret, index=i)`. Compromising a derived signing key does NOT reveal the master key. The master can derive a new signing key and publish a signed rotation.

**What we reject**: Non-hardened (public) derivation. While it allows anyone to verify `master_pubkey + index -> signing_pubkey`, compromising any derived private key trivially recovers the master private key (`master_secret = signing_secret - H(master_pub || i)`). This defeats the entire purpose of key separation. We use hardened derivation and rely on the master key signing a delegation statement to link master and signing keys.

### Multi-Signature / Threshold Signatures (MuSig2 / FROST)

MuSig2 produces a single Schnorr signature from multiple cooperating signers. FROST (RFC 9591) is a threshold scheme where t-of-n signers produce a valid signature.

**What we reject**: Both require multiple devices online to sign. MuSig2 needs all signers; FROST needs a threshold. Neither works for "post from any single device offline." FROST with t=1 is just secret sharing with extra cryptographic overhead.

---

## Key Architecture

### Three-Tier Hierarchy

```
Master Key (Ed25519) -- permanent identity, cold storage
  - master_public = the permanent, unforgeable identity
  - Signs signing key delegations and rotations
  - Stored as master_key.key (32 bytes) on the primary device only
  - Can be backed up (paper key, encrypted USB, etc.)
  - NEVER used for content signing or DM encryption

Signing Key (Ed25519) -- derived from master, shared across all linked devices
  - Derived: signing_secret[i] = HKDF-SHA256(master_secret, info="iroh-social/signing-key", salt=i)
  - Signs posts, interactions, profiles, registrations
  - Derives X25519 key for DM encryption (Noise IK handshake)
  - Stored as signing_key.key (32 bytes) on all linked devices
  - Rotatable: master derives a new one if compromised

Transport Key (Ed25519) -- derived from master + device index, unique per device
  - Derived: transport_secret = HKDF-SHA256(master_secret, info="iroh-social/transport-key", salt=device_index)
  - Primary device uses device_index=0, each linked device gets a unique index
  - iroh EndpointId / NodeId
  - QUIC transport authentication
  - Gossip participation
  - Never used for content signing or DM encryption
  - Passed to iroh via Endpoint::builder().secret_key()
  - Stable across restarts (same master key + device index = same transport NodeId)
```

### Signing Key Derivation

The signing key is derived from the master key using HKDF with a key index:

```rust
use hkdf::Hkdf;
use sha2::Sha256;

/// Derive a signing key from the master key at a given index.
/// Hardened derivation: compromising the signing key does NOT reveal the master key.
fn derive_signing_key(master_secret: &[u8; 32], index: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&index.to_be_bytes()), master_secret);
    let mut signing_secret = [0u8; 32];
    hk.expand(b"iroh-social/signing-key", &mut signing_secret)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    signing_secret
}
```

Properties:
- **Hardened**: knowing `signing_secret[i]` does not reveal `master_secret` or `signing_secret[j]` for any `j != i`.
- **Deterministic**: the same `(master_secret, index)` always produces the same signing key.
- **Cheap**: HKDF is a single hash operation. No complex curve math.

### Transport Key Derivation

The transport key is derived from the master key using HKDF with a device-specific index as the salt. Each linked device uses a unique index, ensuring distinct NodeIds on the network:

```rust
/// Derive a stable transport key from the master key for a given device index.
/// Each device uses a unique index so linked devices get distinct NodeIds.
/// The primary device uses index 0.
fn derive_transport_key(master_secret: &[u8; 32], device_index: u32) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(&device_index.to_be_bytes()), master_secret);
    let mut transport_secret = [0u8; 32];
    hk.expand(b"iroh-social/transport-key", &mut transport_secret)
        .expect("32 bytes is a valid length for HKDF-SHA256");
    transport_secret
}
```

The derived key is passed to iroh via `Endpoint::builder().secret_key()`. This ensures:
- **Stable NodeId**: same master key + device index always produces the same transport NodeId across app restarts.
- **Unique per device**: different device indices produce different NodeIds, so linked devices don't collide on the network.
- **Independence**: the transport key is cryptographically independent from the signing key (different HKDF info).
- **Deterministic**: no separate key file needed; the transport key is always recoverable from the master key + device index.

### Signing Key Delegation

The master key signs a delegation statement binding the current signing key to the identity:

```rust
/// Signed by the master key. Tells peers "this is my current signing key."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningKeyDelegation {
    /// The master public key (permanent identity).
    pub master_pubkey: String,
    /// The current signing public key (derived from master at this index).
    pub signing_pubkey: String,
    /// The derivation index (so the master can prove derivation if needed).
    pub key_index: u32,
    /// When this delegation was issued (Unix timestamp ms).
    pub issued_at: u64,
    /// Ed25519 signature from the master key over the canonical bytes.
    pub signature: String,
}
```

Peers cache this delegation. When verifying content:
1. Look up the cached `SigningKeyDelegation` for `post.author` (the master pubkey).
2. Extract the `signing_pubkey` from the delegation.
3. Check `post.signature` against the `signing_pubkey`.

Every user always has a separate master key and signing key. Even single-device users derive their signing key from the master key at index 0. The delegation is always present and peers always verify via the delegation.

### X25519 Key Derivation (DM Encryption)

The X25519 key for DM encryption is deterministically derived from the signing key:

```rust
// Ed25519 secret -> X25519 secret (standard derivation)
fn ed25519_secret_to_x25519(ed25519_secret: &[u8; 32]) -> [u8; 32] {
    let hash = Sha512::digest(ed25519_secret);
    let mut x25519 = [0u8; 32];
    x25519.copy_from_slice(&hash[..32]);
    x25519[0] &= 248;
    x25519[31] &= 127;
    x25519[31] |= 64;
    x25519
}
```

Since all devices share the same signing key, they all derive the same X25519 key. This means:
- Any device can participate in DM sessions as the same static key
- Peers see one consistent X25519 identity regardless of which device they connect to
- Ratchet state must be synchronized between devices (the hard problem)

### Key Files

| File | Purpose | Present on |
|------|---------|-----------|
| `master_key.key` | Ed25519 master secret (32 bytes), permanent identity | Primary device only (unless user transfers full control) |
| `signing_key.key` | Ed25519 signing secret (32 bytes), derived from master via HKDF | All linked devices |
| (derived from master) | Transport key, derived via HKDF from master key | Every device (deterministic) |

### Key Relationships

| Aspect | Key used |
|--------|----------|
| User identity (pubkey) | master_key.key's public key |
| Content signing | signing_key.key signs |
| DM encryption | X25519 derived from signing_key.key |
| Gossip topic | `user_feed_topic(master_pubkey)` |
| Transport / QUIC | Derived from master key via HKDF (NodeId) |

### First Launch (Fresh Install)

1. Generate a new Ed25519 keypair for `master_key.key`
2. Derive `signing_key.key` at index 0: `derive_signing_key(&master_secret, 0)`
3. Derive transport key: `derive_transport_key(&master_secret, 0)` -- device index 0 for primary, passed to iroh via `Endpoint::builder().secret_key()`
4. Sign a `SigningKeyDelegation` binding the signing key to the master key
5. Store master key bytes in `AppState::master_secret_key_bytes`
6. Store signing key bytes in `AppState::signing_secret_key_bytes`

---

## Identity Model

### Permanent Identity (Master Key)

The user's permanent, unforgeable identity is the master key's public key. This is what peers follow and what survives key rotations. It appears in:
- Follow lists (following/followers)
- Gossip topic derivation (`user_feed_topic(master_pubkey)`)
- `SigningKeyDelegation` (links master to current signing key)
- `KeyRotation` announcements

### Signing Identity (Signing Key)

The signing key is the day-to-day signing and encryption key. It does NOT appear in the `author` field -- `author` is always the master pubkey. The signing key appears only in:
- Content signatures (the actual bytes that sign posts/interactions/profiles)
- DM encryption (X25519 derivation for Noise IK handshake)
- Server registration signatures
- `SigningKeyDelegation` (linking signing key to master key)

Peers look up the signing key via the cached delegation for the master pubkey, then verify signatures against it. For the common case (no key rotation), this is a one-time cached lookup.

### Transport Identity

Each device's iroh NodeId (from iroh's internal key) is used for:
- QUIC connections
- Peer addressing (how to reach this specific device)
- Gossip network participation (iroh endpoint identity)

### Separation of Concerns

Three layers, each with a clear role:
- **Master key** (permanent identity): what peers follow. Survives key rotation. Rarely used directly.
- **Signing key** (signing/DM identity): what signs content and encrypts DMs. Rotatable by master.
- **Transport key** (network identity): how to reach a specific device. Implementation detail.

---

## Device Registry

Peers need to know which transport keys (iroh NodeIds) belong to a user identity so they can:
- Route sync requests to any of a user's devices
- Deliver DMs to online devices
- Display device information in the UI

### LinkedDevicesAnnouncement

Published via gossip on the user's feed topic (`user_feed_topic(master_pubkey)`). Signed by the signing key.

```rust
/// Announces the set of devices linked to an identity.
/// Published via gossip. Peers cache the latest version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkedDevicesAnnouncement {
    /// The user's master public key (permanent identity).
    pub master_pubkey: String,
    /// The current signing key delegation (signed by master key).
    pub delegation: SigningKeyDelegation,
    /// All currently active devices.
    pub devices: Vec<DeviceEntry>,
    /// Monotonically increasing version number. Latest wins.
    pub version: u64,
    /// When this announcement was created (Unix timestamp ms).
    pub timestamp: u64,
    /// Ed25519 signature from the signing key over the canonical bytes.
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// The device's iroh NodeId (transport key public key).
    pub node_id: String,
    /// Human-readable name.
    pub device_name: String,
    /// Whether this is the primary device (the one that originally created the signing key).
    pub is_primary: bool,
    /// When this device was added (Unix timestamp ms).
    pub added_at: u64,
}
```

### GossipMessage Extension

```rust
pub enum GossipMessage {
    NewPost(Post),
    DeletePost {
        id: String,
        author: String,
        signature: String,   // signed by signing key
    },
    ProfileUpdate(Profile),
    NewInteraction(Interaction),
    DeleteInteraction {
        id: String,
        author: String,
        signature: String,   // signed by signing key
    },
    // New:
    LinkedDevices(LinkedDevicesAnnouncement),
}
```

Note: `DeletePost` and `DeleteInteraction` gain a `signature` field (signed by signing key). Since multiple transport NodeIds now publish to the same gossip topic, unsigned deletes would allow any topic participant to forge deletions. No `device_pubkey` field is needed because all devices sign with the same signing key.

### Delete Signing

```rust
fn delete_post_signing_bytes(id: &str, author: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "delete_post",
        "id": id,
        "author": author,
    }))
    .expect("json serialization should not fail")
}

pub fn sign_delete_post(id: &str, author: &str, secret_key: &SecretKey) -> String {
    let bytes = delete_post_signing_bytes(id, author);
    let sig = secret_key.sign(&bytes);
    signature_to_hex(&sig)
}

pub fn verify_delete_post_signature(
    id: &str, author: &str, signature: &str,
) -> Result<(), String> {
    let sig = hex_to_signature(signature)?;
    let author_key: PublicKey = author.parse()
        .map_err(|e| format!("invalid author pubkey: {e}"))?;
    let bytes = delete_post_signing_bytes(id, author);
    author_key.verify(&bytes, &sig)
        .map_err(|_| "delete post signature verification failed".to_string())
}
```

Same pattern for `delete_interaction`.

### Peer-Side Caching

When a peer receives a `LinkedDevicesAnnouncement`:

1. Verify the signature against `signing_pubkey`.
2. Check that `version` is greater than any previously cached version for this identity.
3. Store the announcement (replace previous version).
4. Use the device list for routing DMs and sync requests.

Announcements are also sent via the sync protocol so that peers who come online later receive them. This requires extending `SyncFrame` with a new variant:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub enum SyncFrame {
    Posts(Vec<Post>),
    Interactions(Vec<Interaction>),
    DeviceAnnouncements(Vec<LinkedDevicesAnnouncement>),  // NEW
}
```

---

## Identity Resolution

The master pubkey is the permanent identity, but it is NOT a transport address. To connect to someone (gossip, sync, DM), you need at least one of their transport NodeIds. This section describes how peers resolve master pubkeys to transport endpoints.

### The Problem

Currently, `pubkey == iroh NodeId`, so following someone is trivial: parse the pubkey as an EndpointId and connect. With the three-tier hierarchy, the master pubkey has no direct network presence. The gossip subscription code currently does:

```rust
let topic = user_feed_topic(pubkey);
let bootstrap: EndpointId = pubkey.parse()?;  // THIS BREAKS -- master pubkey is not a NodeId
self.gossip.subscribe(topic, vec![bootstrap]).await?;
```

### Primary Resolution: Direct Peer Query

If you know any transport NodeId for a user, you can connect to it on `PEER_ALPN` and ask "who are you?" The device responds with its full identity info. This is purely P2P -- no server involved.

```rust
pub enum PeerRequest {
    Sync(SyncRequest),
    Push(PushMessage),
    FollowRequest(FollowRequest),
    IdentityRequest,  // NEW: "who are you?"
}

pub enum PeerResponse {
    SyncSummary(SyncSummary),
    PushAck(PushAck),
    FollowResponse(FollowResponse),
    Identity(IdentityResponse),  // NEW
}

/// Response to an IdentityRequest. Any device can answer this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityResponse {
    /// The user's permanent identity.
    pub master_pubkey: String,
    /// Proves the signing key is authorized by the master key.
    pub delegation: SigningKeyDelegation,
    /// All linked devices (transport NodeIds).
    pub devices: Vec<DeviceEntry>,
    /// Current profile.
    pub profile: Profile,
}
```

Any device can answer `IdentityRequest` because all devices know the master pubkey, hold the delegation, and know the device list.

### Resolution Sources

Transport NodeIds for a master pubkey can come from multiple sources, in priority order:

1. **Direct peer query** -- If you have a transport NodeId (from a link, QR code, gossip), connect on `PEER_ALPN`, send `IdentityRequest`, receive `IdentityResponse` with master pubkey, delegation, device list, and profile. Cache everything.

2. **Local cache** -- The `peer_device_announcements` table stores the latest `LinkedDevicesAnnouncement` for each peer, containing all their device NodeIds. Populated from previous gossip/sync/identity query interactions.

3. **Follow source** -- When you discover a user (via QR code, another user's repost, a link), the source provides at least one transport NodeId alongside the master pubkey.

4. **Gossip network** -- If you're already connected to a gossip topic where the user participates (e.g., a mutual follow's topic), you may discover their NodeId from gossip peer metadata.

5. **Discovery server (optional fallback)** -- If the client is registered with a server, it can query the server's device lookup endpoint. This is never required -- the protocol works fully P2P.

### Registration Changes (Server, Optional)

If a user opts in to a discovery server, the registration must include both the master pubkey (identity) and a transport NodeId (how to reach this device). The registration payload becomes:

```rust
pub struct RegistrationPayload {
    pub master_pubkey: String,      // permanent identity
    pub transport_node_id: String,  // this device's iroh NodeId
    pub delegation: SigningKeyDelegation,  // proves signing key is authorized by master
    pub server_url: String,
    pub timestamp: u64,
    pub visibility: Visibility,
    pub action: Option<String>,
}
```

The signature is produced by the signing key. The server verifies it against `delegation.signing_pubkey` after checking the delegation's master signature.

When a user has multiple devices, each device registers independently with the same `master_pubkey` but different `transport_node_id`. The server merges these into a single user record with multiple transport endpoints.

### Server Device Lookup API (Optional)

If a discovery server is available, it can serve as a fallback for resolving master pubkeys to transport NodeIds:

```
GET /api/v1/user/{master_pubkey}/devices
```

Response:

```json
{
  "master_pubkey": "...",
  "devices": [
    { "node_id": "...", "device_name": "Desktop", "is_primary": true, "last_seen": 1709913600000 },
    { "node_id": "...", "device_name": "Phone", "is_primary": false, "last_seen": 1709913540000 }
  ],
  "delegation": { ... }
}
```

This endpoint is public (no auth required). It is a convenience for clients that have server access, but the protocol works without it.

### Follow Flow

Following always starts from a transport NodeId -- you need to reach someone to follow them. The transport NodeId comes from the follow source (QR code, link, repost, gossip).

```
1. User discovers a transport NodeId N (from QR code, link, repost, etc.)
2. Connect to N on PEER_ALPN, send IdentityRequest
3. Receive IdentityResponse: master_pubkey M, delegation, device list, profile
4. Cache delegation and device list locally
5. Store follow relationship keyed by master_pubkey M
6. Subscribe to user_feed_topic(M) with bootstrap = [all device NodeIds from response]
7. Initiate sync from peer via any available transport NodeId
8. Receive LinkedDevicesAnnouncement via gossip -> update cached device list
```

If the user already has the master pubkey (e.g., re-following after app reinstall):
```
1. Check local cache for transport NodeIds
2. If cache empty and server is configured, query server as fallback
3. If no transport NodeIds available, follow cannot proceed until the user provides one
```

### Gossip Subscription (Updated)

```rust
pub async fn subscribe(self: &Arc<Self>, master_pubkey: &str) -> anyhow::Result<()> {
    let topic = user_feed_topic(master_pubkey);

    // Resolve transport NodeIds from local cache (populated by IdentityRequest or gossip)
    let node_ids = self.resolve_transport_nodes(master_pubkey).await?;
    let bootstrap: Vec<EndpointId> = node_ids
        .iter()
        .filter_map(|id| id.parse().ok())
        .collect();

    if bootstrap.is_empty() {
        return Err(anyhow::anyhow!("no transport nodes known for {}", &master_pubkey[..8]));
    }

    let topic_handle = self.gossip.subscribe(topic, bootstrap).await?;
    // ... rest of subscription logic
}

/// Resolve master pubkey -> transport NodeIds.
/// Checks local cache first. The cache is populated by:
/// - IdentityRequest responses (primary path)
/// - LinkedDevicesAnnouncement via gossip
/// - Server lookup (optional fallback, if configured)
async fn resolve_transport_nodes(&self, master_pubkey: &str) -> anyhow::Result<Vec<String>> {
    // 1. Check local cache (populated by direct peer query or gossip)
    if let Ok(devices) = self.storage.get_peer_devices(master_pubkey).await {
        if !devices.is_empty() {
            return Ok(devices.iter().map(|d| d.node_id.clone()).collect());
        }
    }

    // 2. Optional fallback: query discovery server if configured
    if let Some(server_url) = self.storage.get_discovery_server().await? {
        if let Ok(response) = self.query_server_devices(&server_url, master_pubkey).await {
            if let Some(announcement) = response.announcement {
                let _ = self.storage.cache_peer_announcement(&announcement).await;
            }
            return Ok(response.node_ids);
        }
    }

    Err(anyhow::anyhow!("cannot resolve transport nodes for {}", &master_pubkey[..8]))
}
```

### User Profile Links and QR Codes

When sharing a user profile (QR code, web link, copy-paste), the payload must include the master pubkey and all known transport NodeIds. Multiple `n` parameters allow the follower to try each device until one responds:

```
iroh-social://user?m=<master_pubkey>&n=<node_id_1>&n=<node_id_2>
```

The follower iterates through the `n` values, connecting to each on `PEER_ALPN` and sending `IdentityRequest` until one responds. This handles the case where one device is offline but another is reachable.

The web frontend profile page at `/user/{master_pubkey}` should include all known transport NodeIds in its metadata so the Tauri client can resolve the identity even if the primary device is offline.

### Server Gossip Bootstrap

When the server (if used) subscribes to a registered user's gossip topic, it resolves transport NodeIds from its own registration database rather than treating the pubkey as a NodeId:

```rust
// Server subscribing to a registered user's gossip
let topic = user_feed_topic(master_pubkey);
let transport_nodes = self.storage.get_registered_transport_nodes(master_pubkey).await?;
let bootstrap: Vec<EndpointId> = transport_nodes
    .iter()
    .filter_map(|id| id.parse().ok())
    .collect();
self.gossip.subscribe(topic, bootstrap).await?;
```

---

## Pairing Protocol

### Overview

Pairing transfers the signing secret key from an existing device to a new device. This is the most security-critical operation -- the signing key is the full identity. The transfer happens over an encrypted channel authenticated by a one-time shared secret (QR code or pairing code).

```
Existing Device                     New Device
   |                                     |
   |  1. User taps "Link New Device"     |
   |     Existing generates:             |
   |     - one-time secret (32 bytes)    |
   |     - QR code / pairing code        |
   |                                     |
   |  2. User scans QR / enters code     |
   |     on new device                   |
   |                         <---------  |
   |                                     |
   |  3. New device connects via QUIC    |
   |     on PEER_ALPN                    |
   |     <----------------------------   |
   |                                     |
   |  4. Noise IK + PSK handshake        |
   |     (QR secret as pre-shared key)   |
   |     ---------------------------->   |
   |     <----------------------------   |
   |                                     |
   |  5. New device sends its transport  |
   |     pubkey and device name          |
   |     <----------------------------   |
   |                                     |
   |  6. Existing device derives          |
   |     transport key for new device    |
   |     at next available device_index  |
   |     and sends LinkBundle:           |
   |     - User secret key (32 bytes)    |
   |     - Transport secret key for B    |
   |     - Profile data                  |
   |     - Follow list                   |
   |     - Bookmarks, mutes, blocks      |
   |     - DM ratchet sessions           |
   |     ---------------------------->   |
   |                                     |
   |  7. New device confirms receipt     |
   |     <----------------------------   |
   |                                     |
   |  8. Existing device publishes       |
   |     updated LinkedDevicesAnnounce.  |
   |     via gossip                      |
   |                                     |
   |  [Paired. New device begins         |
   |   independent operation.]           |
```

### Protocol Integration

Device linking uses the existing `PEER_ALPN` with new `PeerRequest::LinkRequest` and `PeerResponse::LinkBundle` variants. No new ALPN is needed.

### QR Code Content

```rust
/// Encoded in the QR code displayed by the existing device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkQrPayload {
    /// Existing device's iroh NodeId (for QUIC connection).
    pub node_id: String,
    /// One-time secret for Noise PSK (32 bytes, base64-encoded).
    pub secret: String,
    /// Existing device's network addresses for direct connection.
    pub addrs: String,
}
```

Serialized as a compact URI: `iroh-social://link?n=<node_id>&s=<secret>&a=<addrs>`

The QR code expires after 60 seconds. The one-time secret is generated fresh each time the user initiates pairing.

### Noise IK + PSK Handshake

Use `Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s` -- the Noise IK pattern with an additional pre-shared key (the QR secret). This ensures:

1. **Authentication** -- Both sides know each other's static keys after handshake.
2. **QR verification** -- Only someone who scanned the QR code knows the PSK, preventing MITM.
3. **Forward secrecy** -- Ephemeral keys ensure the pairing channel cannot be decrypted later.

Both sides use their iroh transport X25519 keys (derived from iroh Ed25519 keys) for the Noise handshake. The pairing channel authenticates transport identities; the signing key is payload transferred inside the encrypted channel.

### LinkBundle

Data sent from existing device to new device during pairing. Includes the signing secret key and delegation. The master secret key is only transferred if the existing device holds it (primary device) and the user explicitly opts in.

```rust
/// Data bundle sent during device pairing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkBundle {
    /// The signing Ed25519 secret key (32 bytes, base64-encoded).
    /// Derived from master key. Shared across all devices.
    pub signing_secret_key: String,
    /// The signing key delegation (signed by master key).
    pub delegation: SigningKeyDelegation,
    /// The new device's transport secret key (32 bytes, base64-encoded).
    /// Derived by the existing device: derive_transport_key(master_secret, new_device_index).
    /// The new device stores this directly -- it does NOT need the master key to derive it.
    pub transport_secret_key: String,
    /// The device index used to derive the transport key.
    /// The new device stores this so it can be included in the device registry.
    pub device_index: u32,
    /// The master secret key (32 bytes, base64-encoded).
    /// ONLY included if the sending device holds it AND user opts in.
    /// None for secondary-to-secondary pairing or if user chooses not to share.
    pub master_secret_key: Option<String>,
    /// User profile.
    pub profile: Profile,
    /// Follow list.
    pub follows: Vec<FollowEntry>,
    /// Bookmarked post IDs.
    pub bookmarks: Vec<(String, String)>,  // (author, post_id)
    /// Blocked user pubkeys.
    pub blocked_users: Vec<String>,
    /// Muted user pubkeys.
    pub muted_users: Vec<String>,
    /// Current DM ratchet sessions (serialized).
    /// The new device needs these to continue existing DM conversations.
    pub ratchet_sessions: Vec<RatchetSessionExport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetSessionExport {
    pub peer_pubkey: String,
    pub state_json: String,
}
```

Security notes:
- The `signing_secret_key` controls signing and DM. Compromise allows impersonation until the master key rotates it.
- The `transport_secret_key` is derived by the existing device from the master key using `derive_transport_key(master_secret, device_index)`. The new device stores it directly and passes it to iroh via `Endpoint::builder().secret_key()`. The new device never sees the master key material used to derive it.
- The `master_secret_key` (if included) is the permanent identity. Its compromise is unrecoverable.
- Users should be warned before transferring the master key. Default: do NOT include it. Only include if the user explicitly chooses "transfer full control."
- **Secondary-to-secondary pairing**: A secondary device (one without the master key) can still pair a new device by sharing the signing key and DM sessions, but it CANNOT derive a new transport key. In this case, the existing device must either (a) have a pre-derived transport key assigned to it for this purpose, or (b) the pairing must go through the primary device. The simplest approach: only the primary device (or any device holding the master key) can initiate "Link New Device."
- The Noise IK + PSK channel provides end-to-end encryption and authentication.
- The one-time PSK prevents MITM attacks on the QUIC connection.
- After successful transfer, all secret key bytes should be zeroized from the transfer buffer.

### Desktop-to-Desktop Pairing

When a camera is unavailable (e.g., pairing two desktops), the QR payload can be displayed as a text code that the user copies and pastes into the new device's pairing dialog.

---

## Signing and Verification

Every device signs with the same signing key. The `author` field on all content is the **master pubkey** (the permanent identity). Peers look up the signing key via the cached delegation for that master pubkey, then verify the signature. The delegation is cached, so verification is effectively one signature check per post plus a one-time delegation lookup per identity.

### Post Signing

The `author` field is the master pubkey. The signature is produced by the signing key's secret key.

```rust
pub struct Post {
    pub id: String,
    pub author: String,           // master pubkey (permanent identity)
    pub content: String,
    pub timestamp: u64,
    pub media: Vec<MediaAttachment>,
    pub reply_to: Option<String>,
    pub reply_to_author: Option<String>,
    pub quote_of: Option<String>,
    pub quote_of_author: Option<String>,
    pub signature: String,        // signed by signing key (NOT master key)
}
```

#### Signing

```rust
/// Sign a post. Uses the signing secret key (derived from master).
/// The `author` field is set to the master pubkey.
pub fn sign_post(post: &mut Post, signing_secret_key: &SecretKey) {
    let bytes = post_signing_bytes(post);
    let sig = signing_secret_key.sign(&bytes);
    post.signature = signature_to_hex(&sig);
}
```

#### Verification

```rust
/// Verify a post's signature.
/// 1. Look up the SigningKeyDelegation for post.author (master pubkey).
/// 2. Verify the signature against the delegated signing_pubkey.
/// Fails if no delegation is cached -- every user must have one.
pub fn verify_post_signature(
    post: &Post,
    get_delegation: impl Fn(&str) -> Option<SigningKeyDelegation>,
) -> Result<(), String> {
    let sig = hex_to_signature(&post.signature)?;
    let bytes = post_signing_bytes(post);

    let delegation = get_delegation(&post.author)
        .ok_or_else(|| format!("no delegation cached for {}", &post.author))?;

    let key: PublicKey = delegation.signing_pubkey.parse()
        .map_err(|e| format!("invalid pubkey: {e}"))?;
    key.verify(&bytes, &sig)
        .map_err(|_| "post signature verification failed".to_string())
}
```

This means:
- **On key rotation**: old posts still verify because the old signing key signed them. Peers must cache the delegation history (old and new signing keys for the same master pubkey) or accept that old posts may fail verification after rotation. Simpler approach: old posts are already stored and trusted; only new incoming posts need verification against the current delegation.
- **`reply_to_author` and `quote_of_author`**: these are always master pubkeys, so they remain stable across rotations.
- **Follow lists, mentions, profile lookups**: all keyed by master pubkey. No change on rotation.

### Interaction Signing

Same pattern. `interaction.author` = master pubkey, signature by signing key. Verification uses delegation lookup.

### Profile Signing

With multiple devices publishing to the same gossip topic, profiles need signing to prevent forgery. A malicious node on the gossip topic could inject a fake `ProfileUpdate` without this.

```rust
pub struct Profile {
    pub display_name: String,
    pub bio: String,
    pub avatar_hash: Option<String>,
    pub avatar_ticket: Option<String>,
    pub visibility: Visibility,
    pub signature: String,        // NEW: signed by signing key
}
```

#### Signing

```rust
fn profile_signing_bytes(profile: &Profile) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "display_name": profile.display_name,
        "bio": profile.bio,
        "avatar_hash": profile.avatar_hash,
        "avatar_ticket": profile.avatar_ticket,
        "visibility": profile.visibility,
    }))
    .expect("json serialization should not fail")
}

pub fn sign_profile(profile: &mut Profile, signing_secret_key: &SecretKey) {
    let bytes = profile_signing_bytes(profile);
    let sig = signing_secret_key.sign(&bytes);
    profile.signature = signature_to_hex(&sig);
}
```

#### Verification

```rust
pub fn verify_profile_signature(
    profile: &Profile,
    expected_master_pubkey: &str,
    get_delegation: impl Fn(&str) -> Option<SigningKeyDelegation>,
) -> Result<(), String> {
    let sig = hex_to_signature(&profile.signature)?;
    let bytes = profile_signing_bytes(profile);

    let delegation = get_delegation(expected_master_pubkey)
        .ok_or_else(|| format!("no delegation cached for {}", expected_master_pubkey))?;

    let key: PublicKey = delegation.signing_pubkey.parse()
        .map_err(|e| format!("invalid pubkey: {e}"))?;
    key.verify(&bytes, &sig)
        .map_err(|_| "profile signature verification failed".to_string())
}
```

`expected_master_pubkey` comes from the gossip topic owner (for gossip-received profiles) or `SyncRequest.author` (for sync-received profiles). Both are master pubkeys.

### Registration Signing

Server registration uses the signing key for the signature, but the registration payload identifies the user by master pubkey. The server caches the `SigningKeyDelegation` to verify the signature against the correct signing key.

---

## DM Multi-Device

### Architecture

All devices share the same X25519 static key (derived from the shared Ed25519 signing key). This means a peer establishes ONE ratchet session per user identity (master pubkey), regardless of how many devices that user has.

```
Alice (master: MK_A, signing key: UK_A, X25519: XK_A)
  Device A1 (transport: TK_A1)  -- has UK_A, derives XK_A
  Device A2 (transport: TK_A2)  -- has UK_A, derives XK_A

Bob (master: MK_B, signing key: UK_B, X25519: XK_B)
  Device B1 (transport: TK_B1)  -- has UK_B, derives XK_B

Ratchet session (ONE per user pair, keyed by master pubkeys):
  (MK_A, MK_B): Noise IK(XK_A, XK_B) -> ratchet_AB
```

### The Ratchet Sync Problem

The critical challenge: when Device A1 sends a DM (advancing the ratchet), Device A2's copy of the ratchet is stale. If A2 tries to send with the old ratchet state, it produces ciphertext that Bob cannot decrypt (wrong chain key).

#### Solution: Ratchet State Sync via Device Sync

Ratchet sessions are synchronized between linked devices as part of the device sync protocol (see [Device Sync](#device-sync)). The sync interval is short (60 seconds) and sync is triggered on reconnection.

**Conflict resolution for ratchet state**: Last-write-wins by the `updated_at` timestamp on the ratchet session. If both devices advance the ratchet simultaneously (both send a DM in the same 60-second window), the device with the later timestamp wins. The "losing" device's messages may fail to decrypt on the recipient's side -- the recipient should request re-send.

**Practical mitigation**: In practice, a user is typically active on one device at a time. The sync interval (60s) is short enough that switching devices mid-conversation works if you wait a moment. The DM UI can show a "syncing..." indicator when a ratchet is stale.

**Message ordering**: Each device includes a monotonic sequence number in DM messages. The recipient uses this to detect gaps and request re-delivery.

### DM Sending

When Alice sends a DM to Bob from any device:

1. The device uses the shared X25519 key (from signing key) to encrypt.
2. It looks up Bob's devices from the cached `LinkedDevicesAnnouncement`.
3. It sends the encrypted message to Bob's available devices via their transport NodeIds.
4. All of Bob's devices can decrypt (they all have Bob's signing key -> X25519 key).

```rust
/// Send a DM to a peer. Delivers to all of their online devices.
/// peer_master_pubkey is the permanent identity of the recipient.
async fn send_dm(
    &self,
    peer_master_pubkey: &str,
    content: &str,
) -> anyhow::Result<()> {
    let ratchet = self.get_or_establish_session(peer_master_pubkey).await?;
    let (header, ciphertext) = ratchet.encrypt(content.as_bytes());

    // Send to all known devices of this peer
    let devices = self.get_peer_devices(peer_master_pubkey)?;
    for device in &devices {
        self.send_encrypted_to_node(&device.node_id, header.clone(), ciphertext.clone()).await?;
    }
    Ok(())
}
```

Note: the same ciphertext is sent to all of the recipient's devices (unlike the per-device-key model where each device gets separately encrypted copies). This is because all devices share the same X25519 key and ratchet state.

### DM Receiving

Any of the recipient's devices can decrypt the message (same ratchet state, same keys). The device that receives it advances the ratchet and syncs the new state to other devices.

### Own-Device DM Receipt

When you send a DM from Device A1, Device A2 does not receive the ciphertext directly (Bob sends the response to whichever of Alice's devices he reaches). DM history is synced between own devices via the device sync protocol. The plaintext messages are stored locally and synced as structured data (not re-encrypted).

### DM Handshake Identity

The DM handshake uses the shared X25519 key as the Noise static key. The Noise IK pattern authenticates the static key during the handshake. Since all devices present the same X25519 static key, the peer cannot distinguish which device it is talking to at the Noise level -- which is correct behavior for a shared identity.

The DM ALPN does NOT need to change version. The wire format is the same. The only difference is that the X25519 key is now derived from the signing key rather than the iroh transport key. This is transparent to the protocol.

### DM Delivery and Offline Queuing

The client already implements a sender-side outbox queue (`dm_outbox` table) with a 15-second background flush loop. When `try_send_envelope()` fails, the encrypted envelope is queued locally and retried periodically. The outbox has `retry_count` and `last_retry_at` columns prepared for future backoff improvements.

For multi-device, the flush logic extends to try all of the recipient's known devices (from their cached `LinkedDevicesAnnouncement`), not just one NodeId. If any device accepts the message, remove it from the outbox.

#### Server-Side DM Store (Opt-In)

Users who register with a community server can opt in to server-side DM storage. The server holds encrypted DM payloads for offline recipients without being able to read them.

How it works:
1. The sender, after failing to reach the recipient directly, checks if the recipient is registered on any known server.
2. If so, the sender pushes the encrypted payload to the server via an API endpoint.
3. When the recipient comes online, it polls its registered server for pending DMs and retrieves them.
4. The server deletes stored DMs after successful retrieval (or after a TTL, e.g. 7 days).

The server never has the decryption key -- it stores opaque ciphertext. This is strictly opt-in: users who don't register with a server rely entirely on sender-side queuing and direct delivery.

```rust
// Server API endpoints
// POST /api/dm/store   -- sender pushes encrypted DM for an offline recipient
// GET  /api/dm/pending -- recipient retrieves queued DMs
// POST /api/dm/ack     -- recipient acknowledges retrieval, server deletes

/// Encrypted DM stored on the server for offline delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDm {
    /// Unique message ID (for deduplication and ack).
    pub message_id: String,
    /// Sender's master pubkey.
    pub sender_master_pubkey: String,
    /// Recipient's master pubkey.
    pub recipient_master_pubkey: String,
    /// Opaque encrypted payload (the server cannot decrypt this).
    pub encrypted_payload: Vec<u8>,
    /// When the server received this message.
    pub stored_at: u64,
}
```

```sql
-- Server-side table
CREATE TABLE stored_dms (
    message_id TEXT PRIMARY KEY,
    sender_master_pubkey TEXT NOT NULL,
    recipient_master_pubkey TEXT NOT NULL,
    encrypted_payload BLOB NOT NULL,
    stored_at INTEGER NOT NULL
);
CREATE INDEX idx_stored_dms_recipient ON stored_dms(recipient_master_pubkey);
```

The server enforces limits: max payload size (64KB), max stored DMs per recipient (1000), TTL (7 days). This prevents abuse without requiring the server to understand the content.

---

## Device Sync

Device sync keeps linked devices consistent. It is more complex than the per-device-key model because DM ratchet state must be synchronized.

### What Needs Sync

| Data | Needs sync? | Strategy |
|------|-------------|----------|
| Profile | Yes | LWW by timestamp |
| Follows | Yes | LWW-per-entry by timestamp |
| Posts (own) | Yes | Set union by post id |
| Interactions (own) | Yes | Set union by interaction id |
| DM ratchet sessions | Yes | LWW by updated_at per session |
| DM message history | Yes | Set union by message_id |
| Bookmarks | Yes | Set union |
| Mutes / Blocks | Yes | LWW-per-entry by timestamp |

### Sync Protocol Integration

Device sync uses the existing `PEER_ALPN` with new `PeerRequest::DeviceSync` and `PeerResponse::DeviceSyncData` variants. No new ALPN is needed.

### Sync Protocol

A lightweight protocol between linked devices. Authentication: during the QUIC connection on `PEER_ALPN`, both sides prove they hold the signing secret key by signing a challenge with it. This ensures only devices with the shared signing key can sync.

```rust
/// Summary of what a device has, exchanged at sync start.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceSyncVector {
    /// This device's transport NodeId.
    pub node_id: String,
    /// Timestamp of last profile update.
    pub profile_updated_at: u64,
    /// Timestamp of last follow list change.
    pub follows_updated_at: u64,
    /// Timestamp of last bookmark change.
    pub bookmarks_updated_at: u64,
    /// Timestamp of last mute/block change.
    pub moderation_updated_at: u64,
    /// Total post count and newest post timestamp (for post sync).
    pub post_count: u64,
    pub newest_post_timestamp: u64,
    /// Total interaction count and newest interaction timestamp.
    pub interaction_count: u64,
    pub newest_interaction_timestamp: u64,
    /// Per-peer latest ratchet session updated_at (for ratchet sync).
    pub ratchet_heads: Vec<(String, u64)>,  // (peer_pubkey, updated_at)
    /// Per-conversation latest message timestamp (for DM history sync).
    pub dm_conversation_heads: Vec<(String, u64)>,
}
```

### Sync Authentication

Both sides must prove they hold the signing key. This prevents a rogue device (which only has a transport key) from connecting and extracting ratchet state.

```rust
/// Challenge-response during device sync handshake.
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceSyncAuth {
    /// The user's pubkey (both sides should agree).
    pub signing_pubkey: String,
    /// This device's transport NodeId.
    pub node_id: String,
    /// Random challenge nonce.
    pub nonce: [u8; 32],
    /// Timestamp to prevent replay.
    pub timestamp: u64,
    /// Signature of (signing_pubkey || node_id || nonce || timestamp) by signing key.
    pub signature: String,
}
```

Flow:
1. Device A connects to Device B on `PEER_ALPN` and sends `PeerRequest::DeviceSync`.
2. Both send `DeviceSyncAuth` with a random nonce, signed by signing key.
3. Both verify the other's signature matches the expected user pubkey.
4. If verification fails, disconnect.
5. If verification succeeds, exchange `DeviceSyncVector` and sync.

### Sync Flow

1. Exchange `DeviceSyncAuth` (mutual authentication).
2. Exchange `DeviceSyncVector`.
3. Each device computes what the other is missing (timestamp comparison).
4. Missing data is streamed as length-prefixed JSON frames (same pattern as the existing peer sync protocol).
5. Both devices merge the received data.

### Conflict Resolution

| Category | Resolution |
|----------|-----------|
| Profile | Last-write-wins by timestamp |
| Follows | LWW-per-entry by timestamp |
| Bookmarks | Set union |
| Mutes/Blocks | LWW-per-entry by timestamp |
| Posts | Set union by post id (deduplicate) |
| Interactions | Set union by interaction id (deduplicate) |
| DM ratchet sessions | LWW by updated_at per peer session |
| DM messages | Set union by message_id (deduplicate) |

**LWW-per-entry** (last-write-wins per individual entry): Each follow/mute/block record keeps a `state` (active or removed) and a `last_changed_at` timestamp. When syncing, both devices exchange their full list for the category. For each entry present on either side, the entry with the latest `last_changed_at` wins. This correctly propagates both adds and removes.

Why not set union for follows/mutes/blocks: "set union (add wins over remove)" means removals can never propagate. If Alice unfollows Bob on her phone while her desktop is offline, the next sync would re-add Bob from the desktop's list. LWW-per-entry avoids this by letting the most recent action win regardless of direction.

### Sync Triggers

- **On reconnection** -- When a linked device comes online, sync immediately.
- **Periodic** -- Every 60 seconds while both devices are online.

---

## Device Revocation

Two tiers of revocation, matching the two tiers of keys:

### Tier 1: Trust-Based Device Removal (Simple)

Remove a device from the `LinkedDevicesAnnouncement` and stop syncing with it. The removed device still holds the signing key but peers stop routing to it. Appropriate for "I got a new phone" scenarios where the old device is wiped or trusted.

1. Remove the device from the device registry.
2. Publish an updated `LinkedDevicesAnnouncement` (incremented version, without the removed device).
3. Stop accepting device sync connections from that transport NodeId.

**Limitation**: The removed device can still sign content as the user. This is acceptable when removal is voluntary.

### Tier 2: Signing Key Rotation (Compromised Device)

This is where the master key architecture pays off. If a device is compromised, the master key derives a new signing key. The identity (master pubkey) is preserved -- only the signing/DM key changes.

1. The device holding the master key derives a new signing key at index `i+1`:

```rust
let new_signing_key = derive_signing_key(&master_secret, current_index + 1);
```

2. The master key signs a `SigningKeyRotation` announcement:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningKeyRotation {
    /// The master public key (permanent identity -- unchanged).
    pub master_pubkey: String,
    /// The old user pubkey (being revoked).
    pub old_signing_pubkey: String,
    /// The new user pubkey (replacing it).
    pub new_signing_pubkey: String,
    /// The new key's derivation index.
    pub new_key_index: u32,
    /// When the rotation was issued (Unix timestamp ms).
    pub timestamp: u64,
    /// Signed by the MASTER key (proves the identity owner initiated rotation).
    pub signature: String,
}
```

3. Publish via gossip on the identity's feed topic (`user_feed_topic(master_pubkey)`).
4. Publish a new `SigningKeyDelegation` for the new signing key (signed by master).
5. Re-pair remaining linked devices with the new signing key.
6. Re-register with discovery servers under the new signing key.
7. DM sessions must be re-established (new X25519 key from new signing key).

**What peers do on receiving a `SigningKeyRotation`**:
1. Verify the signature against `master_pubkey`.
2. Cache the new `SigningKeyDelegation`.
3. Reject content signed by `old_signing_pubkey` with timestamps after `rotation.timestamp`.
4. Accept content signed by `new_signing_pubkey` going forward.
5. The follow relationship (keyed by `master_pubkey`) is preserved -- no action needed.
6. DM sessions with the old X25519 key are invalidated. New sessions will be established on next DM exchange.

**Key advantage over flat shared-key model**: The identity (master pubkey) survives rotation. Peers' follow lists, mentions, and references to the user remain valid. Only the signing/DM key changes. Without a master key, rotation would mean a new identity entirely.

### What If the Master Key Device Is Compromised?

If the device holding the master key is the one that's compromised, the identity is fully compromised. This is the fundamental trade-off: the master key is a single point of failure, but it is also the single point of authority.

Mitigations:
- **Paper key backup**: Export the master key as a mnemonic / QR code and store offline. Even if the primary device is lost, the master key can be recovered on a new device.
- **Restrict master key distribution**: By default, only the primary device holds `master_key.key`. The pairing UI warns before transferring it. Most secondary devices hold only the signing key.

### GossipMessage Extension for Rotation

```rust
pub enum GossipMessage {
    // ... existing variants ...
    LinkedDevices(LinkedDevicesAnnouncement),
    SigningKeyRotation(SigningKeyRotation),  // NEW
}
```

---

## Storage

### New Migration

```sql
-- Device registry (own linked devices)
CREATE TABLE IF NOT EXISTS linked_devices (
    node_id TEXT PRIMARY KEY,       -- transport NodeId
    device_name TEXT NOT NULL,
    is_primary INTEGER NOT NULL DEFAULT 0,
    is_self INTEGER NOT NULL DEFAULT 0,  -- is this the current device?
    added_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL DEFAULT 0
);

-- Device sync state tracking
CREATE TABLE IF NOT EXISTS device_sync_state (
    node_id TEXT PRIMARY KEY,
    sync_vector_json TEXT NOT NULL DEFAULT '{}',
    last_sync_at INTEGER NOT NULL DEFAULT 0
);

-- Cached signing key delegations for OTHER users
CREATE TABLE IF NOT EXISTS peer_key_delegations (
    master_pubkey TEXT PRIMARY KEY,
    signing_pubkey TEXT NOT NULL,
    delegation_json TEXT NOT NULL,
    cached_at INTEGER NOT NULL
);

-- Cached device announcements for OTHER users (peer device discovery)
CREATE TABLE IF NOT EXISTS peer_device_announcements (
    master_pubkey TEXT PRIMARY KEY,
    announcement_json TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,
    cached_at INTEGER NOT NULL
);
```

### Schema Changes to Existing Tables

Add LWW state tracking to follows, mutes, and blocks:

```sql
-- Follows: add state + timestamp for LWW-per-entry conflict resolution.
-- Instead of deleting rows on unfollow, set state = 'removed'.
ALTER TABLE follows ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE follows ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;

-- Mutes: same pattern.
ALTER TABLE mutes ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE mutes ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;

-- Blocks: same pattern.
ALTER TABLE blocks ADD COLUMN state TEXT NOT NULL DEFAULT 'active';
ALTER TABLE blocks ADD COLUMN last_changed_at INTEGER NOT NULL DEFAULT 0;
```

Add `signature` and `updated_at` columns to the profiles table:

```sql
ALTER TABLE profiles ADD COLUMN signature TEXT NOT NULL DEFAULT '';
ALTER TABLE profiles ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
```

Add author to bookmarks:

```sql
ALTER TABLE bookmarks ADD COLUMN author TEXT NOT NULL DEFAULT '';
```

### Storage Methods

```rust
// Device management
fn save_linked_device(device: &LinkedDevice) -> Result<()>
fn remove_linked_device(node_id: &str) -> Result<()>
fn get_linked_devices() -> Result<Vec<LinkedDevice>>
fn update_device_last_seen(node_id: &str, timestamp: u64) -> Result<()>

// Peer identity and device info
fn cache_peer_delegation(delegation: &SigningKeyDelegation) -> Result<()>
fn get_peer_delegation(master_pubkey: &str) -> Result<Option<SigningKeyDelegation>>
fn cache_peer_announcement(announcement: &LinkedDevicesAnnouncement) -> Result<()>
fn get_peer_devices(master_pubkey: &str) -> Result<Vec<DeviceEntry>>

// Sync state
fn get_device_sync_state(node_id: &str) -> Result<Option<DeviceSyncVector>>
fn update_device_sync_state(node_id: &str, vector: &DeviceSyncVector) -> Result<()>

// Data export for pairing
fn export_link_bundle() -> Result<LinkBundle>
fn import_link_bundle(bundle: &LinkBundle) -> Result<()>

// Ratchet session export/import (for device sync and pairing)
fn export_ratchet_sessions() -> Result<Vec<RatchetSessionExport>>
fn import_ratchet_sessions(sessions: &[RatchetSessionExport]) -> Result<()>
```

---

## Client Integration

### AppState Changes

```rust
pub struct AppState {
    // Existing
    pub endpoint: Endpoint,
    pub gossip: Gossip,
    pub storage: Arc<Storage>,
    // ...

    // Master key (primary device only, None on secondary devices without it)
    pub master_secret_key_bytes: Option<[u8; 32]>,
    pub master_pubkey: String,  // permanent identity

    // Signing key (derived from master, shared across all devices)
    pub signing_secret_key_bytes: [u8; 32],
    pub signing_pubkey: String,
    pub signing_key_index: u32,  // derivation index

    // Transport identity (iroh's own key)
    pub transport_node_id: String,
}
```

### Protocol Handlers

Register new protocol handlers in the router:

The router does NOT need new ALPN registrations. Device linking and device sync are handled as new variants within the existing `PeerHandler` on `PEER_ALPN`:

```rust
let router = Router::builder(endpoint.clone())
    .accept(iroh_blobs::ALPN, blobs.clone())
    .accept(iroh_gossip::ALPN, gossip.clone())
    .accept(PEER_ALPN, peer_handler)  // handles Sync, Push, FollowRequest, IdentityRequest, LinkRequest, DeviceSync
    .accept(DM_ALPN, dm_handler.clone())
    .spawn();
```

The `PeerHandler` is extended with new `PeerRequest`/`PeerResponse` variants:
- `PeerRequest::LinkRequest(LinkPayload)` / `PeerResponse::LinkBundle(LinkBundle)` for pairing
- `PeerRequest::DeviceSync(DeviceSyncAuth)` / `PeerResponse::DeviceSyncData(...)` for device sync

### SyncHandler Update (Peer Sync)

The existing `PeerHandler` (for peer sync on `PEER_ALPN`) must be updated to accept sync requests where `SyncRequest.author` matches this device's master pubkey. Currently verification checks that the author matches the iroh NodeId; with separate transport keys, the NodeId differs from the identity. The handler must map from master pubkey to "this is my identity" via `AppState::master_pubkey`.

### Gossip Topic Participation

Each device subscribes to **two categories** of gossip topics:

1. **Own identity topic** -- `user_feed_topic(master_pubkey)` for the user's own identity. All linked devices join this topic so they can all receive interactions (replies, likes) and relay their own posts to followers. Multiple transport NodeIds participate in the same topic. The gossip layer handles this naturally -- each node joins as a topic peer.

2. **Followed users' topics** -- `user_feed_topic(followed_master_pubkey)` for every user the account follows. Since the follow list is synced between devices via Device Sync, each device knows the full set of followed users and independently subscribes to all their gossip topics. This means any online device receives real-time posts/interactions from followed users, even if the other linked devices are offline. When a device comes back online, Device Sync catches it up on anything it missed.

Bootstrap nodes for each followed user's topic come from the locally cached `LinkedDevicesAnnouncement` (which lists all their transport NodeIds). When a new device is linked, it receives the follow list via Device Sync, resolves transport NodeIds from the local cache (or via `IdentityRequest`), and subscribes to all followed users' topics.

When publishing to a gossip topic, content is signed by the signing key. The publishing NodeId (transport key) differs from the `author` field (master pubkey). Peers accept this because they verify the signature against the signing key (looked up via the delegation for `author`), not against the gossip sender's NodeId.

### Tauri Commands

```
// Pairing
start_device_link()                -> LinkQrPayload  // generates QR, starts listening
cancel_device_link()               -> ()
link_with_device(qr_payload)       -> ()             // scans QR, pairs, receives signing key

// Device management
get_linked_devices()               -> Vec<LinkedDevice>
get_device_info()                  -> DeviceInfo      // this device's info
unlink_device(node_id)             -> ()
rename_device(node_id, name)       -> ()

// Sync
force_device_sync()                -> { synced_items: u32 }
```

### Tauri Events

```
device-link-started    { qr_uri: String }
device-link-progress   { step: String }
device-linked          { device: LinkedDevice }
device-unlinked        { node_id: String }
device-sync-complete   { node_id, items: u32 }
```

### Frontend Pages

**`/settings/devices` page:**

- Shows this device's info (name, transport NodeId, is_primary badge)
- Lists all linked devices (name, NodeId, last seen)
- "Link New Device" button -- opens QR code modal
- "Link to Existing Device" button (for new/unlinked devices) -- opens scanner/paste modal
- Unlink button per device
- Rename device inline edit
- Force sync button

**QR Code Display:**

- Full-screen QR code modal with countdown timer (60s expiry)
- Pairing code displayed as text below QR (for desktop-to-desktop)
- "Waiting for new device to scan..." status
- Cancel button

**QR Scanner:**

- Camera viewfinder for scanning (reuses existing `ScannerModal.svelte`)
- Text input field for pasting pairing code
- Progress indicator during pairing
- Success/error states

### TypeScript Types

```typescript
interface LinkedDevice {
  node_id: string;
  device_name: string;
  is_primary: boolean;
  is_self: boolean;
  added_at: number;
  last_seen_at: number;
}

interface DeviceInfo {
  node_id: string;
  signing_pubkey: string;
  device_name: string;
  is_primary: boolean;
}
```

---

## Discovery Server Considerations

The discovery server subscribes to users' gossip topics and indexes their posts. The shared signing key model simplifies server-side changes compared to the per-device-key model.

### Signing Verification

The server verifies post signatures against the signing key looked up via the cached `SigningKeyDelegation` for `post.author` (the master pubkey). On key rotation, the server must accept the new signing key and reject the old one after the rotation timestamp.

### Server Must Handle New Gossip Variants

The server's gossip subscriber receives `LinkedDevices(LinkedDevicesAnnouncement)`. It should:

1. Verify the signature against `signing_pubkey`.
2. Cache the device list for routing purposes.
3. Optionally display device info on the web frontend.

### Server Must Handle Signed Deletes

The new `DeletePost` and `DeleteInteraction` gossip variants include a `signature` field. The server must verify the signature before processing the delete.

### Registration

Server registration uses the signing key for signing. The server must also store the `SigningKeyDelegation` to know which master pubkey the signing key belongs to. On key rotation, re-registration with the new signing key is required.

### Server-Side Sync Routing

When the server tries to sync from a user, it can connect to any of the user's devices (from the cached announcement). If the primary device is offline, the server can try secondary devices. This improves sync reliability -- the issue of sync timing out because a single device is unreachable is mitigated by having multiple endpoints.

```rust
// Server sync: try any available device for a user (identified by master pubkey)
async fn sync_from_user(&self, master_pubkey: &str) -> Result<(usize, usize)> {
    let devices = self.storage.get_peer_devices(master_pubkey).await?;
    for device in &devices {
        match Self::sync_from_node(&self.endpoint, &self.storage, master_pubkey, &device.node_id).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                tracing::warn!("[sync] device {} unreachable: {e}", &device.node_id[..8]);
                continue;
            }
        }
    }
    Err(anyhow::anyhow!("all devices unreachable for {}", &master_pubkey[..8]))
}
```

---

## Provenance Log (Optional)

An optional, supplementary append-only hash chain that records identity events. This does NOT replace the snapshot-based verification model -- the `SigningKeyDelegation` and `LinkedDevicesAnnouncement` snapshots remain the primary verification path. The provenance log adds auditability for peers who want it.

### Motivation

The snapshot model (latest versioned announcement wins, cached delegation) is simple and works well for verification. But it provides no history: a peer cannot distinguish "this user rotated their key yesterday" from "this is a brand-new identity that just appeared." A lightweight provenance log fills this gap without complicating the core verification path.

### Identity Event Chain

Each identity maintains a hash-chained log of identity-affecting events, all signed by the master key:

```rust
/// A single entry in the identity provenance log.
/// Append-only, hash-chained. Signed by the master key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityEvent {
    /// Monotonic sequence number (0 = genesis).
    pub seq: u64,
    /// SHA-256 hash of the previous event's canonical bytes. None for genesis.
    pub prev_hash: Option<String>,
    /// When this event occurred (Unix timestamp ms).
    pub timestamp: u64,
    /// What happened.
    pub payload: IdentityEventPayload,
    /// Ed25519 signature from the master key over the canonical bytes.
    pub master_sig: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IdentityEventPayload {
    /// seq=0 only. Establishes the identity.
    Genesis {
        master_pubkey: String,
    },
    /// A signing key was delegated (includes the full delegation).
    SigningKeyDelegated {
        signing_pubkey: String,
        key_index: u32,
    },
    /// A device was added to the identity.
    DeviceAdded {
        node_id: String,
        device_name: String,
    },
    /// A device was removed from the identity.
    DeviceRevoked {
        node_id: String,
        reason: String,
    },
    /// The signing key was rotated.
    SigningKeyRotated {
        old_signing_pubkey: String,
        new_signing_pubkey: String,
        new_key_index: u32,
    },
}
```

### Relationship to Snapshots

The provenance log and snapshot model coexist without dependency:

| Operation | Snapshot (primary) | Provenance log (supplementary) |
|-----------|-------------------|-------------------------------|
| Verify a post signature | Check cached `SigningKeyDelegation`, verify against `signing_pubkey` | Not consulted |
| Verify device membership | Check cached `LinkedDevicesAnnouncement` | Not consulted |
| Audit identity history | Not possible | Walk the chain from genesis |
| Key rotation | Accept new `SigningKeyDelegation`, discard old | Append `SigningKeyRotated` event |
| Device addition | Accept new `LinkedDevicesAnnouncement` (higher version) | Append `DeviceAdded` event |
| Device revocation | Accept new `LinkedDevicesAnnouncement` (device removed) | Append `DeviceRevoked` event |

The `LinkedDevicesAnnouncement.version` maps to the chain's latest `seq`. Peers can optionally verify that version N corresponds to event N in the log, tying snapshots to their history.

### No Fork Problem

Since only the master key can sign events and one device holds the master key, there is a natural single writer. Forks can only arise from key compromise, which is already catastrophic regardless of whether a provenance log exists. If a fork is detected (two events with the same `seq` but different `prev_hash`), the identity is considered compromised.

### Replication

The chain is tiny -- one entry per identity event (device add/remove, key rotation). A typical user accumulates maybe a dozen events over their entire usage lifetime. Replication is on-demand via a new protocol request:

```rust
// Add to PeerRequest
IdentityLogRequest {
    master_pubkey: String,
    /// Request events starting from this seq (inclusive). None = from genesis.
    since_seq: Option<u64>,
}

// Add to PeerResponse
IdentityLogResponse {
    events: Vec<IdentityEvent>,
}
```

### Gossip Integration

When a new identity event is created, broadcast it on `user_feed_topic(master_pubkey)` as a gossip message. Peers who maintain the log can append it. Peers who don't care simply ignore it -- they still have the snapshot for verification.

```rust
// Add variant to GossipMessage
IdentityEvent(IdentityEvent),
```

### Trust Signals

The provenance log enables trust-on-first-use (TOFU) improvements:

- **Chain length**: A long chain with consistent history is more trustworthy than a fresh identity.
- **Age**: The genesis event's timestamp establishes when the identity was created.
- **Rotation history**: "This user has rotated keys twice in 2 years" vs "this key appeared 5 minutes ago."
- **Device stability**: Frequent device churn might indicate suspicious behavior.

These signals are advisory -- they inform the UI but do not gate verification. A new identity with a single genesis event is still fully valid.

### Storage

```sql
CREATE TABLE identity_events (
    master_pubkey TEXT NOT NULL,
    seq INTEGER NOT NULL,
    prev_hash TEXT,
    timestamp INTEGER NOT NULL,
    payload TEXT NOT NULL,       -- JSON-serialized IdentityEventPayload
    master_sig TEXT NOT NULL,
    PRIMARY KEY (master_pubkey, seq)
);
```

### When to Create Events

Events are created on the device holding the master key:

- **First launch**: `Genesis` event (seq=0), then `SigningKeyDelegated` (seq=1), then `DeviceAdded` for the first device (seq=2).
- **Device pairing**: `DeviceAdded` event after successful pairing.
- **Device removal**: `DeviceRevoked` event when removing a device.
- **Key rotation**: `SigningKeyRotated` event when deriving a new signing key.

---

## Implementation Roadmap

### Phase 1: Key Hierarchy and Separation

- [x] Implement `derive_signing_key(master_secret, index)` using HKDF-SHA256
- [x] Define `SigningKeyDelegation` type with canonical signing bytes and sign/verify functions
- [x] Generate `master_key.key` on first launch, derive `signing_key.key` at index 0
- [x] Sign `SigningKeyDelegation` binding signing key to master key on startup
- [x] Update `AppState` to hold `master_secret_key_bytes`, `master_pubkey`, `signing_secret_key_bytes`, `signing_pubkey`, `signing_key_index`, and `transport_node_id`
- [x] Update all post/interaction/profile signing to use `signing_secret_key_bytes`
- [x] Update all verification to require delegation lookup (server has backward-compat fallback for transition)
- [x] Update DM handler to derive X25519 from signing key
- [x] Add `IdentityRequest` / `IdentityResponse` to `PeerRequest` / `PeerResponse` enums
- [x] Implement `IdentityRequest` handler: respond with master pubkey, delegation, device list, profile
- [x] Implement `resolve_transport_nodes(master_pubkey)` with local cache (+ optional server fallback)
- [x] Update gossip subscription to resolve transport NodeIds instead of parsing master pubkey as EndpointId
- [x] Update follow flow: connect to transport NodeId, send IdentityRequest, cache result, then subscribe
- [x] Update `SyncHandler` to accept sync requests where `author == master_pubkey`
- [x] Update user profile links/QR codes to include transport NodeId alongside master pubkey
- [x] Update server registration payload to include `master_pubkey`, `transport_node_id`, and `SigningKeyDelegation`
- [x] Add server endpoint (optional): `GET /api/v1/users/{master_pubkey}/devices`
- [x] Verify functionality works with separated keys (single device)

### Phase 2: Profile Signing and Delete Signing

- [x] Add `signature` field to `Profile` struct
- [x] Implement `profile_signing_bytes`, `sign_profile`, `verify_profile_signature`
- [x] Add `signature` field to `DeletePost` and `DeleteInteraction` gossip variants
- [x] Implement `sign_delete_post`, `verify_delete_post_signature`, `sign_delete_interaction`, `verify_delete_interaction_signature`
- [x] Update gossip message handlers to verify profile signatures and delete signatures
- [x] Update sync-received profile verification
- [x] Add `signature` and `updated_at` columns to profiles table
- [x] Update server ingestion to verify profile signatures and delete signatures

### Phase 3: Device Registry and Storage

- [x] Define `LinkedDevicesAnnouncement` and `DeviceEntry` types
- [x] Add `LinkedDevices` variant to `GossipMessage`
- [x] Add `DeviceAnnouncements` variant to `SyncFrame`
- [x] Add database migrations for device tables
- [x] Implement storage methods for device management and peer announcement caching
- [x] Handle incoming gossip announcements: validate and cache
- [x] For single-device users, publish a single-device announcement on startup
- [x] Add LWW state columns to `follows`, `mutes`, `blocks` tables
- [x] Update follow/mute/block operations to set state + timestamp instead of deleting rows

### Phase 4: Pairing Protocol

- [x] Add `LinkRequest`/`LinkBundle` variants to `PeerRequest`/`PeerResponse` and define wire types (`LinkQrPayload`, `LinkBundle`, `RatchetSessionExport`)
- [x] Handle `LinkRequest` in `PeerHandler`
- [x] Implement Noise IK + PSK handshake for pairing channel
- [x] Implement existing device side: generate QR, listen, send LinkBundle (including signing key, delegation, optionally master key, and ratchet sessions)
- [x] Implement new device side: scan QR, connect, receive LinkBundle, import data, save signing key (and master key if provided)
- [x] Add UI warning before transferring master key ("transfer full control")
- [x] Publish `LinkedDevicesAnnouncement` after successful pairing
- [x] Add Tauri commands: `start_device_link`, `link_with_device`, `cancel_device_link`
- [x] Build QR code display modal
- [x] Build QR scan / paste modal
- [x] Build `/settings/devices` management page

### Phase 5: Device Sync

- [x] Add `DeviceSync` variants to `PeerRequest`/`PeerResponse` and define sync types (`DeviceSyncVector`, `DeviceSyncAuth`)
- [x] Handle `DeviceSync` in `PeerHandler`
- [x] Implement challenge-response authentication (both sides prove signing key possession)
- [x] Implement sync vector generation from local state
- [x] Implement delta computation and streaming
- [x] Implement ratchet session sync (export/import with LWW by updated_at)
- [x] Implement DM message history sync (set union by message_id)
- [x] Extend outbox flush to try all recipient devices from `LinkedDevicesAnnouncement`
- [x] Implement post/interaction sync between own devices
- [x] Implement LWW-per-entry merge logic for follows, mutes, blocks
- [x] Add periodic sync task (60s interval)
- [x] Add Tauri command: `force_device_sync`

### Phase 6: Server Multi-Device Support

Already done (from earlier phases):
- [x] Update server ingestion to handle `LinkedDevices` gossip variant
- [x] Cache `SigningKeyDelegation` on the server (map master pubkey -> user pubkey)
- [x] Cache peer device announcements on the server
- [x] Handle signed deletes in server ingestion
- [x] Verify profile signatures in server ingestion

Remaining:
- [ ] Update server sync to try multiple devices when syncing from a user
  - `get_all_transport_node_ids()` storage method (check `peer_delegations.transport_node_ids_json` first, fall back to single node id)
  - `sync_from_peer_inner` tries each device in order, falls through on failure
- [ ] Update gossip subscription bootstrap to use all device NodeIds
- [ ] Handle `SigningKeyRotation` gossip variant (invalidate old signing key, accept new one) -- deferred to Phase 7

### Phase 6b: Server-Side DM Relay (opt-in)

Server acts as a dead-drop relay for E2E encrypted DM envelopes when direct P2P delivery fails. The server never sees plaintext -- only opaque encrypted envelopes. Requires explicit opt-in per server via `dm_relay` flag.

Server side:
- [ ] Add `stored_dms` table migration: `id TEXT PK, sender TEXT, recipient TEXT, envelope_json TEXT, stored_at INTEGER`
- [ ] Add indexes: `idx_stored_dms_recipient(recipient, stored_at)`, `idx_stored_dms_stored_at(stored_at)`
- [ ] Storage methods: `store_dm`, `get_pending_dms`, `ack_dms`, `count_pending_dms`, `prune_old_dms`
- [ ] API endpoints (all require signed requests verified against delegation):
  - `POST /api/v1/dm/store` -- sender stores an encrypted envelope for a recipient
  - `POST /api/v1/dm/pending` -- recipient fetches their pending DMs
  - `POST /api/v1/dm/ack` -- recipient acknowledges receipt (server deletes)
- [ ] Per-recipient limit (500 pending), envelope size limit (64KB)
- [ ] TTL cleanup: delete stored DMs older than 7 days (hourly pruning task)

Shared types (iroh-social-types):
- [ ] `DmStoreRequest`, `DmFetchRequest`, `DmAckRequest`, `PendingDm`, `DmFetchResponse`
- [ ] Sign/verify functions for each request type (canonical JSON signing bytes)

Client side:
- [ ] Add `dm_relay` boolean column to `servers` table (migration, default off)
- [ ] `set_server_dm_relay` Tauri command to toggle per server
- [ ] `get_dm_relay_server_urls()` storage method (only servers with `dm_relay = 1 AND registered_at IS NOT NULL`)
- [ ] `dm_relay.rs` module: `push_dm_to_servers()` and `poll_servers_for_dms()`
- [ ] DM outbox flush: when direct P2P delivery fails, fall back to server relay (push envelope to all opted-in relay servers)
- [ ] `handle_relayed_message()` on DmHandler to process server-relayed envelopes
- [ ] Periodic poll task: fetch pending DMs from relay servers, process, and ack
- [ ] UI: toggle DM relay per server in server settings

### Phase 7: Key Rotation

- [ ] Define `SigningKeyRotation` type with canonical signing bytes and verify function
- [ ] Implement signing key rotation on the device holding the master key
- [ ] Derive new signing key at index+1, sign `SigningKeyRotation` with master key
- [ ] Publish rotation via gossip, update delegation
- [ ] Re-pair remaining linked devices with new signing key
- [ ] Re-register with discovery servers
- [ ] Handle incoming `SigningKeyRotation` on peers: update cached delegation, reject old signing key after rotation timestamp
- [ ] Handle incoming `SigningKeyRotation` on server: same as peers
- [ ] DM session invalidation and re-establishment after rotation
- [ ] UI for key rotation (Settings -> Security -> "Rotate signing key")

### Phase 8: Revocation and Polish

- [ ] Implement trust-based device removal (remove from announcement, stop syncing)
- [ ] Paper key backup for master key (mnemonic or QR export)
- [ ] Master key recovery flow (import paper key on new device)
- [ ] Device rename UI
- [ ] Sync progress UI
- [ ] Error states and edge cases (pairing timeout, sync failure, network interruption)
- [ ] Handle "first device" vs "linked device" onboarding flow

### Phase 9: Provenance Log (Optional)

- [ ] Define `IdentityEvent` and `IdentityEventPayload` types with canonical signing bytes
- [ ] Add `identity_events` table migration
- [ ] Create genesis + delegation + device-added events on first launch
- [ ] Create `DeviceAdded` event during pairing
- [ ] Create `DeviceRevoked` event during device removal
- [ ] Create `SigningKeyRotated` event during key rotation
- [ ] Add `IdentityEvent` variant to `GossipMessage`
- [ ] Handle incoming `IdentityEvent` gossip: validate chain integrity (prev_hash, seq, master_sig), store
- [ ] Add `IdentityLogRequest`/`IdentityLogResponse` to `PeerRequest`/`PeerResponse`
- [ ] Implement on-demand chain replication (fetch missing events from peers)
- [ ] UI: identity age / chain length indicator on user profiles
- [ ] Server: store and serve provenance logs for registered users
