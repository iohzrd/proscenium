use super::auth::HostAuthState;
use super::fanout::Fanout;
use crate::audio::{FRAME_SIZE, OpusEncoder, SAMPLES_PER_FRAME, TAG_NORMAL};
use crate::error::AppError;
use iroh::SecretKey;
use proscenium_types::short_id;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Per-speaker sample buffer cap: 5 frames (~100ms). Excess oldest samples
/// are dropped, so a speaker that sends faster than real time cannot grow the buffer.
const MAX_SPEAKER_BUFFER: usize = FRAME_SIZE * 5;

// ---- Command enum -------------------------------------------------------

pub enum MixerCommand {
    /// New speaker connection: register its decoded PCM channel with the mixer.
    /// The mixer removes the input when the sending side of `pcm_rx` is dropped.
    AddSpeaker {
        /// Identifier of the speaker connection. Unique per connection, so the inputs
        /// of two connections from one node are separate.
        id: u64,
        /// Name used in log messages.
        label: String,
        pcm_rx: mpsc::Receiver<Vec<f32>>,
    },
    /// Mark an input as the local host and provide a direct PCM channel for
    /// mix-minus playback. The host receives all other speakers except themselves.
    SetHostSpeaker {
        id: u64,
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
        id: u64,
        label: String,
        pcm_rx: mpsc::Receiver<Vec<f32>>,
    ) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::AddSpeaker { id, label, pcm_rx })
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
    }

    pub async fn set_host_speaker(
        &self,
        id: u64,
        pcm_tx: mpsc::Sender<Vec<f32>>,
    ) -> Result<(), AppError> {
        self.cmd_tx
            .send(MixerCommand::SetHostSpeaker { id, pcm_tx })
            .await
            .map_err(|_| AppError::Other("mixer actor closed".into()))
    }
}

// ---- Actor --------------------------------------------------------------

/// Spawn the HostMixer Tokio actor.
///
/// Returns `(MixerHandle, Arc<Fanout>)`:
/// - `MixerHandle`: add speaker inputs
/// - `Arc<Fanout>`: the listener mix fanout; add QUIC-stream subscribers for listeners/relays
///
/// `host_sfu_fanout`: the host's own voice SFU fanout. The mixer encodes the host's
/// contribution separately and sends to this fanout each tick so connected speakers
/// can receive the host's voice via uni-streams.
///
/// `master_key` (the host's) and `stage_id` are used to sign the checkpoints of the
/// listener mix.
///
/// The actor owns the encoders, auth state, and all per-speaker buffers exclusively.
pub fn spawn_mixer(
    master_key: SecretKey,
    stage_id: String,
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
        HostAuthState::new(master_key, stage_id),
        cancel,
    ));

    Ok((MixerHandle { cmd_tx }, listener_fanout))
}

/// The mixer actor loop. Runs until `cancel` fires or all senders are dropped.
async fn run_mixer(
    mut cmd_rx: mpsc::Receiver<MixerCommand>,
    listener_fanout: Arc<Fanout>,
    host_sfu_fanout: Arc<Fanout>,
    mut auth: HostAuthState,
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

    // Input id -> sample buffer
    let mut buffers: HashMap<u64, Vec<f32>> = HashMap::new();
    // Separate vec for the PCM receivers so we can drain them each tick
    let mut speaker_channels: Vec<(u64, String, mpsc::Receiver<Vec<f32>>)> = Vec::new();
    // Host input id + direct PCM channel for mix-minus local playback
    let mut host_speaker: Option<(u64, mpsc::Sender<Vec<f32>>)> = None;

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
                    Some(MixerCommand::AddSpeaker { id, label, pcm_rx }) => {
                        log::info!("[mixer] adding speaker {} (input {id})", short_id(&label));
                        buffers.insert(id, Vec::new());
                        speaker_channels.push((id, label, pcm_rx));
                    }
                    Some(MixerCommand::SetHostSpeaker { id, pcm_tx }) => {
                        host_speaker = Some((id, pcm_tx));
                    }
                    None => break,
                }
            }

            _ = mix_interval.tick() => {
                // Drain all available PCM from each speaker's channel into their buffer.
                let mut dead: Vec<u64> = Vec::new();
                for (id, label, rx) in &mut speaker_channels {
                    loop {
                        match rx.try_recv() {
                            Ok(samples) => {
                                if let Some(buf) = buffers.get_mut(id) {
                                    buf.extend_from_slice(&samples);
                                    if buf.len() > MAX_SPEAKER_BUFFER {
                                        let excess = buf.len() - MAX_SPEAKER_BUFFER;
                                        log::warn!(
                                            "[mixer] speaker {} buffer overflow, dropping {excess} oldest samples",
                                            short_id(label)
                                        );
                                        buf.drain(..excess);
                                    }
                                }
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                log::debug!("[mixer] speaker {} channel closed, removing", short_id(label));
                                dead.push(*id);
                                break;
                            }
                        }
                    }
                }
                for id in dead {
                    buffers.remove(&id);
                    speaker_channels.retain(|(other, _, _)| *other != id);
                }

                // Collect the host's contribution and compute the unclamped total sum
                // for mix-minus arithmetic.
                let host_id = host_speaker.as_ref().map(|(id, _)| *id);
                let mut host_contrib = vec![0.0f32; FRAME_SIZE];
                let mut total = vec![0.0f32; FRAME_SIZE];
                for (id, buf) in buffers.iter_mut() {
                    let available = buf.len().min(FRAME_SIZE);
                    let is_host = Some(*id) == host_id;
                    for ((sample, sum), host) in buf
                        .drain(..available)
                        .zip(total.iter_mut())
                        .zip(host_contrib.iter_mut())
                    {
                        *sum += sample;
                        if is_host {
                            *host = sample;
                        }
                    }
                }

                // Full mix (clamped), encoded once, sent to the listener fanout.
                let full_mix: Vec<f32> = total.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
                let packets = listener_encoder.push_samples(&full_mix);
                for packet in &packets {
                    let (tag, wire_payload) = auth.process(listener_seq, listener_ts, packet);
                    listener_fanout.send_frame(listener_seq, listener_ts, tag, wire_payload);
                    listener_seq = listener_seq.wrapping_add(1);
                    listener_ts = listener_ts.wrapping_add(SAMPLES_PER_FRAME as u32);
                    frame_count = frame_count.wrapping_add(1);
                }

                // Mix-minus for host local playback: PCM of all speakers except the host.
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
