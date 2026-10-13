use crate::error::CmdResult;
use crate::setup::{IdentityKeys, SEED_BACKED_UP_FILE};
use crate::state::AppState;
use iroh::SecretKey;
use proscenium_types::{
    RegistrationPayload, RegistrationRequest, Visibility, derive_signing_key, now_millis,
    sign_registration, sign_rotation,
};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::{Manager, State};
use zeroize::Zeroize;

/// Number of word positions `verify_seed_phrase_words` requires per call.
const MIN_VERIFY_WORDS: usize = 3;
/// Failed `verify_seed_phrase_words` calls allowed per process run.
const MAX_FAILED_VERIFICATIONS: u32 = 10;
/// Failed `verify_seed_phrase_words` calls since the process started.
static FAILED_VERIFICATIONS: AtomicU32 = AtomicU32::new(0);

/// True if `get_seed_phrase` must ask for native confirmation: the backup marker file
/// exists (or its presence cannot be determined), or the local user has a profile.
async fn seed_phrase_requires_confirmation(state: &AppState) -> CmdResult<bool> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    if data_dir
        .join(SEED_BACKED_UP_FILE)
        .try_exists()
        .unwrap_or(true)
    {
        return Ok(true);
    }
    let master_pubkey = state.identity.read().await.master_pubkey.clone();
    Ok(state.storage.get_profile(&master_pubkey).await?.is_some())
}

/// Return the recovery phrase (the BIP39 mnemonic of the master key).
///
/// During first-run setup (no backup marker file and no profile) the phrase is returned
/// directly. Afterwards a native dialog must be confirmed; the error `"cancelled"` is
/// returned if it is declined.
#[tauri::command]
pub async fn get_seed_phrase(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    if seed_phrase_requires_confirmation(&state).await? {
        let confirmed = crate::confirm::confirm_native(
            &state.app_handle,
            "Show recovery phrase",
            "Show the recovery phrase of this identity? Anyone who reads the phrase can take over the identity.",
            "Show phrase",
        )
        .await;
        if !confirmed {
            return Err("cancelled".into());
        }
    }
    let master_secret_key_bytes = state.identity.read().await.master_secret_key_bytes;
    let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes)?;
    Ok(mnemonic.to_string())
}

#[tauri::command]
pub async fn is_seed_phrase_backed_up(state: State<'_, Arc<AppState>>) -> CmdResult<bool> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    Ok(data_dir.join(SEED_BACKED_UP_FILE).exists())
}

#[tauri::command]
pub async fn mark_seed_phrase_backed_up(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    tokio::fs::write(data_dir.join(SEED_BACKED_UP_FILE), b"1").await?;
    Ok(())
}

/// Rotate the signing key to the next derivation index.
///
/// The new index is written to disk and the in-memory identity is replaced first. The
/// rotation, the device announcement and the profile (signed with the new key) are
/// broadcast afterwards; a failure of those steps is logged and does not undo the
/// rotation. Registration with discovery servers is repeated in a background task.
#[tauri::command]
pub async fn rotate_signing_key(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    if state.replacing_identity.load(Ordering::SeqCst) {
        return Err("the identity of this device is being replaced".into());
    }
    let (old_index, master_secret_key_bytes, old_signing_pub) = {
        let id = state.identity.read().await;
        (
            id.signing_key_index,
            id.master_secret_key_bytes,
            id.signing_key.public(),
        )
    };
    let new_index = old_index
        .checked_add(1)
        .ok_or("signing key index cannot be raised further")?;

    let new_delegation =
        crate::setup::raise_signing_key_index(&state.app_handle, &state.identity, new_index)
            .await?;
    log::info!("[rotate] signing key rotated from index {old_index} to {new_index}");

    let master_secret = SecretKey::from_bytes(&master_secret_key_bytes);
    let new_signing_pub =
        SecretKey::from_bytes(&derive_signing_key(&master_secret_key_bytes, new_index)).public();
    let rotation = sign_rotation(
        &master_secret,
        &old_signing_pub,
        &new_signing_pub,
        new_index,
        new_delegation.issued_at,
        new_delegation,
    );

    let gossip = state.gossip();
    if let Err(e) = gossip.broadcast_signing_key_rotation(&rotation).await {
        log::warn!("[rotate] failed to broadcast rotation: {e}");
    }

    // Signs the stored profile with the new key and saves it.
    match crate::announcement::build_own_identity_response(&state.storage, &state.identity).await {
        Ok(response) => {
            if let Err(e) = gossip
                .broadcast_linked_devices(&response.announcement)
                .await
            {
                log::warn!("[rotate] failed to broadcast device announcement: {e}");
            }
            if let Some(profile) = &response.profile
                && let Err(e) = gossip.broadcast_profile(profile).await
            {
                log::warn!("[rotate] failed to broadcast profile: {e}");
            }
        }
        Err(e) => log::warn!("[rotate] failed to build device announcement: {e}"),
    }

    let state_arc: Arc<AppState> = Arc::clone(&state);
    tokio::spawn(async move {
        reregister_with_servers(&state_arc).await;
    });

    Ok(format!("Signing key rotated to index {new_index}."))
}

