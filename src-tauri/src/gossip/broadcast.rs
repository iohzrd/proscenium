use crate::constants::{GOSSIP_FRAME_OVERHEAD, GOSSIP_MAX_MESSAGE_SIZE};
use crate::error::AppError;
use bytes::Bytes;
use proscenium_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, Post, PostTombstone, Profile,
    PushMessage, StageAnnouncement, StageEnd, StageTicket, Visibility, now_millis, short_id,
    sign_stage_announcement, sign_stage_ended,
};

/// A `PushMessage` of `author` without content.
fn empty_push(author: String) -> PushMessage {
    PushMessage {
        author,
        posts: vec![],
        interactions: vec![],
        profile: None,
        announcement: None,
        tombstones: vec![],
        stage: None,
        stage_ended: None,
    }
}

impl super::GossipService {
    /// Serialize `msg` and broadcast it on the own feed topic.
    ///
    /// Returns `Ok(false)` without broadcasting if no own feed topic is open, if the stored
    /// visibility is not Public (the topic is then closed; another device of the user can
    /// have changed it), or if the serialized message does not fit in one gossip frame
    /// (iroh-gossip ends the send loop to a neighbor when asked to write a larger frame).
    pub(super) async fn broadcast_msg(
        &self,
        msg: &GossipMessage,
        label: &str,
    ) -> Result<bool, AppError> {
        let sender = self.inner.lock().await.my_sender.clone();
        let Some(sender) = sender else {
            return Ok(false);
        };
        if self.my_visibility().await? != Visibility::Public {
            self.stop_own_feed().await;
            return Ok(false);
        }
        let payload = serde_json::to_vec(msg)?;
        let max_payload = GOSSIP_MAX_MESSAGE_SIZE - GOSSIP_FRAME_OVERHEAD;
        if payload.len() > max_payload {
            log::warn!(
                "[gossip] {label} not broadcast: {} bytes exceed the {max_payload} byte limit",
                payload.len()
            );
            return Ok(false);
        }
        sender.broadcast(Bytes::from(payload)).await?;
        log::info!("[gossip] broadcast {label}");
        Ok(true)
    }

