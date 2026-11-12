mod initiator;
mod responder;
mod session;
mod state;
mod termination;

use crate::dm::DmHandler;
use crate::storage::Storage;
use iroh::Endpoint;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use proscenium_types::{CallEvent, CallState, short_id};
use state::{ActiveCall, CONNECT_TIMEOUT, CallPhase};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Semaphore};

/// `CallReject` signals for offers received during a call that may be in transit at one
/// time. Further offers are ignored without a reply.
const MAX_PENDING_REJECTS: usize = 4;

#[derive(Clone)]
pub struct CallHandler {
    pub(crate) storage: Arc<Storage>,
    pub(crate) endpoint: Endpoint,
    pub(crate) dm: DmHandler,
    app_handle: AppHandle,
    pub(crate) active_call: Arc<Mutex<Option<ActiveCall>>>,
    reject_permits: Arc<Semaphore>,
}

impl CallHandler {
    pub fn new(
        storage: Arc<Storage>,
        endpoint: Endpoint,
        dm: DmHandler,
        app_handle: AppHandle,
    ) -> Self {
        Self {
            storage,
            endpoint,
            dm,
            app_handle,
            active_call: Arc::new(Mutex::new(None)),
            reject_permits: Arc::new(Semaphore::new(MAX_PENDING_REJECTS)),
        }
    }

    pub(crate) fn emit_call_event(&self, call_id: &str, peer_pubkey: &str, state: CallState) {
        let _ = self.app_handle.emit(
            "call-state",
            CallEvent {
                call_id: call_id.to_string(),
                peer_pubkey: peer_pubkey.to_string(),
                state,
            },
        );
    }
}

impl std::fmt::Debug for CallHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallHandler").finish_non_exhaustive()
    }
}

fn reject(reason: &'static str) -> AcceptError {
    AcceptError::from_err(std::io::Error::other(reason))
}

impl ProtocolHandler for CallHandler {
    /// Accept the caller's audio connection for a call the local user answered.
    ///
    /// The connection is served only if the remote NodeId, which QUIC authenticates, is
    /// the device that sent the offer of the active call, and the call is incoming and
    /// in phase `Connecting` (`accept_call` was invoked). The caller's stream becomes
    /// visible with its first audio frame, which the session then reads.
    ///
    /// Microphone capture starts after these checks.
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote_str = conn.remote_id().to_string();
        log::info!(
            "[call] incoming audio connection from {}",
            short_id(&remote_str)
        );

        // Resolve transport NodeId to master pubkey
        let Some(peer_pubkey) = self
            .storage
            .get_master_pubkey_for_transport(&remote_str)
            .await
        else {
            conn.close(1u32.into(), b"unknown peer");
            return Err(reject("audio connection from an unknown node"));
        };

        let call_id = {
            let lock = self.active_call.lock().await;
            match lock.as_ref() {
                Some(call)
                    if call.peer_pubkey == peer_pubkey
                        && call.peer_device == Some(conn.remote_id())
                        && !call.outgoing
                        && call.phase == CallPhase::Connecting =>
                {
                    call.call_id.clone()
                }
                _ => {
                    log::warn!(
                        "[call] rejecting unexpected audio connection from {}",
                        short_id(&peer_pubkey)
                    );
                    conn.close(1u32.into(), b"no accepted call");
                    return Err(reject("no accepted call with this peer"));
                }
            }
        };

        let Ok(Ok((send, recv))) = tokio::time::timeout(CONNECT_TIMEOUT, conn.accept_bi()).await
        else {
            conn.close(1u32.into(), b"no stream");
            return Err(reject("audio stream was not opened"));
        };

        // The call can end, or another connection can take it, while the stream opens.
        let activated = {
            let mut lock = self.active_call.lock().await;
            match lock.as_mut() {
                Some(call) if call.call_id == call_id && call.phase == CallPhase::Connecting => {
                    call.enter_phase(CallPhase::Active);
                    true
                }
                _ => false,
            }
        };
        if !activated {
            conn.close(1u32.into(), b"call ended");
            return Err(reject("call ended before the audio stream opened"));
        }

        log::info!(
            "[call] accepting audio stream for call {}",
            short_id(&call_id)
        );
        self.run_audio_session(conn, send, recv, &call_id).await;
        Ok(())
    }
}
