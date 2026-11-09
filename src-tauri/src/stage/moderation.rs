use super::state::{ActiveStage, Moderation};
use super::{CLOSE_BANNED, CLOSE_NORMAL, Outcome};
use crate::error::AppError;
use proscenium_types::{
    MAX_STAGE_CHAT_CHARS, MAX_STAGE_REACTION_CHARS, StageControl, StageEvent, StageRole,
    is_valid_pubkey, now_millis, short_id, truncate_chars,
};
use std::sync::atomic::Ordering;

impl ActiveStage {
    // ---- Actions of the local participant --------------------------------

    /// Request a speaker slot. If the host already assigned a speaking role to the
    /// local user, the speaker pipeline starts now.
    pub(super) async fn raise_hand(&mut self) -> Result<(), AppError> {
        if self.my_role != StageRole::Listener {
            return Err(AppError::Other(
                "cannot raise hand: already a speaker".into(),
            ));
        }
        self.hand_raised = true;
        self.broadcast(StageControl::RaiseHand).await;
        self.emit(StageEvent::HandRaised {
            pubkey: self.my_pubkey.clone(),
        });
        self.sync_local_role();
        Ok(())
    }

    pub(super) async fn lower_hand(&mut self) -> Result<(), AppError> {
        self.hand_raised = false;
        self.broadcast(StageControl::LowerHand).await;
        self.emit(StageEvent::HandLowered {
            pubkey: self.my_pubkey.clone(),
        });
        Ok(())
    }

    /// Flip the local microphone mute flag. Returns the new value.
    pub(super) async fn toggle_self_mute(&mut self) -> Result<bool, AppError> {
        let muted = !self.self_muted.fetch_xor(true, Ordering::Relaxed);
        self.broadcast(StageControl::SelfMuteToggle { muted }).await;
        self.emit(StageEvent::MuteChanged {
            pubkey: self.my_pubkey.clone(),
            self_muted: muted,
            host_muted: self.roles.is_host_muted(&self.my_pubkey),
        });
        Ok(muted)
    }

    pub(super) async fn send_reaction(&mut self, emoji: String) -> Result<(), AppError> {
        let emoji = truncate_chars(emoji.trim(), MAX_STAGE_REACTION_CHARS).to_string();
        if emoji.is_empty() {
            return Err(AppError::Other("reaction is empty".into()));
        }
        self.broadcast(StageControl::Reaction {
            emoji: emoji.clone(),
        })
        .await;
        self.emit(StageEvent::Reaction {
            pubkey: self.my_pubkey.clone(),
            emoji,
        });
        Ok(())
    }

    pub(super) async fn send_chat(&mut self, text: String) -> Result<(), AppError> {
        let text = truncate_chars(text.trim(), MAX_STAGE_CHAT_CHARS).to_string();
        if text.is_empty() {
            return Err(AppError::Other("chat message is empty".into()));
        }
        self.broadcast(StageControl::Chat { text: text.clone() })
            .await;
        self.emit(StageEvent::Chat {
            pubkey: self.my_pubkey.clone(),
            text,
        });
        Ok(())
    }

    // ---- Local role -------------------------------------------------------

    /// Start or stop the local speaker pipeline so that `my_role` equals the role the
    /// host assigned. Returns true if `my_role` changed.
    ///
    /// A change from listener to a speaking role starts microphone capture, so it is
    /// applied only while `hand_raised` is set; otherwise `my_role` stays `Listener`.
    /// A change to listener stops the speaker pipeline, the microphone and any relay,
    /// and restarts the listener pipeline at the host.
    pub(super) fn sync_local_role(&mut self) -> bool {
        if self.host.is_some() {
            return false;
        }
        let assigned = self.roles.role_of(&self.my_pubkey);
        if assigned == self.my_role || assigned == StageRole::Host {
            return false;
        }
        let speaks = |role| matches!(role, StageRole::Speaker | StageRole::CoHost);
        match (speaks(self.my_role), speaks(assigned)) {
            (false, true) => {
                if !self.hand_raised {
                    log::info!("[stage] not acting on a promotion: hand is not raised");
                    return false;
                }
                self.stop_listener();
                self.start_speaker();
                self.hand_raised = false;
                if let Some(me) = self.participants.get_mut(&self.my_pubkey) {
                    me.hand_raised = false;
                }
            }
            (true, false) => {
                self.stop_speaker();
                self.stop_relay();
                let host_node_id = self.host_node_id.clone();
                self.start_listener(host_node_id);
            }
            _ => {}
        }
        log::info!("[stage] local role: {:?} -> {assigned:?}", self.my_role);
        self.my_role = assigned;
        self.emit_snapshot();
        true
    }