/// Send a registration signed with the current signing key to every discovery server
/// the user is registered with, so each server stores the current delegation.
/// Failures are logged.
async fn reregister_with_servers(state: &AppState) {
    let servers = match state.storage.get_registered_servers().await {
        Ok(servers) => servers,
        Err(e) => {
            log::warn!("[rotate] failed to load registered servers: {e}");
            return;
        }
    };
    let (master_pubkey, transport_node_id, signing_key, delegation) = {
        let id = state.identity.read().await;
        (
            id.master_pubkey.clone(),
            id.transport_node_id.clone(),
            id.signing_key.clone(),
            id.delegation.clone(),
        )
    };

    for server in &servers {
        let Ok(visibility) = server.visibility.parse::<Visibility>() else {
            log::warn!(
                "[rotate] not registering with {}: stored visibility is not valid",
                server.url
            );
            continue;
        };
        let payload = RegistrationPayload {
            master_pubkey: master_pubkey.clone(),
            transport_node_id: transport_node_id.clone(),
            server_url: server.url.clone(),
            timestamp: now_millis(),
            visibility,
            action: None,
        };
        let signature = sign_registration(&payload, &signing_key);
        let request = RegistrationRequest {
            master_pubkey: payload.master_pubkey,
            transport_node_id: payload.transport_node_id,
            server_url: payload.server_url,
            timestamp: payload.timestamp,
            visibility,
            action: None,
            signature,
            delegation: delegation.clone(),
        };
        match state
            .http_client
            .post(format!("{}/api/v1/register", server.url))
            .json(&request)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                log::info!("[rotate] registered again with {}", server.url);
            }
            Ok(resp) => {
                log::warn!(
                    "[rotate] registration with {} failed: {}",
                    server.url,
                    resp.status()
                );
            }
            Err(e) => {
                log::warn!("[rotate] registration with {} failed: {e}", server.url);
            }
        }
    }
}

/// Compare words of the recovery phrase with `checks` (pairs of zero-based word position
/// and word).
///
/// Returns `false` unless `checks` names at least `MIN_VERIFY_WORDS` distinct positions,
/// all inside the phrase, and every word matches. After `MAX_FAILED_VERIFICATIONS` calls
/// that returned `false`, further calls return an error until the app is restarted, so
/// the command cannot be used to find the phrase word by word.
#[tauri::command]
pub async fn verify_seed_phrase_words(
    state: State<'_, Arc<AppState>>,
    checks: Vec<(usize, String)>,
) -> CmdResult<bool> {
    if FAILED_VERIFICATIONS.load(Ordering::Relaxed) >= MAX_FAILED_VERIFICATIONS {
        return Err("Too many failed attempts. Restart the app to verify the phrase again.".into());
    }
    let master_secret_key_bytes = state.identity.read().await.master_secret_key_bytes;
    let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes)?;
    let words: Vec<&str> = mnemonic.words().collect();
    let verified = seed_words_match(&words, &checks);
    if !verified {
        FAILED_VERIFICATIONS.fetch_add(1, Ordering::Relaxed);
    }
    Ok(verified)
}

