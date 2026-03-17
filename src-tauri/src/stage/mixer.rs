use super::auth::HostAuthState;
use super::fanout::Fanout;
use crate::audio::{FRAME_SIZE, OpusEncoder, SAMPLES_PER_FRAME};

/// Per-speaker sample buffer cap: 5 frames (~100ms). Excess oldest samples
/// are dropped to prevent a rogue or slow speaker from causing unbounded growth.
const MAX_SPEAKER_BUFFER: usize = FRAME_SIZE * 5;
use crate::error::AppError;
use iroh::SecretKey;
use iroh_social_types::short_id;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// ---- Command enum -------------------------------------------------------

#[allow(dead_code)]
pub enum MixerCommand {
    /// New speaker joined — register their decoded PCM channel with the mixer.
    AddSpeaker {
        pubkey: String,
        pcm_rx: mpsc::Receiver<Vec<f32>>,
    },
    /// Speaker left or was demoted — remove their input from the mix.
    RemoveSpeaker(String),
    /// Query current per-speaker RMS levels (for SpeakerActivity gossip).
    GetLevels {
        reply: oneshot::Sender<HashMap<String, f32>>,
    },
    Shutdown,
}

// ---- Handle -------------------------------------------------------------

/// Cheap-to-clone handle to the HostMixer actor.
#[derive(Clone)]
pub struct MixerHandle {
    cmd_tx: mpsc::Sender<MixerCommand>,
}

#[allow(dead_code)]
impl MixerHandle {
    pub async fn add_speaker(
        &self,
        pubkey: String,
        pcm_rx: mpsc::Receiver<Vec<f32>>,
    ) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::AddSpeaker { pubkey, pcm_rx })
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
    }

    pub async fn remove_speaker(&self, pubkey: String) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::RemoveSpeaker(pubkey))
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
    }

    pub async fn get_levels(&self) -> HashMap<String, f32> {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(MixerCommand::GetLevels { reply: tx })
            .await
            .is_err()
        {
            return HashMap::new();
        }
        rx.await.unwrap_or_default()
    }
}

// ---- Actor --------------------------------------------------------------

/// Spawn the HostMixer Tokio actor.
///
/// Returns `(MixerHandle, Arc<Fanout>)`:
/// - `MixerHandle`: add/remove speakers, query RMS levels
/// - `Arc<Fanout>`: add QUIC-stream subscribers (from StageHandler::accept)
///
/// The actor owns the encoder, auth state, and all per-speaker buffers exclusively.
/// Speaker recv tasks deliver decoded PCM via per-speaker `mpsc::Sender<Vec<f32>>`.
pub fn spawn_mixer(
    signing_key: SecretKey,
    cancel: CancellationToken,
) -> Result<(MixerHandle, Arc<Fanout>), AppError> {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let fanout = Arc::new(Fanout::new());
    let actor_fanout = fanout.clone();

    tokio::spawn(run_mixer(cmd_rx, actor_fanout, signing_key, cancel));

    Ok((MixerHandle { cmd_tx }, fanout))
}

/// The mixer actor loop. Runs until `cancel` fires or all senders are dropped.
async fn run_mixer(
    mut cmd_rx: mpsc::Receiver<MixerCommand>,
    fanout: Arc<Fanout>,
    signing_key: SecretKey,
    cancel: CancellationToken,
) {
    let mut encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("[mixer] failed to create Opus encoder: {e}");
            return;
        }
    };

    let mut auth = HostAuthState::new(signing_key);

    // pubkey -> (sample buffer, most-recent RMS)
    let mut buffers: HashMap<String, (Vec<f32>, f32)> = HashMap::new();
    // Separate vec for the PCM receivers so we can drain them each tick
    let mut speaker_channels: Vec<(String, mpsc::Receiver<Vec<f32>>)> = Vec::new();

    let mut seq: u32 = 0;
    let mut timestamp: u32 = 0;
    let mut frame_count: u32 = 0;

    // Mix tick: one Opus frame = 20ms
    let mut mix_interval = tokio::time::interval(std::time::Duration::from_millis(20));
    mix_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(MixerCommand::AddSpeaker { pubkey, pcm_rx }) => {
                        log::info!("[mixer] adding speaker {}", short_id(&pubkey));
                        buffers.insert(pubkey.clone(), (Vec::new(), 0.0));
                        speaker_channels.push((pubkey, pcm_rx));
                    }
                    Some(MixerCommand::RemoveSpeaker(pubkey)) => {
                        log::info!("[mixer] removing speaker {}", short_id(&pubkey));
                        buffers.remove(&pubkey);
                        speaker_channels.retain(|(pk, _)| pk != &pubkey);
                    }
                    Some(MixerCommand::GetLevels { reply }) => {
                        let levels: HashMap<String, f32> = buffers
                            .iter()
                            .map(|(pk, (_, rms))| (pk.clone(), *rms))
                            .collect();
                        let _ = reply.send(levels);
                    }
                    Some(MixerCommand::Shutdown) | None => break,
                }
            }

            _ = mix_interval.tick() => {
                // Drain all available PCM from each speaker's channel into their buffer.
                // Collect disconnected channels for removal after the loop.
                let mut dead: Vec<String> = Vec::new();
                for (pubkey, rx) in &mut speaker_channels {
                    loop {
                        match rx.try_recv() {
                            Ok(samples) => {
                                if let Some((buf, _)) = buffers.get_mut(pubkey) {
                                    buf.extend_from_slice(&samples);
                                    if buf.len() > MAX_SPEAKER_BUFFER {
                                        let excess = buf.len() - MAX_SPEAKER_BUFFER;
                                        log::warn!(
                                            "[mixer] speaker {} buffer overflow, dropping {excess} oldest samples",
                                            short_id(pubkey)
                                        );
                                        buf.drain(..excess);
                                    }
                                }
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                log::debug!("[mixer] speaker {} channel closed, removing", short_id(pubkey));
                                dead.push(pubkey.clone());
                                break;
                            }
                        }
                    }
                }
                for pubkey in dead {
                    buffers.remove(&pubkey);
                    speaker_channels.retain(|(pk, _)| pk != &pubkey);
                }

                // Mix: sum one frame worth of samples from all speakers, clamp
                let mut mix = vec![0.0f32; FRAME_SIZE];
                for (_, (buf, rms)) in buffers.iter_mut() {
                    let available = buf.len().min(FRAME_SIZE);
                    let sum_sq: f32 = buf[..available].iter().map(|s| s * s).sum();
                    *rms = (sum_sq / available.max(1) as f32).sqrt();

                    for (i, s) in buf.drain(..available).enumerate() {
                        mix[i] = (mix[i] + s).clamp(-1.0, 1.0);
                    }
                }

                // Encode, authenticate, and distribute
                let packets = encoder.push_samples(&mix);
                for packet in packets {
                    let (tag, wire_payload) = auth.process(&packet);
                    fanout.send_frame(seq, timestamp, tag, wire_payload);
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(SAMPLES_PER_FRAME as u32);
                    frame_count = frame_count.wrapping_add(1);
                }
            }
        }
    }

    log::info!("[mixer] actor stopped ({frame_count} frames encoded)");
}
