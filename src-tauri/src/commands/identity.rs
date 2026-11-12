use crate::error::CmdResult;
use crate::setup::{IdentityKeys, SEED_BACKED_UP_FILE, SEED_CONFIRM_FILE};
use crate::state::AppState;
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

/// True if `get_seed_phrase` must ask for native confirmation: the backup marker file or
/// the marker written by `wipe_all_data` exists (or its presence cannot be determined),
/// or the local user has a profile.
async fn seed_phrase_requires_confirmation(state: &AppState) -> CmdResult<bool> {
    let data_dir = state.app_handle.path().app_data_dir()?;
    if [SEED_BACKED_UP_FILE, SEED_CONFIRM_FILE]
        .iter()
        .any(|name| data_dir.join(name).try_exists().unwrap_or(true))
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
    let mut master_secret_key_bytes = state.identity.read().await.master_secret_key_bytes;
    let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes);
    master_secret_key_bytes.zeroize();
    Ok(mnemonic?.to_string())
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
    let mut master_secret_key_bytes = state.identity.read().await.master_secret_key_bytes;
    let mnemonic = bip39::Mnemonic::from_entropy(&master_secret_key_bytes);
    master_secret_key_bytes.zeroize();
    let mnemonic = mnemonic?;
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
/// and media are deleted, a new random transport key is written, the master key file is
/// written last, and the network stack is rebuilt in place. The caller reloads the webview after
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
         The keys, posts, messages, follows, media and settings stored on this device are \
         deleted. This cannot be undone.\n\n\
         This device then announces itself as the only device of the identity, so others \
         stop reaching your other devices. To add a device while you still have one, link \
         it instead.",
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
            transport_secret_key: crate::setup::random_key()?,
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
