use super::control::VerifiedControl;
use super::state::{ActiveStage, Moderation, Participant};
use super::{CLOSE_BANNED, Outcome};
use proscenium_types::{
    MAX_STAGE_PARTICIPANTS, StageControl, StageEvent, StageRole, now_millis, short_id,
};

impl ActiveStage {
    /// Process a control message whose signature, payload limits and timestamp window
    /// the receive task checked.
    ///
    /// The actor of a message is the master pubkey bound to its signer: by the delegation
    /// of the `Presence` itself, or by an earlier verified `Presence` of that signer.
    /// A message from an unbound signer, a banned identity, or with a timestamp that is
    /// not higher than the signer's last one is discarded.
    pub(super) async fn handle_control(&mut self, msg: VerifiedControl) -> Outcome {
        let VerifiedControl { signed, presence } = msg;
        if signed.stage_id != self.stage_id {
            return Outcome::Continue;
        }

        let actor = match (&signed.control, presence) {
            (StageControl::Presence { .. }, Some(presence)) => {
                if self.roles.is_banned(&presence.master_pubkey)
                    || !self
                        .signers
                        .bind(&presence.signing_pubkey, &presence.master_pubkey)
                {
                    return Outcome::Continue;
                }
                presence.master_pubkey
            }
            (StageControl::Presence { .. }, None) => return Outcome::Continue,
            _ => match self.signers.master_of(&signed.sender_pubkey) {
                Some(master) => master.to_string(),
                None => {
                    log::debug!(
                        "[stage-ctrl] discarding message of unknown signer {}",
                        short_id(&signed.sender_pubkey)
                    );
                    return Outcome::Continue;
                }
            },
        };
        if !self
            .signers
            .accept_timestamp(&signed.sender_pubkey, signed.timestamp)
        {
            log::debug!(
                "[stage-ctrl] discarding replayed message of {}",
                short_id(&actor)
            );
            return Outcome::Continue;
        }
        if self.roles.is_banned(&actor) {
            return Outcome::Continue;
        }
        let from_host = actor == self.roles.host_pubkey;
        let is_host = self.host.is_some();

        match signed.control {
            StageControl::Presence { .. } => self.handle_presence(actor).await,
            StageControl::End => {
                if from_host && !is_host {
                    return Outcome::Ended;
                }
            }
            StageControl::RoomState(state) => {
                if from_host && !is_host {
                    return self.handle_room_state(&state);
                }
            }
            StageControl::RaiseHand => {
                if let Some(p) = self.participants.get_mut(&actor) {
                    p.hand_raised = true;
                    self.emit(StageEvent::HandRaised { pubkey: actor });
                }
            }
            StageControl::LowerHand => {
                if let Some(p) = self.participants.get_mut(&actor) {
                    p.hand_raised = false;
                    self.emit(StageEvent::HandLowered { pubkey: actor });
                }
            }
            StageControl::SelfMuteToggle { muted } => {
                if let Some(p) = self.participants.get_mut(&actor) {
                    p.self_muted = muted;
                    let host_muted = self.roles.is_host_muted(&actor);
                    self.emit(StageEvent::MuteChanged {
                        pubkey: actor,
                        self_muted: muted,
                        host_muted,
                    });
                }
            }
            StageControl::Reaction { emoji } => {
                if let Some(p) = self.participants.get_mut(&actor)
                    && p.reaction_limit.allow(now_millis())
                {
                    self.emit(StageEvent::Reaction {
                        pubkey: actor,
                        emoji,
                    });
                }
            }
            StageControl::Chat { text } => {
                if let Some(p) = self.participants.get_mut(&actor)
                    && p.chat_limit.allow(now_millis())
                {
                    self.emit(StageEvent::Chat {
                        pubkey: actor,
                        text,
                    });
                }
            }
            StageControl::PromoteSpeaker { pubkey } => {
                return self
                    .handle_moderation(&actor, Moderation::Promote, &pubkey)
                    .await;
            }
            StageControl::DemoteSpeaker { pubkey } => {
                return self
                    .handle_moderation(&actor, Moderation::Demote, &pubkey)
                    .await;
            }
            StageControl::MuteSpeaker { pubkey } => {
                return self
                    .handle_moderation(&actor, Moderation::Mute, &pubkey)
                    .await;
            }
            StageControl::Kick { pubkey } => {
                return self
                    .handle_moderation(&actor, Moderation::Kick, &pubkey)
                    .await;
            }
            StageControl::Ban { pubkey } => {
                return self
                    .handle_moderation(&actor, Moderation::Ban, &pubkey)
                    .await;
            }
            StageControl::RelayVolunteer { capacity } => {
                if is_host {
                    self.handle_relay_volunteer(&actor, capacity).await;
                }
            }
            StageControl::RelayAssignment {
                source_node_id,
                listener_pubkeys,
            } => {
                if from_host && !is_host {
                    self.handle_relay_assignment(source_node_id, &listener_pubkeys);
                }
            }
        }
        Outcome::Continue
    }

