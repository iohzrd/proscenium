use crate::error::AppError;
use crate::stage::StageActor;
use crate::stage::pipeline;
use proscenium_types::{
    StageControl, StageEvent, StageRole, now_millis, short_id, sign_stage_control,
};
use std::sync::atomic::Ordering;
use tauri::Emitter;

impl StageActor {
    pub(super) async fn raise_hand(&mut self) -> Result<(), AppError> {
        self.require_active("raise hand")?;
        let stage_id = self.active.as_ref().unwrap().stage_id.clone();
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::RaiseHand {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::HandRaised { pubkey: my_pubkey });
        let _ = stage_id; // used in closure above
        Ok(())
    }

    pub(super) async fn lower_hand(&mut self) -> Result<(), AppError> {
        self.require_active("lower hand")?;
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::LowerHand {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self
            .app_handle
            .emit("stage-event", StageEvent::HandLowered { pubkey: my_pubkey });
        Ok(())
    }

    pub(super) async fn toggle_self_mute(&mut self) -> Result<bool, AppError> {
        let stage = self
            .active
            .as_mut()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;

        let muted = !stage.self_muted.fetch_xor(true, Ordering::Relaxed);
        let my_pubkey = stage.my_pubkey.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::SelfMuteToggle {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                    muted,
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::MuteChanged {
                pubkey: my_pubkey,
                self_muted: muted,
                host_muted: false,
            },
        );
        Ok(muted)
    }

    pub(super) async fn send_reaction(&mut self, emoji: String) -> Result<(), AppError> {
        self.require_active("send reaction")?;
        // Truncate to 16 chars
        let emoji: String = emoji.chars().take(16).collect();
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();
        let emoji_clone = emoji.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::Reaction {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                    emoji: emoji_clone.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::Reaction {
                pubkey: my_pubkey,
                emoji,
            },
        );
        Ok(())
    }

    pub(super) async fn send_chat(&mut self, text: String) -> Result<(), AppError> {
        self.require_active("send chat")?;
        // Truncate to 500 chars
        let text: String = text.chars().take(500).collect();
        let my_pubkey = self.active.as_ref().unwrap().my_pubkey.clone();
        let text_clone = text.clone();

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::Chat {
                    stage_id: sid.to_string(),
                    pubkey: pk.to_string(),
                    text: text_clone.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::Chat {
                pubkey: my_pubkey,
                text,
            },
        );
        Ok(())
    }

    pub(super) async fn promote_speaker(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "promote")?;

        if let Some(p) = self.active.as_mut().unwrap().participants.get_mut(&pubkey) {
            p.role = StageRole::Speaker;
        }

        let pubkey_for_bcast = pubkey.clone();
        self.broadcast_control(move |sid, pk, sk| {
            sign_stage_control(
                StageControl::PromoteSpeaker {
                    stage_id: sid.to_string(),
                    pubkey: pubkey_for_bcast.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::RoleChanged {
                pubkey,
                role: StageRole::Speaker,
            },
        );
        Ok(())
    }

    pub(super) async fn demote_speaker(&mut self, pubkey: String) -> Result<(), AppError> {
        self.require_host_active(&pubkey, "demote")?;

        if let Some(p) = self.active.as_mut().unwrap().participants.get_mut(&pubkey) {
            p.role = StageRole::Listener;
        }

        self.broadcast_control(|sid, pk, sk| {
            sign_stage_control(
                StageControl::DemoteSpeaker {
                    stage_id: sid.to_string(),
                    pubkey: pubkey.clone(),
                },
                pk,
                sk,
                now_millis(),
            )
        })
        .await;

        let _ = self.app_handle.emit(
            "stage-event",
            StageEvent::RoleChanged {
                pubkey,
                role: StageRole::Listener,
            },
        );
        Ok(())
    }

    pub(super) fn sweep_presence(&mut self) {
        let stage = match self.active.as_mut() {
            Some(s) => s,
            None => return,
        };
        let cutoff =
            now_millis().saturating_sub(crate::stage::control::PRESENCE_EXPIRY.as_millis() as u64);
        let expired: Vec<String> = stage
            .participants
            .values()
            .filter(|p| p.last_seen_ms < cutoff && p.pubkey != stage.my_pubkey)
            .map(|p| p.pubkey.clone())
            .collect();
        for pubkey in expired {
            stage.participants.remove(&pubkey);
            log::debug!("[stage] participant {} timed out", short_id(&pubkey));
            let _ = self
                .app_handle
                .emit("stage-event", StageEvent::ParticipantLeft { pubkey });
        }
    }

    pub(super) async fn volunteer_as_relay(&mut self, capacity: u32) -> Result<(), AppError> {
        let stage = self
            .active
            .as_mut()
            .ok_or_else(|| AppError::Other("not in a stage".into()))?;

        let upstream_id = stage
            .listener_upstream_id
            .clone()
            .ok_or_else(|| AppError::Other("no upstream audio source to relay from".into()))?;

        let relay_cancel = stage.cancel.child_token();
        let relay_handle =
            pipeline::start_relay_pipeline(self.endpoint.clone(), upstream_id, relay_cancel)
                .await?;
        stage.relay_handle = Some(relay_handle);

        let id = self.identity.read().await;
        let relay_node_id = id.transport_node_id.clone();
        let signing_pubkey = id.signing_key.public().to_string();
        let signing_key = id.signing_key.clone();
        drop(id);

        let signed = sign_stage_control(
            StageControl::RelayVolunteer {
                stage_id: stage.stage_id.clone(),
                relay_pubkey: stage.my_pubkey.clone(),
                relay_node_id: relay_node_id.clone(),
                capacity,
            },
            &signing_pubkey,
            &signing_key,
            now_millis(),
        );

        if let Err(e) = stage.control_plane.broadcast(&signed).await {
            log::warn!("[stage-relay] relay volunteer broadcast failed: {e}");
        }

        log::info!(
            "[stage] volunteered as relay (cap={capacity}, node={})",
            short_id(&relay_node_id)
        );
        Ok(())
    }
}
