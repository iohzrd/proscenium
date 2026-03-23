use crate::audio::AudioPlayback;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Per-stream buffer cap: 5 frames (~100ms).
pub(super) const MAX_STREAM_BUFFER: usize = crate::audio::FRAME_SIZE * 5;

enum SpeakerMixerCmd {
    AddStream {
        id: u32,
        rx: mpsc::Receiver<Vec<f32>>,
    },
    RemoveStream(u32),
    /// Replace the AEC far-end reference channel (called on each reconnect).
    SetFarEndTx(mpsc::Sender<Vec<f32>>),
}

/// Cheap-to-clone handle to the SpeakerMixer actor.
#[derive(Clone)]
pub(super) struct SpeakerMixerHandle {
    cmd_tx: mpsc::Sender<SpeakerMixerCmd>,
}

impl SpeakerMixerHandle {
    pub(super) async fn add_stream(&self, id: u32, rx: mpsc::Receiver<Vec<f32>>) {
        let _ = self
            .cmd_tx
            .send(SpeakerMixerCmd::AddStream { id, rx })
            .await;
    }

    pub(super) async fn remove_stream(&self, id: u32) {
        let _ = self.cmd_tx.send(SpeakerMixerCmd::RemoveStream(id)).await;
    }

    pub(super) async fn set_far_end_tx(&self, tx: mpsc::Sender<Vec<f32>>) {
        let _ = self.cmd_tx.send(SpeakerMixerCmd::SetFarEndTx(tx)).await;
    }
}

/// Spawn the speaker-side PCM mixer actor.
///
/// Drains per-stream sample buffers on a 20 ms tick, sums them (clamp to [-1,1]),
/// and pushes the mix to `AudioPlayback`. The AEC far-end reference channel is
/// wired via `SpeakerMixerHandle::set_far_end_tx` (called each reconnect).
pub(super) fn spawn_speaker_mixer(cancel: CancellationToken) -> SpeakerMixerHandle {
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    tokio::spawn(run_speaker_mixer(cmd_rx, cancel));
    SpeakerMixerHandle { cmd_tx }
}

async fn run_speaker_mixer(mut cmd_rx: mpsc::Receiver<SpeakerMixerCmd>, cancel: CancellationToken) {
    let (mut prod, _playback) = match AudioPlayback::start() {
        Ok(p) => p,
        Err(e) => {
            log::error!("[stage-speaker-mixer] failed to start playback: {e}");
            return;
        }
    };

    let mut buffers: HashMap<u32, Vec<f32>> = HashMap::new();
    let mut streams: Vec<(u32, mpsc::Receiver<Vec<f32>>)> = Vec::new();
    let mut far_end_tx: Option<mpsc::Sender<Vec<f32>>> = None;
    let mut frames: u32 = 0;

    let mut mix_interval = tokio::time::interval(std::time::Duration::from_millis(20));
    mix_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SpeakerMixerCmd::AddStream { id, rx }) => {
                        buffers.insert(id, Vec::new());
                        streams.push((id, rx));
                    }
                    Some(SpeakerMixerCmd::RemoveStream(id)) => {
                        buffers.remove(&id);
                        streams.retain(|(i, _)| *i != id);
                    }
                    Some(SpeakerMixerCmd::SetFarEndTx(tx)) => {
                        far_end_tx = Some(tx);
                    }
                    None => break,
                }
            }

            _ = mix_interval.tick() => {
                // Drain all streams into per-stream buffers.
                let mut dead: Vec<u32> = Vec::new();
                for (id, rx) in &mut streams {
                    loop {
                        match rx.try_recv() {
                            Ok(samples) => {
                                if let Some(buf) = buffers.get_mut(id) {
                                    buf.extend_from_slice(&samples);
                                    if buf.len() > MAX_STREAM_BUFFER {
                                        let excess = buf.len() - MAX_STREAM_BUFFER;
                                        buf.drain(..excess);
                                    }
                                }
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                dead.push(*id);
                                break;
                            }
                        }
                    }
                }
                for id in dead {
                    buffers.remove(&id);
                    streams.retain(|(i, _)| *i != id);
                }

                // Sum contributions and clamp.
                let mut mix = vec![0.0f32; crate::audio::FRAME_SIZE];
                for buf in buffers.values_mut() {
                    let available = buf.len().min(crate::audio::FRAME_SIZE);
                    for (i, s) in buf.drain(..available).enumerate() {
                        mix[i] += s;
                    }
                }
                let mix: Vec<f32> = mix.iter().map(|s| s.clamp(-1.0, 1.0)).collect();

                if let Some(ref tx) = far_end_tx {
                    let _ = tx.try_send(mix.clone());
                }
                prod.push(&mix);
                frames = frames.wrapping_add(1);
            }
        }
    }

    log::info!("[stage-speaker-mixer] stopped ({frames} frames mixed)");
}
