# DM Key Split: Separate DM Key from Signing Key

## Motivation

The DM X25519 keypair is currently derived from the signing key via ed25519-to-x25519 conversion. This couples DM sessions to signing key rotation -- rotating the signing key breaks all existing DM sessions. Splitting them allows independent rotation and cleaner crypto domain separation (signing keys sign, DH keys do DH).

## Key Hierarchy (After)

```
master_key.key (32 random bytes, primary device only, cold storage)
  |
  +-- signing key: HKDF(master, salt=signing_index, info="iroh-social/signing-key")
  |     Ed25519. Signs posts, interactions, profiles, follow requests,
  |     device announcements, device sync challenges.
  |     Never touches DH.
  |
  +-- DM key: HKDF(master, salt=dm_index, info="iroh-social/dm-key")
  |     Raw 32 bytes, clamped to X25519 private key.
  |     Used for Noise IK handshake and Double Ratchet seeding.
  |     Shared across linked devices via LinkBundleData.
  |     Independent rotation from signing key.
  |
  +-- transport key: HKDF(master, salt=device_index, info="iroh-social/transport-key")
  |     Ed25519, per-device. Unchanged.
  |
  +-- ratchet storage key: HKDF(dm_secret, info="iroh-social-ratchet-storage-v1")
        Symmetric ChaCha20Poly1305. Derived from DM key (not master),
        so secondary devices without the master key can still encrypt/decrypt.
```

## Device Model

Primary device (has master_key.key):
- Derives all keys from master
- Signs delegations with master key
- Distributes signing_secret, dm_secret, transport_secret to linked devices

Secondary device (NO master key):
- Receives signing_secret, dm_secret, transport_secret via LinkBundleData
- Persists each to disk as raw 32-byte files
- Cannot derive new keys or sign delegations
- Can fully participate in DM conversations

## Changes by File

### 1. crates/iroh-social-types/src/delegation.rs

- Add `derive_dm_key(master_secret: &[u8; 32], index: u32) -> [u8; 32]`
  - HKDF-SHA256, salt=index.to_be_bytes(), info="iroh-social/dm-key"
- Add fields to `SigningKeyDelegation`:
  - `dm_pubkey: String` (hex-encoded X25519 public key, 64 chars)
  - `dm_key_index: u32`
- Update `delegation_signing_bytes` to include dm_pubkey and dm_key_index
- Update `sign_delegation` signature: takes dm_pubkey + dm_key_index
- Update `SigningKeyRotation` fields if DM key can change during rotation
- Update `verify_delegation` accordingly
- Update all tests

### 2. crates/iroh-social-types/src/dm.rs

- `DmHandshake::Init.sender`: now carries hex DM pubkey (not signing pubkey)
- `EncryptedEnvelope.sender`: now carries hex DM pubkey

### 3. crates/iroh-social-types/src/types.rs

- `LinkBundleData`: add `dm_secret_key: String` (base64, 32 bytes)

### 4. src-tauri/src/crypto.rs

- Add `clamp_to_x25519(secret: &[u8; 32]) -> [u8; 32]` -- RFC 7748 clamping
- Add `x25519_keypair_from_raw(secret: &[u8; 32]) -> ([u8; 32], [u8; 32])` -- clamp + derive public
- Keep ed25519_secret_to_x25519 and ed25519_public_to_x25519 (still used by device pairing)

### 5. src-tauri/src/dm.rs

- `DmHandler::new` signature changes:
  - Takes `dm_secret: [u8; 32]` instead of `signing_secret: [u8; 32]`
  - Takes `dm_pubkey_str: String` instead of `signing_pubkey_str: String`
  - Removes `master_secret: [u8; 32]` parameter
  - X25519 derived from dm_secret via clamp (no ed25519 conversion)
  - Ratchet storage key derived from dm_secret (not master)
- Replace `my_signing_pubkey_str` with `my_dm_pubkey_str` everywhere
- `get_or_establish_session`: look up peer DM pubkey via `get_peer_dm_pubkey()`
  instead of `get_peer_signing_pubkey()`
- Handshake: peer's DM pubkey IS already X25519 -- no ed25519_public_to_x25519 needed.
  Just hex-decode to get 32 bytes and use directly.
- `handle_encrypted_message`: reverse lookup via `get_master_pubkey_for_dm_pubkey()`
- Sessions keyed by peer's DM pubkey (not signing pubkey)
- Remove plaintext migration fallback in open_ratchet_state

### 6. src-tauri/src/setup.rs

- Load or derive DM key:
  - Primary: `derive_dm_key(master_secret, dm_key_index)`
  - Secondary: load from `dm_key.key` file (received via LinkBundleData)
- Persist `dm_key_index` to disk (like signing_key_index)
- Compute DM X25519 public key, hex-encode
- Pass dm_secret, dm_pubkey to DmHandler::new (no master_secret)
- Include dm_pubkey and dm_key_index in sign_delegation call

### 7. src-tauri/src/state.rs

- AppState: add `dm_key_index: u32`, `dm_pubkey: String`
- Update signing_secret_key_bytes doc comment (no longer "signs content and DMs")

### 8. src-tauri/src/storage/peer_delegations.rs

- Add `dm_pubkey TEXT` column to peer_delegations
- `cache_peer_identity`: store dm_pubkey from delegation
- Add `get_peer_dm_pubkey(master_pubkey) -> Result<Option<String>>`
- Add `get_master_pubkey_for_dm_pubkey(dm_pubkey) -> Option<String>`
- Remove `get_master_pubkey_for_signing_pubkey` (no longer needed for DM)

### 9. src-tauri/migrations/002_peer_delegations.sql

- Add `dm_pubkey TEXT NOT NULL DEFAULT ''`

### 10. src-tauri/src/commands/devices.rs

- `link_with_device`: decode and persist dm_secret_key from bundle to dm_key.key
- `export_link_bundle` / LinkBundleData: include dm_secret_key

### 11. src-tauri/src/storage/linked_devices.rs

- `export_link_bundle`: add dm_secret_key_bytes param, encode to base64
- `import_link_bundle`: no change needed (ratchet sessions imported as-is)

## Wire Protocol

DM pubkey format on wire: lowercase hex-encoded X25519 public key (64 characters).
This replaces signing pubkey in DmHandshake::Init.sender and EncryptedEnvelope.sender.

## DM Key Rotation

Bumping dm_key_index:
1. Primary device derives new DM key from master
2. Publishes new delegation with new dm_pubkey (signed by master)
3. All existing ratchet sessions become invalid (peers can't complete Noise with old key)
4. Peers re-handshake lazily on next message attempt
5. Linked devices receive new dm_secret via device sync

Signing key rotation no longer affects DM at all.
