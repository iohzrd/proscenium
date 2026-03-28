use iroh::{endpoint::Connection, protocol::AcceptError};
use proscenium_types::{FollowersListResponse, FollowsListResponse, PeerResponse, Visibility};

use super::PeerHandler;

impl PeerHandler {
    pub(super) async fn handle_follows_list_request(
        &self,
        my_pubkey: &str,
        remote_str: &str,
        mut send: iroh::endpoint::SendStream,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let remote_pubkey = self
            .storage
            .get_master_pubkey_for_transport(remote_str)
            .await
            .unwrap_or_else(|| remote_str.to_string());

        let visibility = self
            .storage
            .get_visibility(my_pubkey)
            .await
            .unwrap_or(Visibility::Public);
        let allowed = match visibility {
            Visibility::Public => true,
            Visibility::Listed => self
                .storage
                .is_follower(my_pubkey, &remote_pubkey)
                .await
                .unwrap_or(false),
            Visibility::Private => self
                .storage
                .is_mutual(my_pubkey, &remote_pubkey)
                .await
                .unwrap_or(false),
        };

        let share = allowed && crate::preferences::get_share_follows(&self.storage).await;

        let follows = if share {
            self.storage
                .get_follows(my_pubkey)
                .await
                .unwrap_or_default()
        } else {
            vec![]
        };

        let response = PeerResponse::FollowsList(FollowsListResponse {
            pubkey: my_pubkey.to_string(),
            follows,
            hidden: !share,
        });

        let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
        send.write_all(&resp_bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;
        conn.closed().await;
        Ok(())
    }

    pub(super) async fn handle_followers_list_request(
        &self,
        my_pubkey: &str,
        remote_str: &str,
        mut send: iroh::endpoint::SendStream,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let remote_pubkey = self
            .storage
            .get_master_pubkey_for_transport(remote_str)
            .await
            .unwrap_or_else(|| remote_str.to_string());

        let visibility = self
            .storage
            .get_visibility(my_pubkey)
            .await
            .unwrap_or(Visibility::Public);
        let allowed = match visibility {
            Visibility::Public => true,
            Visibility::Listed => self
                .storage
                .is_follower(my_pubkey, &remote_pubkey)
                .await
                .unwrap_or(false),
            Visibility::Private => self
                .storage
                .is_mutual(my_pubkey, &remote_pubkey)
                .await
                .unwrap_or(false),
        };

        let share = allowed && crate::preferences::get_share_followers(&self.storage).await;

        let followers = if share {
            self.storage
                .get_followers(my_pubkey)
                .await
                .unwrap_or_default()
        } else {
            vec![]
        };

        let response = PeerResponse::FollowersList(FollowersListResponse {
            pubkey: my_pubkey.to_string(),
            followers,
            hidden: !share,
        });

        let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
        send.write_all(&resp_bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;
        conn.closed().await;
        Ok(())
    }
}
