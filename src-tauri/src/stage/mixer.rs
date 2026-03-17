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
    /// Mark a speaker as the local host and provide a direct PCM channel for
    /// mix-minus playback. The host receives all other speakers except themselves.
    SetHostSpeaker {
        pubkey: String,
        pcm_tx: mpsc::Sender<Vec<f32>>,
    },
    /// Register a per-speaker return channel for personalized mix-minus Opus output.
    /// The mixer encodes mix-minus-self and sends raw Opus packets to this channel.
    AddReturnChannel {
        pubkey: String,
        opus_tx: mpsc::Sender<Vec<u8>>,
    },
    /// Remove a speaker's return channel (speaker left or was demoted).
    RemoveReturnChannel(String),
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

    pub async fn set_host_speaker(
        &self,
        pubkey: String,
        pcm_tx: mpsc::Sender<Vec<f32>>,
    ) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::SetHostSpeaker { pubkey, pcm_tx })
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
    }

    pub async fn add_return_channel(
        &self,
        pubkey: String,
        opus_tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::AddReturnChannel { pubkey, opus_tx })
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
    }

    pub async fn remove_return_channel(&self, pubkey: String) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::RemoveReturnChannel(pubkey))
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
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
    // Host speaker pubkey + direct PCM channel for mix-minus local playback
    let mut host_speaker: Option<(String, mpsc::Sender<Vec<f32>>)> = None;
    // Per-remote-speaker return channels: pubkey -> (opus sender, per-speaker encoder)
    let mut return_channels: HashMap<String, (mpsc::Sender<Vec<u8>>, OpusEncoder)> = HashMap::new();

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
                    Some(MixerCommand::SetHostSpeaker { pubkey, pcm_tx }) => {
                        host_speaker = Some((pubkey, pcm_tx));
                    }
                    Some(MixerCommand::AddReturnChannel { pubkey, opus_tx }) => {
                        match OpusEncoder::new() {
                            Ok(enc) => {
                                return_channels.insert(pubkey, (opus_tx, enc));
                            }
                            Err(e) => {
                                log::error!(
                                    "[mixer] failed to create return encoder for {}: {e}",
                                    short_id(&pubkey)
                                );
                            }
                        }
                    }
                    Some(MixerCommand::RemoveReturnChannel(pubkey)) => {
                        return_channels.remove(&pubkey);
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

                // Collect per-speaker contributions (FRAME_SIZE samples each) and
                // compute the unclamped total sum for mix-minus arithmetic.
                let mut contributions: HashMap<String, Vec<f32>> = HashMap::new();
                let mut total = vec![0.0f32; FRAME_SIZE];
                for (pubkey, (buf, rms)) in buffers.iter_mut() {
                    let available = buf.len().min(FRAME_SIZE);
                    let sum_sq: f32 = buf[..available].iter().map(|s| s * s).sum();
                    *rms = (sum_sq / available.max(1) as f32).sqrt();

                    let mut contrib = vec![0.0f32; FRAME_SIZE];
                    for (i, s) in buf.drain(..available).enumerate() {
                        contrib[i] = s;
                        total[i] += s;
                    }
                    contributions.insert(pubkey.clone(), contrib);
                }

                // Full mix (clamped) → encode once → fanout (all listeners and relays).
                let full_mix: Vec<f32> = total.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
                let packets = encoder.push_samples(&full_mix);
                for packet in &packets {
                    let (tag, wire_payload) = auth.process(packet);
                    fanout.send_frame(seq, timestamp, tag, wire_payload);
                    seq = seq.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(SAMPLES_PER_FRAME as u32);
                    frame_count = frame_count.wrapping_add(1);
                }

                // Mix-minus for host local playback: PCM of all speakers except the host.
                if let Some((ref host_pk, ref pcm_tx)) = host_speaker {
                    let host_contrib = contributions
                        .get(host_pk.as_str())
                        .cloned()
                        .unwrap_or_else(|| vec![0.0; FRAME_SIZE]);
                    let minus_host: Vec<f32> = total
                        .iter()
                        .zip(host_contrib.iter())
                        .map(|(t, c)| (t - c).clamp(-1.0, 1.0))
                        .collect();
                    // Non-blocking: drop frame if consumer is slow rather than stall the mix tick.
                    let _ = pcm_tx.try_send(minus_host);
                }

                // Mix-minus per remote speaker: encode and forward on each return channel.
                let mut dead_returns: Vec<String> = Vec::new();
                for (pubkey, (opus_tx, ret_encoder)) in return_channels.iter_mut() {
                    let contrib = contributions
                        .get(pubkey.as_str())
                        .cloned()
                        .unwrap_or_else(|| vec![0.0; FRAME_SIZE]);
                    let minus_self: Vec<f32> = total
                        .iter()
                        .zip(contrib.iter())
                        .map(|(t, c)| (t - c).clamp(-1.0, 1.0))
                        .collect();
                    for packet in ret_encoder.push_samples(&minus_self) {
                        if opus_tx.try_send(packet).is_err() {
                            dead_returns.push(pubkey.clone());
                            break;
                        }
                    }
                }
                for pk in dead_returns {
                    return_channels.remove(&pk);
                }
            }
        }
    }

    log::info!("[mixer] actor stopped ({frame_count} frames encoded)");
}
