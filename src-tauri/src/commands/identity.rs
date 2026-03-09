use crate::state::AppState;
use std::sync::Arc;
use tauri::{Manager, State};

/// Return the master key as a BIP39 24-word mnemonic.
#[tauri::command]
pub async fn get_seed_phrase(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let mnemonic = bip39::Mnemonic::from_entropy(&state.master_secret_key_bytes)
        .map_err(|e| format!("failed to generate mnemonic: {e}"))?;
    Ok(mnemonic.to_string())
}

/// Check whether the user has backed up their seed phrase.
#[tauri::command]
pub async fn is_seed_phrase_backed_up(app_handle: tauri::AppHandle) -> Result<bool, String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve data dir: {e}"))?;
    Ok(data_dir.join(".seed_backed_up").exists())
}

/// Mark the seed phrase as backed up by creating a marker file.
#[tauri::command]
pub async fn mark_seed_phrase_backed_up(app_handle: tauri::AppHandle) -> Result<(), String> {
    let data_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve data dir: {e}"))?;
    std::fs::write(data_dir.join(".seed_backed_up"), b"1")
        .map_err(|e| format!("failed to write marker: {e}"))
}

/// Verify that specific words from the seed phrase are correct.
/// Takes a list of (index, word) pairs where index is 0-based.
#[tauri::command]
pub async fn verify_seed_phrase_words(
    state: State<'_, Arc<AppState>>,
    checks: Vec<(usize, String)>,
) -> Result<bool, String> {
    let mnemonic = bip39::Mnemonic::from_entropy(&state.master_secret_key_bytes)
        .map_err(|e| format!("failed to generate mnemonic: {e}"))?;
    let words: Vec<&str> = mnemonic.words().collect();
    for (idx, word) in &checks {
        if *idx >= words.len() {
            return Ok(false);
        }
        if words[*idx] != word.as_str() {
            return Ok(false);
        }
    }
    Ok(true)
}