/// True if `checks` names at least `MIN_VERIFY_WORDS` distinct positions, every position
/// is inside `words`, and every pair holds the word at its position.
fn seed_words_match(words: &[&str], checks: &[(usize, String)]) -> bool {
    let positions: HashSet<usize> = checks.iter().map(|(position, _)| *position).collect();
    positions.len() >= MIN_VERIFY_WORDS
        && checks
            .iter()
            .all(|(position, word)| words.get(*position).is_some_and(|w| *w == word.as_str()))
}

/// Recover identity from a BIP39 recovery phrase.
///
/// Validates the phrase, asks for confirmation in a native dialog (error `"cancelled"` if
/// declined), then replaces the local identity: the network stack is stopped, all rows
/// are deleted, the derivation indexes are reset to 0, the master key file is written
/// last, and the network stack is rebuilt in place. The caller reloads the webview after
/// `Ok`. An error after the network stop states that a restart is required.
#[tauri::command]
pub async fn recover_from_seed_phrase(
    state: State<'_, Arc<AppState>>,
    phrase: String,
) -> CmdResult<()> {
    let mut phrase = phrase;
    let parsed = bip39::Mnemonic::from_str(phrase.trim());
    phrase.zeroize();
    let mnemonic = parsed.map_err(|e| format!("Invalid recovery phrase: {e}"))?;

    let mut entropy = mnemonic.to_entropy();
    let master: Result<[u8; 32], _> = entropy.as_slice().try_into();
    entropy.zeroize();
    let master_secret_key =
        master.map_err(|_| "Recovery phrase did not produce 32 bytes of entropy")?;

    if state.identity.read().await.master_secret_key_bytes == master_secret_key {
        return Err("This device already uses the identity of that recovery phrase.".into());
    }

    let confirmed = crate::confirm::confirm_native(
        &state.app_handle,
        "Recover identity",
        "Replace the identity on this device with the identity of the recovery phrase?\n\n\
         The keys, posts, messages, follows and settings stored on this device are deleted. \
         This cannot be undone.",
        "Replace identity",
    )
    .await;
    if !confirmed {
        return Err("cancelled".into());
    }

    let state_arc: Arc<AppState> = Arc::clone(&state);
    crate::setup::replace_identity(
        &state_arc,
        IdentityKeys {
            master_secret_key,
            device_index: 0,
            signing_key_index: 0,
            dm_key_index: 0,
        },
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(pairs: &[(usize, &str)]) -> Vec<(usize, String)> {
        pairs
            .iter()
            .map(|(position, word)| (*position, word.to_string()))
            .collect()
    }

    #[test]
    fn seed_word_checks_need_three_distinct_matching_positions() {
        let words = ["alpha", "bravo", "charlie", "delta"];

        assert!(seed_words_match(
            &words,
            &checks(&[(0, "alpha"), (2, "charlie"), (3, "delta")])
        ));
        // Empty, too few, or repeated positions.
        assert!(!seed_words_match(&words, &[]));
        assert!(!seed_words_match(
            &words,
            &checks(&[(0, "alpha"), (1, "bravo")])
        ));
        assert!(!seed_words_match(
            &words,
            &checks(&[(0, "alpha"), (0, "alpha"), (0, "alpha")])
        ));
        // One wrong word, or a position outside the phrase.
        assert!(!seed_words_match(
            &words,
            &checks(&[(0, "alpha"), (1, "bravo"), (2, "delta")])
        ));
        assert!(!seed_words_match(
            &words,
            &checks(&[(0, "alpha"), (1, "bravo"), (4, "echo")])
        ));
    }
}
