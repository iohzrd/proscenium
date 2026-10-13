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
use state::{
    ActiveCall, CALL_PROOF_LEN, CONNECT_TIMEOUT, CallPhase, IO_TIMEOUT, ROLE_CALLEE, ROLE_CALLER,
    call_proof, proofs_equal,
};
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
    /// The connection is served only if all of these hold:
    /// - the remote NodeId belongs to the peer of the active call,
    /// - the call is incoming and in phase `Connecting` (`accept_call` was invoked),
    /// - the first bytes of the stream are the caller's proof of the call id.
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

        // The caller opens a bi-stream and sends its proof first.
        let Ok(Ok((mut send, mut recv))) =
            tokio::time::timeout(CONNECT_TIMEOUT, conn.accept_bi()).await
        else {
            conn.close(1u32.into(), b"no stream");
            return Err(reject("audio stream was not opened"));
        };
        let mut proof = [0u8; CALL_PROOF_LEN];
        let read = tokio::time::timeout(IO_TIMEOUT, recv.read_exact(&mut proof)).await;
        if !matches!(read, Ok(Ok(()))) || !proofs_equal(&proof, &call_proof(&call_id, ROLE_CALLER))
        {
            log::warn!(
                "[call] rejecting audio connection from {}: invalid call proof",
                short_id(&peer_pubkey)
            );
            conn.close(1u32.into(), b"invalid call proof");
            return Err(reject("invalid call proof"));
        }

        // The call can end, or another connection can take it, while the proof is read.
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

        let reply = call_proof(&call_id, ROLE_CALLEE);
        let written = tokio::time::timeout(IO_TIMEOUT, send.write_all(&reply)).await;
        if !matches!(written, Ok(Ok(()))) {
            conn.close(1u32.into(), b"handshake failed");
            self.end_call(&call_id, CallState::Failed, state::PeerNotice::Hangup)
                .await;
            return Err(reject("failed to send the call proof"));
        }

        log::info!(
            "[call] accepting audio stream for call {}",
            short_id(&call_id)
        );
        self.run_audio_session(conn, send, recv, &call_id).await;
        Ok(())
    }
}