    // ---- Moderation -------------------------------------------------------

    /// Run a moderation command of the local user: check the permission, apply the
    /// action, broadcast the signed message and, on the host, the new `RoomState`.
    pub(super) async fn moderate(
        &mut self,
        action: Moderation,
        target: String,
    ) -> Result<(), AppError> {
        if !is_valid_pubkey(&target) {
            return Err(AppError::Other("invalid participant pubkey".into()));
        }
        if !self.roles.may_moderate(&self.my_pubkey, &target, action) {
            return Err(AppError::Other(format!(
                "cannot {}: not permitted",
                action_name(action)
            )));
        }
        self.apply_moderation(action, &target).await?;

        let control = match action {
            Moderation::Promote => StageControl::PromoteSpeaker { pubkey: target },
            Moderation::Demote => StageControl::DemoteSpeaker { pubkey: target },
            Moderation::Mute => StageControl::MuteSpeaker { pubkey: target },
            Moderation::Kick => StageControl::Kick { pubkey: target },
            Moderation::Ban => StageControl::Ban { pubkey: target },
        };
        self.broadcast(control).await;
        self.publish_roles().await;
        Ok(())
    }

    /// Apply a moderation action whose issuer the caller authorized with
    /// `RoomRoles::may_moderate`.
    ///
    /// On the host this changes the authoritative roles and enforces them: it closes the
    /// connections of a demoted, kicked or banned identity and discards the audio of a
    /// muted speaker. On other nodes it changes the local copy of the roles until the
    /// next `RoomState` replaces it.
    ///
    /// Returns `Outcome::Kicked` if the local user was kicked or banned.
    pub(super) async fn apply_moderation(
        &mut self,
        action: Moderation,
        target: &str,
    ) -> Result<Outcome, AppError> {
        let is_host = self.host.is_some();
        match action {
            Moderation::Promote => {
                if is_host {
                    let raised = self.participants.get(target).map(|p| p.hand_raised);
                    match raised {
                        None => {
                            return Err(AppError::Other(
                                "cannot promote: not a participant".into(),
                            ));
                        }
                        Some(false) => {
                            return Err(AppError::Other(
                                "cannot promote: participant has not raised a hand".into(),
                            ));
                        }
                        Some(true) => {}
                    }
                }
                if !self.roles.promote(target) {
                    return Err(AppError::Other(
                        "cannot promote: already a speaker, banned, or the speaker limit is reached"
                            .into(),
                    ));
                }
                if let Some(p) = self.participants.get_mut(target) {
                    p.hand_raised = false;
                }
                if let Some(host) = self.host.as_mut() {
                    host.topology.release_listener(target);
                }
                self.emit_role_change(target, StageRole::Speaker);
            }
            Moderation::Demote => {
                if !self.roles.demote(target) {
                    return Err(AppError::Other("cannot demote: not a speaker".into()));
                }
                self.close_speaker_connections_of(target, CLOSE_NORMAL, b"demoted");
                self.remove_relay_of(target).await;
                if is_host && self.participants.contains_key(target) {
                    self.assign_new_listener(target).await;
                }
                self.emit_role_change(target, StageRole::Listener);
            }
            Moderation::Mute => {
                if !self.roles.mute(target) {
                    return Err(AppError::Other(
                        "cannot mute: not a speaker, or already muted".into(),
                    ));
                }
                if let Some(host) = self.host.as_ref() {
                    for speaker in host.speaker_conns.values() {
                        if speaker.master_pubkey == target {
                            speaker.muted.store(true, Ordering::Relaxed);
                        }
                    }
                }
                let self_muted = if target == self.my_pubkey {
                    self.self_muted.load(Ordering::Relaxed)
                } else {
                    self.participants.get(target).is_some_and(|p| p.self_muted)
                };
                self.emit(StageEvent::MuteChanged {
                    pubkey: target.to_string(),
                    self_muted,
                    host_muted: true,
                });
            }
            Moderation::Kick | Moderation::Ban => {
                if action == Moderation::Ban {
                    if !self.roles.ban(target) {
                        return Err(AppError::Other("cannot ban: the ban list is full".into()));
                    }
                } else {
                    self.roles.remove(target);
                }
                if target == self.my_pubkey {
                    return Ok(Outcome::Kicked);
                }
                let code = if action == Moderation::Ban {
                    CLOSE_BANNED
                } else {
                    CLOSE_NORMAL
                };
                self.remove_participant(target, code).await;
            }
        }
        Ok(Outcome::Continue)
    }

