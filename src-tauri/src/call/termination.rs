use crate::audio::android;
use crate::error::AppError;
use proscenium_types::{CallState, DmPayload, short_id};

impl super::CallHandler {
    /// Reject an incoming call or cancel an outgoing call.
    pub async fn reject_call(&self, call_id: &str) -> Result<(), AppError> {
        let peer_pubkey = {
            let mut lock = self.active_call.lock().await;
            match lock.take() {
                Some(call) if call.call_id == call_id => {
                    call.cancel.cancel();
                    call.peer_pubkey
                }
                Some(other) => {
                    let pk = other.peer_pubkey.clone();
                    *lock = Some(other);
                    return Err(AppError::Other(format!(
                        "active call is {}, not {call_id}",
                        short_id(&pk)
                    )));
                }
                None => return Err(AppError::Other("no active call".into())),
            }
        };

        let reject = DmPayload::CallReject {
            call_id: call_id.to_string(),
        };
        let _ = self.dm.send_signal(&peer_pubkey, reject).await;
        self.emit_call_event(call_id, &peer_pubkey, CallState::Ended);
        Ok(())
    }

    /// Hang up the current call.
    pub async fn hangup(&self) -> Result<(), AppError> {
        let call = {
            let mut lock = self.active_call.lock().await;
            lock.take()
        };
        let Some(call) = call else {
            return Err(AppError::Other("no active call".into()));
        };

        call.cancel.cancel();
        let hangup = DmPayload::CallHangup {
            call_id: call.call_id.clone(),
        };
        let _ = self.dm.send_signal(&call.peer_pubkey, hangup).await;
        self.emit_call_event(&call.call_id, &call.peer_pubkey, CallState::Ended);
        android::restore_default_routing();
        log::info!("[call] hung up {}", short_id(&call.call_id));
        Ok(())
    }

    /// Called when peer sends CallReject or CallHangup.
    pub async fn on_call_ended(&self, call_id: &str) {
        let mut lock = self.active_call.lock().await;
        if let Some(call) = &*lock
            && call.call_id == call_id
        {
            call.cancel.cancel();
            let pk = call.peer_pubkey.clone();
            *lock = None;
            drop(lock);
            self.emit_call_event(call_id, &pk, CallState::Ended);
            android::restore_default_routing();
        }
    }
}
