use crate::constants::MAX_REMOTE_LIST_ENTRIES;
use crate::storage::RemoteListKind;
use iroh::{
    endpoint::{Connection, SendStream},
    protocol::AcceptError,
};
use proscenium_types::{FollowersListResponse, FollowsListResponse, PeerResponse, Visibility};

use super::PeerHandler;

impl PeerHandler {
    /// Answer a request for the local user's follow list or follower list.
    ///
    /// The list is returned only if the local visibility allows the requesting NodeId
    /// (Public: anyone, Listed: approved followers, Private: approved followers that the
    /// local user follows) and the matching share preference is on. Otherwise an empty
    /// list with `hidden = true` is returned. A failed visibility or follower lookup
    /// rejects the request. At most `MAX_REMOTE_LIST_ENTRIES` entries are sent.
    pub(super) async fn handle_list_request(
        &self,
        kind: RemoteListKind,
        my_pubkey: &str,
        remote_str: &str,
        send: SendStream,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        let visibility = self
            .storage
            .get_visibility(my_pubkey)
            .await
            .map_err(AcceptError::from_err)?;
        let allowed = if visibility == Visibility::Public {
            true
        } else {
            match self
                .storage
                .try_get_master_pubkey_for_transport(remote_str)
                .await
                .map_err(AcceptError::from_err)?
            {
                None => false,
                Some(remote_pubkey) => match visibility {
                    Visibility::Private => self.storage.is_mutual(my_pubkey, &remote_pubkey).await,
                    _ => self.storage.is_approved_follower(&remote_pubkey).await,
                }
                .map_err(AcceptError::from_err)?,
            }
        };

        let share = allowed
            && match kind {
                RemoteListKind::Follows => {
                    crate::preferences::get_share_follows(&self.storage).await
                }
                RemoteListKind::Followers => {
                    crate::preferences::get_share_followers(&self.storage).await
                }
            };

        let mut entries = if !share {
            Vec::new()
        } else {
            match kind {
                RemoteListKind::Follows => self.storage.get_follows(my_pubkey).await,
                RemoteListKind::Followers => self.storage.get_followers(my_pubkey).await,
            }
            .map_err(AcceptError::from_err)?
        };
        entries.truncate(MAX_REMOTE_LIST_ENTRIES);

        let response = match kind {
            RemoteListKind::Follows => PeerResponse::FollowsList(FollowsListResponse {
                pubkey: my_pubkey.to_string(),
                follows: entries,
                hidden: !share,
            }),
            RemoteListKind::Followers => PeerResponse::FollowersList(FollowersListResponse {
                pubkey: my_pubkey.to_string(),
                followers: entries,
                hidden: !share,
            }),
        };
        Self::respond(send, conn, &response).await
    }
}
