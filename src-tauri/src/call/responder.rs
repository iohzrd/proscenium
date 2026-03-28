use crate::error::AppError;
use proscenium_types::{CallState, DmPayload, short_id};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio_util::sync::CancellationToken;

use super::state::ActiveCall;

impl super::CallHandler {
    /// Called when we receive a CallOffer from a peer (via DM handler).
    pub async fn on_call_offer(&self, call_id: &str, peer_pubkey: &str) {
        {
            let lock = self.active_call.lock().await;
            if lock.is_some() {
                // Already in a call, auto-reject
                log::info!(
                    "[call] auto-rejecting offer {} (already in call)",
                    short_id(call_id)
                );
                let reject = DmPayload::CallReject {
                    call_id: call_id.to_string(),
                };
                let _ = self.dm.send_signal(peer_pubkey, reject).await;
                return;
            }
        }

        let cancel = CancellationToken::new();
        {
            let mut lock = self.active_call.lock().await;
            *lock = Some(ActiveCall {
                call_id: call_id.to_string(),
                peer_pubkey: peer_pubkey.to_string(),
                cancel: cancel.clone(),
                muted: Arc::new(AtomicBool::new(false)),
                capture: Arc::new(std::sync::Mutex::new(None)),
                playback: Arc::new(std::sync::Mutex::new(None)),
            });
        }

        self.emit_call_event(call_id, peer_pubkey, CallState::Incoming);

        // Timeout: auto-reject after 30s if not answered
        let handler = self.clone();
        let cid = call_id.to_string();
        let pk = peer_pubkey.to_string();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {},
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                    log::info!("[call] incoming call {} timed out", short_id(&cid));
                    handler.emit_call_event(&cid, &pk, CallState::Ended);
                    let mut lock = handler.active_call.lock().await;
                    if lock.as_ref().is_some_and(|c| c.call_id == cid) {
                        let reject = DmPayload::CallReject { call_id: cid };
                        let _ = handler.dm.send_signal(&pk, reject).await;
                        *lock = None;
                    }
                }
            }
        });
    }

    /// Accept an incoming call. Sends CallAnswer via DM and waits for the
    /// caller to open the CALL_ALPN connection (handled by ProtocolHandler::accept).
    pub async fn accept_call(&self, call_id: &str) -> Result<(), AppError> {
        let peer_pubkey = {
            let mut lock = self.active_call.lock().await;
            match &mut *lock {
                Some(call) if call.call_id == call_id => {
                    call.cancel.cancel();
                    call.cancel = CancellationToken::new();
                    call.peer_pubkey.clone()
                }
                _ => return Err(AppError::Other("no incoming call with that ID".into())),
            }
        };

        log::info!(
            "[call] accepting call {} from {}",
            short_id(call_id),
            short_id(&peer_pubkey)
        );

        let answer = DmPayload::CallAnswer {
            call_id: call_id.to_string(),
        };
        self.dm.send_signal(&peer_pubkey, answer).await?;
        self.emit_call_event(call_id, &peer_pubkey, CallState::Active);
        Ok(())
    }
}