    /// Report a role change to the frontend. For another participant this is a
    /// `RoleChanged` event. For the local user the role is applied with
    /// `sync_local_role` and reported as a state snapshot, because `my_role` does not
    /// change when a promotion is not acted on.
    fn emit_role_change(&mut self, target: &str, role: StageRole) {
        if target != self.my_pubkey {
            self.emit(StageEvent::RoleChanged {
                pubkey: target.to_string(),
                role,
            });
        } else if !self.sync_local_role() {
            self.emit_snapshot();
        }
    }

    /// Remove a participant: its entry, its topology slot, its relay and its audio
    /// connections to this node. Emits `ParticipantLeft` if the entry existed.
    pub(super) async fn remove_participant(&mut self, pubkey: &str, close_code: u32) {
        self.close_connections_of(pubkey, close_code, b"removed");
        self.remove_relay_of(pubkey).await;
        if let Some(host) = self.host.as_mut() {
            host.topology.release_listener(pubkey);
        }
        if self.participants.remove(pubkey).is_some() {
            self.emit(StageEvent::ParticipantLeft {
                pubkey: pubkey.to_string(),
            });
        }
    }

    // ---- Presence expiry --------------------------------------------------

    /// Remove participants whose last `Presence` is older than `PRESENCE_EXPIRY`.
    /// The local user is never removed. The host keeps a participant that has an audio
    /// connection to it, and takes the speaking role from one it removes.
    pub(super) async fn sweep_presence(&mut self) {
        let now = now_millis();
        let expiry_ms = u64::try_from(super::control::PRESENCE_EXPIRY.as_millis()).unwrap_or(0);
        let cutoff = now.saturating_sub(expiry_ms);
        let expired: Vec<String> = self
            .participants
            .values()
            .filter(|p| p.last_seen_ms < cutoff && p.pubkey != self.my_pubkey)
            .map(|p| p.pubkey.clone())
            .filter(|pk| !self.has_connection(pk))
            .collect();

        let mut roles_changed = false;
        for pubkey in expired {
            log::debug!("[stage] participant {} timed out", short_id(&pubkey));
            if self.host.is_some() && self.roles.demote(&pubkey) {
                roles_changed = true;
            }
            self.remove_participant(&pubkey, CLOSE_NORMAL).await;
        }
        if roles_changed {
            self.publish_roles().await;
        }

        let participants = &self.participants;
        let host_pubkey = &self.roles.host_pubkey;
        self.senders.prune(now, |sender| {
            sender == host_pubkey || participants.contains_key(sender)
        });
    }
}

fn action_name(action: Moderation) -> &'static str {
    match action {
        Moderation::Promote => "promote",
        Moderation::Demote => "demote",
        Moderation::Mute => "mute",
        Moderation::Kick => "kick",
        Moderation::Ban => "ban",
    }
}
