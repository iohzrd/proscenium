use super::control::ControlSigner;
use super::fanout::Fanout;
use super::pipeline::{BACKOFF_INITIAL, BACKOFF_MAX, STREAM_IDLE_TIMEOUT, open_audio_connection};
use super::{CLOSE_NORMAL, CONN_TYPE_LISTENER};
use crate::audio::read_audio_frame;
use iroh::{Endpoint, EndpointId};
use proscenium_types::short_id;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Parameters of the relay upstream task.
pub(super) struct RelayConfig {
    pub(super) endpoint: Endpoint,
    /// The host's transport NodeId. A relay always reads the mixed stream from the host.
    pub(super) host_node_id: String,
    pub(super) signer: Arc<ControlSigner>,
    /// Downstream listeners subscribe to this fanout.
    pub(super) fanout: Arc<Fanout>,
    pub(super) cancel: CancellationToken,
}

/// Relay upstream task: connect to the host as a listener and forward every received
/// frame, unmodified, to `config.fanout`. Reconnects with exponential backoff when the
/// stream ends; the delay returns to `BACKOFF_INITIAL` after a session that delivered
/// frames. Runs until `config.cancel` is cancelled.
///
/// The frames keep the host's header and checkpoint data, so downstream listeners verify
/// the stream against the host key.
pub(super) async fn run_relay_upstream(config: RelayConfig) {
    let Ok(host_id) = config.host_node_id.parse::<EndpointId>() else {
        log::error!("[stage-relay] invalid host node id");
        return;
    };
    let mut backoff = BACKOFF_INITIAL;

    loop {
        let forwarded = forward_once(&config, host_id).await;
        if config.cancel.is_cancelled() {
            break;
        }
        if forwarded > 0 {
            backoff = BACKOFF_INITIAL;
        }
        log::info!(
            "[stage-relay] upstream ended after {forwarded} frames, reconnecting in {backoff:?}"
        );
        tokio::select! {
            _ = config.cancel.cancelled() => break,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
    log::info!("[stage-relay] upstream task stopped");
}

/// One upstream session. Returns the number of frames forwarded.
///
/// The frame read is the only branch besides cancellation, so a partially read frame is
/// discarded only when the session ends.
async fn forward_once(config: &RelayConfig, host_id: EndpointId) -> u64 {
    let opened = tokio::select! {
        _ = config.cancel.cancelled() => return 0,
        opened = open_audio_connection(&config.endpoint, host_id, CONN_TYPE_LISTENER, &config.signer) => opened,
    };
    let (conn, _send, mut recv) = match opened {
        Ok(streams) => streams,
        Err(e) => {
            log::warn!("[stage-relay] connection to host failed: {e}");
            return 0;
        }
    };
    log::info!(
        "[stage-relay] connected to host {}",
        short_id(&config.host_node_id)
    );

    let mut forwarded: u64 = 0;
    loop {
        tokio::select! {
            _ = config.cancel.cancelled() => break,
            result = tokio::time::timeout(STREAM_IDLE_TIMEOUT, read_audio_frame(&mut recv)) => {
                match result {
                    Ok(Ok(Some(frame))) => {
                        config.fanout.send_frame(frame.seq, frame.timestamp, frame.tag, frame.payload);
                        forwarded += 1;
                    }
                    Ok(Ok(None)) => break,
                    Ok(Err(e)) => {
                        log::info!("[stage-relay] upstream read error: {e}");
                        break;
                    }
                    Err(_) => {
                        log::warn!("[stage-relay] no frame from host within {STREAM_IDLE_TIMEOUT:?}");
                        break;
                    }
                }
            }
        }
    }
    conn.close(CLOSE_NORMAL.into(), b"relay stopped");
    forwarded
}
