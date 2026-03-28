use crate::error::AppError;
use bytes::Bytes;
use proscenium_types::{
    GossipMessage, Interaction, LinkedDevicesAnnouncement, Post, Profile, PushMessage,
    SigningKeyRotation, StageTicket,
};

impl super::GossipService {
    /// Serialize and broadcast a gossip message if we have an active sender.
    /// Returns `true` if the message was broadcast, `false` if no sender is active.
    pub(super) async fn broadcast_msg(
        &self,
        msg: &GossipMessage,
        label: &str,
    ) -> Result<bool, AppError> {
        let sender = self.inner.lock().await.my_sender.clone();
        if let Some(sender) = sender {
            let payload = serde_json::to_vec(msg)?;
            sender.broadcast(Bytes::from(payload)).await?;
            log::info!("[gossip] broadcast {label}");
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn broadcast_heartbeat(&self) -> Result<(), AppError> {
        self.broadcast_msg(&GossipMessage::Heartbeat, "heartbeat")
            .await?;
        Ok(())
    }

    pub async fn broadcast_profile(&self, profile: &Profile) -> Result<(), AppError> {
        let msg = GossipMessage::ProfileUpdate(profile.clone());
        if !self
            .broadcast_msg(&msg, &format!("profile: {}", profile.display_name))
            .await?
        {
            let my_id = self.identity.read().await.master_pubkey.clone();
            self.attempt_push(PushMessage {
                author: my_id,
                posts: vec![],
                interactions: vec![],
                profile: Some(profile.clone()),
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_post(&self, post: &Post) -> Result<(), AppError> {
        let msg = GossipMessage::NewPost(post.clone());
        if !self
            .broadcast_msg(&msg, &format!("post {}", &post.id))
            .await?
        {
            let my_id = self.identity.read().await.master_pubkey.clone();
            self.attempt_push(PushMessage {
                author: my_id,
                posts: vec![post.clone()],
                interactions: vec![],
                profile: None,
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_delete(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::DeletePost {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self.broadcast_msg(&msg, &format!("delete {id}")).await? {
            log::debug!("[gossip] delete post {id}: no gossip sender, peers will sync");
        }
        Ok(())
    }

    pub async fn broadcast_interaction(&self, interaction: &Interaction) -> Result<(), AppError> {
        let msg = GossipMessage::NewInteraction(interaction.clone());
        let label = format!(
            "{:?} on post {}",
            interaction.kind, &interaction.target_post_id
        );
        if !self.broadcast_msg(&msg, &label).await? {
            let my_id = self.identity.read().await.master_pubkey.clone();
            self.attempt_push(PushMessage {
                author: my_id,
                posts: vec![],
                interactions: vec![interaction.clone()],
                profile: None,
            })
            .await;
        }
        Ok(())
    }

    pub async fn broadcast_delete_interaction(
        &self,
        id: &str,
        author: &str,
        signature: &str,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::DeleteInteraction {
            id: id.to_string(),
            author: author.to_string(),
            signature: signature.to_string(),
        };
        if !self
            .broadcast_msg(&msg, &format!("delete interaction {id}"))
            .await?
        {
            log::debug!("[gossip] delete interaction {id}: no gossip sender, peers will sync");
        }
        Ok(())
    }

    pub async fn broadcast_linked_devices(
        &self,
        announcement: &LinkedDevicesAnnouncement,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::LinkedDevices(announcement.clone());
        self.broadcast_msg(
            &msg,
            &format!("device announcement v{}", announcement.version),
        )
        .await?;
        Ok(())
    }

    pub async fn broadcast_signing_key_rotation(
        &self,
        rotation: &SigningKeyRotation,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::SigningKeyRotation(rotation.clone());
        self.broadcast_msg(
            &msg,
            &format!("signing key rotation to index {}", rotation.new_key_index),
        )
        .await?;
        Ok(())
    }

    /// Announce a new Stage room on the host's own user-feed gossip topic so followers discover it.
    pub async fn broadcast_stage_announcement(
        &self,
        stage_id: String,
        title: String,
        ticket: StageTicket,
        host_pubkey: String,
        started_at: u64,
    ) -> Result<(), AppError> {
        let msg = GossipMessage::StageAnnouncement {
            stage_id,
            title,
            ticket,
            host_pubkey,
            started_at,
        };
        self.broadcast_msg(&msg, "stage announcement").await?;
        Ok(())
    }

    /// Broadcast that a Stage room has ended on the host's own user-feed gossip topic.
    pub async fn broadcast_stage_ended(&self, stage_id: String) -> Result<(), AppError> {
        let msg = GossipMessage::StageEnded { stage_id };
        self.broadcast_msg(&msg, "stage ended").await?;
        Ok(())
    }
}
