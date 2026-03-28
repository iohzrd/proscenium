use crate::audio::{SharedCapture, SharedPlayback};
use crate::error::AppError;
use crate::preferences;
use proscenium_types::short_id;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

/// Active call state tracked by the handler.
pub(crate) struct ActiveCall {
    pub(super) call_id: String,
    pub(super) peer_pubkey: String,
    pub(super) cancel: CancellationToken,
    pub(super) muted: Arc<AtomicBool>,
    /// Shared handle to the active audio capture (if audio session is running).
    pub(super) capture: SharedCapture,
    pub(super) playback: SharedPlayback,
}

impl super::CallHandler {
    /// Load audio device preferences from storage.
    /// Returns (input_device, output_device) where None means "use default".
    pub(super) async fn audio_device_prefs(&self) -> (Option<String>, Option<String>) {
        let input = self
            .storage
            .get_preference(preferences::AUDIO_INPUT_DEVICE)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        let output = self
            .storage
            .get_preference(preferences::AUDIO_OUTPUT_DEVICE)
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        (input, output)
    }

    /// Switch the input (microphone) device mid-call. Saves the preference
    /// and immediately rebuilds the capture stream on the new device.
    pub async fn switch_input_device(&self, name: &str) -> Result<(), AppError> {
        let pref_value = if name.is_empty() { "" } else { name };
        self.storage
            .set_preference(preferences::AUDIO_INPUT_DEVICE, pref_value)
            .await?;

        let device_name = if name.is_empty() { None } else { Some(name) };

        let lock = self.active_call.lock().await;
        if let Some(call) = &*lock {
            let mut cap = call.capture.lock().unwrap();
            if let Some(capture) = cap.as_mut() {
                capture
                    .switch_device(device_name)
                    .map_err(|e| AppError::Other(format!("failed to switch input device: {e}")))?;
            }
        }
        Ok(())
    }

    /// Switch the output (speaker) device mid-call. Saves the preference
    /// and immediately rebuilds the playback stream on the new device.
    pub async fn switch_output_device(&self, name: &str) -> Result<(), AppError> {
        let pref_value = if name.is_empty() { "" } else { name };
        self.storage
            .set_preference(preferences::AUDIO_OUTPUT_DEVICE, pref_value)
            .await?;

        let device_name = if name.is_empty() { None } else { Some(name) };

        let lock = self.active_call.lock().await;
        if let Some(call) = &*lock {
            let mut pb = call.playback.lock().unwrap();
            if let Some(playback) = pb.as_mut() {
                playback
                    .switch_device(device_name)
                    .map_err(|e| AppError::Other(format!("failed to switch output device: {e}")))?;
            }
        }
        Ok(())
    }

    /// Rebuild the live cpal capture and playback streams on the default
    /// device. Called after Android audio route changes -- Android kills
    /// the old AAudio streams when the communication device changes, so
    /// we must build new ones.
    pub async fn rebuild_streams(&self) {
        let lock = self.active_call.lock().await;
        let Some(call) = &*lock else { return };

        if let Some(capture) = call.capture.lock().unwrap().as_mut()
            && let Err(e) = capture.switch_device(None)
        {
            log::warn!("[call] failed to rebuild capture stream: {e}");
        }
        if let Some(playback) = call.playback.lock().unwrap().as_mut()
            && let Err(e) = playback.switch_device(None)
        {
            log::warn!("[call] failed to rebuild playback stream: {e}");
        }
        log::info!("[call] rebuilt audio streams after route change");
    }

    /// Toggle mute on the current call. Returns the new mute state.
    pub async fn toggle_mute(&self) -> Result<bool, AppError> {
        let lock = self.active_call.lock().await;
        let call = lock
            .as_ref()
            .ok_or(AppError::Other("no active call".into()))?;
        let was_muted = call.muted.load(Ordering::Relaxed);
        call.muted.store(!was_muted, Ordering::Relaxed);
        log::info!(
            "[call] {} call {}",
            if was_muted { "unmuted" } else { "muted" },
            short_id(&call.call_id)
        );
        Ok(!was_muted)
    }
}