    async fn my_master_pubkey(&self) -> String {
        self.identity.read().await.master_pubkey.clone()
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> Result<(), AppError> {
        let msg = GossipMessage::ProfileUpdate(profile.clone());
        if !self.broadcast_msg(&msg, "profile").await? {
            self.attempt_push(PushMessage {
                profile: Some(profile.clone()),
                ..empty_push(self.my_master_pubkey().await)
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_post(&self, post: &Post) -> Result<(), AppError> {
        let msg = GossipMessage::NewPost(post.clone());
        if !self
            .broadcast_msg(&msg, &format!("post {}", short_id(&post.id)))
            .await?
        {
            self.attempt_push(PushMessage {
                posts: vec![post.clone()],
                ..empty_push(self.my_master_pubkey().await)
            })
            .await;
        }
        Ok(())
    }

    /// Record the signed deletion of the local user's post `id` as a tombstone, then send
    /// it to followers: on the own feed topic, or by push if no topic is open.
    ///
    /// The tombstone makes the deletion part of the sync protocol, so a follower that is
    /// offline now receives it at its next sync.
    pub async fn broadcast_delete(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> Result<(), AppError> {
        let tombstone = PostTombstone {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        self.storage
            .apply_post_tombstones(author, std::slice::from_ref(&tombstone), now_millis())
            .await?;

        let msg = GossipMessage::DeletePost {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self
            .broadcast_msg(&msg, &format!("delete {}", short_id(id)))
            .await?
        {
            self.attempt_push(PushMessage {
                tombstones: vec![tombstone],
                ..empty_push(author.to_string())
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> Result<(), AppError> {
        let msg = GossipMessage::NewInteraction(interaction.clone());
        let label = format!(
            "{:?} on post {}",
            interaction.kind,
            short_id(&interaction.target_post_id)
        );
        if !self.broadcast_msg(&msg, &label).await? {
            self.attempt_push(PushMessage {
                interactions: vec![interaction.clone()],
                ..empty_push(self.my_master_pubkey().await)
            })
            .await;
        }
        Ok(())
    }

    /// Record the deletion of the local user's interaction `id` as a tombstone and
    /// broadcast it on the own feed topic.
    ///
    /// `PushMessage` has no field for interaction deletions. Without an own feed topic
    /// nothing is sent; a follower removes the interaction at its next sync, when the
    /// responder's interaction set no longer contains it.
    pub async fn broadcast_delete_interaction(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> Result<(), AppError> {
        self.storage
            .record_interaction_tombstone(author, id, now_millis())
            .await?;

        let msg = GossipMessage::DeleteInteraction {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self
            .broadcast_msg(&msg, &format!("delete interaction {}", short_id(id)))
            .await?
        {
            log::debug!(
                "[gossip] delete interaction {}: no own feed topic, followers remove it at their next sync",
                short_id(id)
            );
        }
        Ok(())
    }

    /// Send the signed device list to followers: on the own feed topic, or by push if no
    /// topic is open.
    pub async fn broadcast_linked_devices(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::LinkedDevices(announcement.clone());
        if !self
            .broadcast_msg(
                &msg,
                &format!("device announcement v{}", announcement.version),
            )
            .await?
        {
            self.attempt_push(PushMessage {
                announcement: Some(announcement.clone()),
                ..empty_push(announcement.master_pubkey.clone())
            })
            .await;
        }
        Ok(())
    }

    /// Announce a Stage room on the host's own user-feed gossip topic so followers discover
    /// it. Without a broadcast (the host is not Public), the announcement is pushed to the
    /// push recipients if `push` is set. The announcement is signed with the local master
    /// key; `host_pubkey` must be the local master pubkey or receivers reject it.
    pub async fn broadcast_stage_announcement(
        &self,
        stage_id: String,
        title: String,
        ticket: StageTicket,
        host_pubkey: String,
        started_at: u64,
        push: bool,
    ) -> Result<(), AppError> {
        let signature = {
            let id = self.identity.read().await;
            sign_stage_announcement(
                &stage_id,
                &title,
                &ticket,
                &host_pubkey,
                started_at,
                &id.master_key,
            )
        };
        let announcement = StageAnnouncement {
            stage_id,
            title,
            ticket,
            host_pubkey,
            started_at,
            signature,
        };
        let msg = GossipMessage::StageAnnouncement {
            stage_id: announcement.stage_id.clone(),
            title: announcement.title.clone(),
            ticket: announcement.ticket.clone(),
            host_pubkey: announcement.host_pubkey.clone(),
            started_at: announcement.started_at,
            signature: announcement.signature.clone(),
        };
        if !self.broadcast_msg(&msg, "stage announcement").await? && push {
            self.attempt_push(PushMessage {
                stage: Some(announcement),
                ..empty_push(self.my_master_pubkey().await)
            })
            .await;
        }
        Ok(())
    }

    /// Broadcast that a Stage room has ended on the host's own user-feed gossip topic, or
    /// push it to the push recipients without a broadcast. Signed with the local master key.
    pub async fn broadcast_stage_ended(&self, stage_id: String) -> Result<(), AppError> {
        let (host_pubkey, signature) = {
            let id = self.identity.read().await;
            (
                id.master_pubkey.clone(),
                sign_stage_ended(&stage_id, &id.master_pubkey, &id.master_key),
            )
        };
        let msg = GossipMessage::StageEnded {
            stage_id: stage_id.clone(),
            host_pubkey: host_pubkey.clone(),
            signature: signature.clone(),
        };
        if !self.broadcast_msg(&msg, "stage ended").await? {
            self.attempt_push(PushMessage {
                stage_ended: Some(StageEnd {
                    stage_id,
                    host_pubkey: host_pubkey.clone(),
                    signature,
                }),
                ..empty_push(host_pubkey)
            })
            .await;
        }
        Ok(())
    }
}