    /// Record a verified `Presence` of `actor`. Adds the participant if it is new and
    /// the participant limit is not reached.
    async fn handle_presence(&mut self, actor: String) {
        let now = now_millis();
        if let Some(p) = self.participants.get_mut(&actor) {
            p.last_seen_ms = now;
            return;
        }
        if self.participants.len() >= MAX_STAGE_PARTICIPANTS {
            return;
        }
        self.participants
            .insert(actor.clone(), Participant::new(actor.clone(), now));
        let role = self.roles.role_of(&actor);
        self.emit(StageEvent::ParticipantJoined {
            pubkey: actor.clone(),
            role,
        });

        if let Some(host) = self.host.as_mut() {
            // The new participant learns the roles from the next RoomState.
            host.room_state_pending = true;
            if role == StageRole::Listener {
                self.assign_new_listener(&actor).await;
            }
        }
    }

    /// Apply a moderation message of `actor` if the role rules permit it.
    /// On the host a change is followed by a `RoomState` broadcast.
    async fn handle_moderation(
        &mut self,
        actor: &str,
        action: Moderation,
        target: &str,
    ) -> Outcome {
        if !self.roles.may_moderate(actor, target, action) {
            log::debug!(
                "[stage-ctrl] discarding {action:?} of {}: not permitted",
                short_id(actor)
            );
            return Outcome::Continue;
        }
        match self.apply_moderation(action, target).await {
            Ok(outcome) => {
                self.publish_roles().await;
                outcome
            }
            Err(e) => {
                log::debug!(
                    "[stage-ctrl] {action:?} of {} not applied: {e}",
                    short_id(actor)
                );
                Outcome::Continue
            }
        }
    }

    /// Replace the local copy of the roles with a host-signed snapshot and act on the
    /// difference: leave if banned, close connections of banned identities, start or
    /// stop the local speaker pipeline.
    fn handle_room_state(&mut self, state: &proscenium_types::RoomState) -> Outcome {
        let Some(changed) = self.roles.apply_snapshot(state) else {
            return Outcome::Continue;
        };
        if self.roles.is_banned(&self.my_pubkey) {
            return Outcome::Kicked;
        }
        if !changed {
            // The snapshot can still differ from `my_role` if an earlier promotion was
            // not acted on; `sync_local_role` handles that after `raise_hand`.
            return Outcome::Continue;
        }

        let banned: Vec<String> = self
            .participants
            .keys()
            .filter(|pk| self.roles.is_banned(pk))
            .cloned()
            .collect();
        for pubkey in banned {
            self.close_connections_of(&pubkey, CLOSE_BANNED, b"banned");
            self.participants.remove(&pubkey);
        }
        for pubkey in self.roles.speakers.iter().chain(&self.roles.co_hosts) {
            if let Some(p) = self.participants.get_mut(pubkey) {
                p.hand_raised = false;
            }
        }

        // `sync_local_role` emits a snapshot itself when the local role changes.
        if !self.sync_local_role() {
            self.emit_snapshot();
        }
        Outcome::Continue
    }

    /// Switch the listener pipeline to the source the host assigned.
    /// Ignored unless this node is a listener named in the assignment, and for a relay
    /// that is this node itself or that failed before.
    fn handle_relay_assignment(&mut self, source_node_id: String, listener_pubkeys: &[String]) {
        if self.my_role != StageRole::Listener
            || !listener_pubkeys.contains(&self.my_pubkey)
            || source_node_id == self.my_node_id
            || self.failed_upstreams.contains(&source_node_id)
            || self.listener_upstream_id.as_deref() == Some(source_node_id.as_str())
        {
            return;
        }
        log::info!(
            "[stage-listener] assigned to source {}",
            short_id(&source_node_id)
        );
        self.start_listener(source_node_id);
    }
}
