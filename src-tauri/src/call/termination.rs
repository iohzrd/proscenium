use crate::error::AppError;
use proscenium_types::{CallState, short_id};

use super::state::PeerNotice;

impl super::CallHandler {
    /// Reject an incoming call or cancel an outgoing call.
    pub async fn reject_call(&self, call_id: &str) -> Result<(), AppError> {
        if self
            .end_call(call_id, CallState::Ended, PeerNotice::Reject)
            .await
        {
            Ok(())
        } else {
            Err(AppError::Other(format!(
                "no active call {}",
                short_id(call_id)
            )))
        }
    }

    /// Hang up the current call.
    pub async fn hangup(&self) -> Result<(), AppError> {
        let call = self.active_call.lock().await.take();
        let Some(call) = call else {
            return Err(AppError::Other("no active call".into()));
        };
        self.finish_call(call, CallState::Ended, PeerNotice::Hangup)
            .await;
        Ok(())
    }

    /// Called when a peer sends CallReject or CallHangup.
    /// Ends the active call only if the signal names it and comes from its peer.
    pub async fn on_call_ended(&self, call_id: &str, peer_pubkey: &str) {
        self.end_call_if(
            call_id,
            |call| call.peer_pubkey == peer_pubkey,
            CallState::Ended,
            PeerNotice::None,
        )
        .await;
    }
}
