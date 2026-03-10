use crate::storage::Storage;
use iroh_social_types::{IdentityResponse, SigningKeyDelegation, now_millis};
use rusqlite::params;

impl Storage {
    /// Cache a peer's delegation and transport NodeIds from an IdentityResponse.
    pub fn cache_peer_identity(&self, response: &IdentityResponse) -> anyhow::Result<()> {
        let delegation_json = serde_json::to_string(&response.delegation)?;
        let transport_json = serde_json::to_string(&response.transport_node_ids)?;
        let now = now_millis() as i64;
        self.with_db(|db| {
            db.execute(
                "INSERT INTO peer_delegations (master_pubkey, signing_pubkey, delegation_json, transport_node_ids_json, cached_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(master_pubkey) DO UPDATE SET
                    signing_pubkey = ?2,
                    delegation_json = ?3,
                    transport_node_ids_json = ?4,
                    cached_at = ?5",
                params![
                    response.master_pubkey,
                    response.delegation.signing_pubkey,
                    delegation_json,
                    transport_json,
                    now,
                ],
            )?;
            Ok(())
        })
    }

    /// Get the cached delegation for a peer by their master pubkey.
    #[allow(dead_code)]
    pub fn get_peer_delegation(
        &self,
        master_pubkey: &str,
    ) -> anyhow::Result<Option<SigningKeyDelegation>> {
        self.with_db(|db| {
            let mut stmt = db
                .prepare("SELECT delegation_json FROM peer_delegations WHERE master_pubkey = ?1")?;
            let result = stmt.query_row(params![master_pubkey], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            });
            match result {
                Ok(json) => {
                    let delegation: SigningKeyDelegation = serde_json::from_str(&json)?;
                    Ok(Some(delegation))
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    /// Get cached transport NodeIds for a peer by their master pubkey.
    pub fn get_peer_transport_node_ids(&self, master_pubkey: &str) -> anyhow::Result<Vec<String>> {
        self.with_db(|db| {
            let mut stmt = db.prepare(
                "SELECT transport_node_ids_json FROM peer_delegations WHERE master_pubkey = ?1",
            )?;
            let result = stmt.query_row(params![master_pubkey], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            });
            match result {
                Ok(json) => {
                    let ids: Vec<String> = serde_json::from_str(&json)?;
                    Ok(ids)
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(vec![]),
                Err(e) => Err(e.into()),
            }
        })
    }

    /// Reverse lookup: find the master pubkey that has a given transport NodeId in its cached list.
    pub fn get_master_pubkey_for_transport(&self, transport_id: &str) -> Option<String> {
        self.with_db(|db| {
            let mut stmt =
                db.prepare("SELECT master_pubkey, transport_node_ids_json FROM peer_delegations")?;
            let rows = stmt.query_map([], |row| {
                let master: String = row.get(0)?;
                let json: String = row.get(1)?;
                Ok((master, json))
            })?;
            for row in rows.flatten() {
                if let Ok(ids) = serde_json::from_str::<Vec<String>>(&row.1)
                    && ids.iter().any(|id| id == transport_id)
                {
                    return Ok(Some(row.0));
                }
            }
            Ok(None)
        })
        .ok()
        .flatten()
    }

    /// Get the cached signing pubkey for a peer (the key that signs their content).
    #[allow(dead_code)]
    pub fn get_peer_signing_pubkey(&self, master_pubkey: &str) -> anyhow::Result<Option<String>> {
        self.with_db(|db| {
            let mut stmt =
                db.prepare("SELECT signing_pubkey FROM peer_delegations WHERE master_pubkey = ?1")?;
            let result = stmt.query_row(params![master_pubkey], |row| row.get(0));
            match result {
                Ok(pubkey) => Ok(Some(pubkey)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }
}
