use super::auth::HostAuthState;
use super::fanout::Fanout;
use crate::audio::{FRAME_SIZE, OpusEncoder, SAMPLES_PER_FRAME, TAG_NORMAL};

/// Per-speaker sample buffer cap: 5 frames (~100ms). Excess oldest samples
/// are dropped to prevent a rogue or slow speaker from causing unbounded growth.
const MAX_SPEAKER_BUFFER: usize = FRAME_SIZE * 5;
use crate::error::AppError;
use iroh::SecretKey;
use proscenium_types::short_id;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---- Command enum -------------------------------------------------------

pub enum MixerCommand {
    /// New speaker joined — register their decoded PCM channel with the mixer.
    AddSpeaker {
        pubkey: String,
        pcm_rx: mpsc::Receiver<Vec<f32>>,
    },
    /// Mark a speaker as the local host and provide a direct PCM channel for
    /// mix-minus playback. The host receives all other speakers except themselves.
    SetHostSpeaker {
        pubkey: String,
        pcm_tx: mpsc::Sender<Vec<f32>>,
    },
}

// ---- Handle -------------------------------------------------------------

/// Cheap-to-clone handle to the HostMixer actor.
#[derive(Clone)]
pub struct MixerHandle {
    cmd_tx: mpsc::Sender<MixerCommand>,
}

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
}

// ---- Actor --------------------------------------------------------------

/// Spawn the HostMixer Tokio actor.
///
/// Returns `(MixerHandle, Arc<Fanout>)`:
/// - `MixerHandle`: add/remove speakers, query RMS levels
/// - `Arc<Fanout>`: the listener mix fanout — add QUIC-stream subscribers for listeners/relays
///
/// `host_sfu_fanout`: the host's own voice SFU fanout — the mixer encodes the host's
/// contribution separately and sends to this fanout each tick so connected speakers
/// can receive the host's voice via uni-streams.
///
/// The actor owns the encoders, auth state, and all per-speaker buffers exclusively.
pub fn spawn_mixer(
    signing_key: SecretKey,
    host_sfu_fanout: Arc<Fanout>,
    cancel: CancellationToken,
) -> Result<(MixerHandle, Arc<Fanout>), AppError> {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let listener_fanout = Arc::new(Fanout::new());
    let actor_fanout = listener_fanout.clone();

    tokio::spawn(run_mixer(
        cmd_rx,
        actor_fanout,
        host_sfu_fanout,
        signing_key,
        cancel,
    ));

    Ok((MixerHandle { cmd_tx }, listener_fanout))
}

/// The mixer actor loop. Runs until `cancel` fires or all senders are dropped.
async fn run_mixer(
    mut cmd_rx: mpsc::Receiver<MixerCommand>,
    listener_fanout: Arc<Fanout>,
    host_sfu_fanout: Arc<Fanout>,
    signing_key: SecretKey,
    cancel: CancellationToken,
) {
    let mut listener_encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("[mixer] failed to create listener Opus encoder: {e}");
            return;
        }
    };

    let mut host_sfu_encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            log::error!("[mixer] failed to create host SFU encoder: {e}");
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

    let mut listener_seq: u32 = 0;
    let mut listener_ts: u32 = 0;
    let mut host_sfu_seq: u32 = 0;
    let mut host_sfu_ts: u32 = 0;
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
                    Some(MixerCommand::SetHostSpeaker { pubkey, pcm_tx }) => {
                        host_speaker = Some((pubkey, pcm_tx));
                    }
                    None => break,
                }
            }

            _ = mix_interval.tick() => {
                // Drain all available PCM from each speaker's channel into their buffer.
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

                // Full mix (clamped) → encode once → listener fanout.
                let full_mix: Vec<f32> = total.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
                let packets = listener_encoder.push_samples(&full_mix);
                for packet in &packets {
                    let (tag, wire_payload) = auth.process(packet);
                    listener_fanout.send_frame(listener_seq, listener_ts, tag, wire_payload);
                    listener_seq = listener_seq.wrapping_add(1);
                    listener_ts = listener_ts.wrapping_add(SAMPLES_PER_FRAME as u32);
                    frame_count = frame_count.wrapping_add(1);
                }

                // Mix-minus for host local playback: PCM of all speakers except the host.
                let host_contrib = if let Some((ref host_pk, _)) = host_speaker {
                    contributions
                        .get(host_pk.as_str())
                        .cloned()
                        .unwrap_or_else(|| vec![0.0; FRAME_SIZE])
                } else {
                    vec![0.0; FRAME_SIZE]
                };

                if let Some((_, ref pcm_tx)) = host_speaker {
                    let minus_host: Vec<f32> = total
                        .iter()
                        .zip(host_contrib.iter())
                        .map(|(t, c)| (t - c).clamp(-1.0, 1.0))
                        .collect();
                    let _ = pcm_tx.try_send(minus_host);
                }

                // Host SFU stream: encode host's own contribution and send to
                // all connected speakers via the host_sfu_fanout.
                let host_only: Vec<f32> = host_contrib
                    .iter()
                    .map(|s| s.clamp(-1.0, 1.0))
                    .collect();
                for packet in host_sfu_encoder.push_samples(&host_only) {
                    host_sfu_fanout.send_frame(host_sfu_seq, host_sfu_ts, TAG_NORMAL, packet);
                    host_sfu_seq = host_sfu_seq.wrapping_add(1);
                    host_sfu_ts = host_sfu_ts.wrapping_add(SAMPLES_PER_FRAME as u32);
                }
            }
        }
    }

    log::info!("[mixer] actor stopped ({frame_count} frames encoded)");
}
