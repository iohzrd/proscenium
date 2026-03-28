use crate::error::AppError;
use iroh::{endpoint::Connection, protocol::AcceptError};
use proscenium_types::{LinkQrPayload, PeerResponse, derive_transport_key, now_millis, short_id};
use std::sync::Arc;
use tauri::Emitter;

use super::PeerHandler;

/// Active device-linking session on the existing device.
/// Created when the user initiates "Link New Device", consumed when a new device connects.
pub(super) struct PendingLink {
    pub(super) psk: [u8; 32],
    pub(super) x25519_private: [u8; 32],
    pub(super) expires_at: u64,
    pub(super) transfer_master_key: bool,
}

pub(super) type PendingLinkState = Arc<tokio::sync::Mutex<Option<PendingLink>>>;

pub(super) const LINK_SESSION_TTL_MS: u64 = 60_000; // 60 seconds

impl PeerHandler {
    /// Begin a device-link session. Returns the QR payload the initiating device displays.
    /// `relay_url` should come from `endpoint.addr().relay_urls().next()`.
    pub async fn start_link_session(
        &self,
        transfer_master_key: bool,
        relay_url: Option<String>,
    ) -> Result<LinkQrPayload, AppError> {
        let mut psk = [0u8; 32];
        getrandom::fill(&mut psk)?;

        let (master_secret_key_bytes, transport_node_id) = {
            let id = self.identity.read().await;
            (id.master_secret_key_bytes, id.transport_node_id.clone())
        };

        let transport_secret_bytes = derive_transport_key(&master_secret_key_bytes, 0);
        let x25519_private = crate::crypto::ed25519_secret_to_x25519(&transport_secret_bytes);

        let expires_at = now_millis() + LINK_SESSION_TTL_MS;

        {
            let mut lock = self.pending_link.lock().await;
            *lock = Some(PendingLink {
                psk,
                x25519_private,
                expires_at,
                transfer_master_key,
            });
        }

        use base64::Engine;
        let payload = LinkQrPayload {
            node_id: transport_node_id,
            secret: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(psk),
            relay_url,
        };

        log::info!("[link] started device link session (expires in 60s)");
        Ok(payload)
    }

    /// Cancel an active device-link session.
    pub async fn cancel_link_session(&self) {
        let mut lock = self.pending_link.lock().await;
        *lock = None;
        log::info!("[link] cancelled device link session");
    }

    pub(super) async fn handle_link_request(
        &self,
        mut send: iroh::endpoint::SendStream,
        noise_init: Vec<u8>,
        conn: &Connection,
    ) -> Result<(), AcceptError> {
        // Take the pending link session (consume it - one-time use)
        let link_session = {
            let mut lock = self.pending_link.lock().await;
            lock.take()
        };

        let link_session = match link_session {
            Some(session) => {
                // Check expiry
                if now_millis() > session.expires_at {
                    log::warn!("[link] pending link session expired");
                    return Err(AcceptError::from_err(std::io::Error::other(
                        "link session expired",
                    )));
                }
                session
            }
            None => {
                log::warn!("[link] no pending link session");
                return Err(AcceptError::from_err(std::io::Error::other(
                    "no pending link session",
                )));
            }
        };

        // Perform Noise IK+PSK handshake (responder side)
        let (mut transport, noise_response) = crate::crypto::noise_psk_respond(
            &link_session.x25519_private,
            &link_session.psk,
            &noise_init,
        )
        .map_err(|e| {
            log::error!("[link] noise handshake failed: {e}");
            AcceptError::from_err(std::io::Error::other(format!(
                "noise handshake failed: {e}"
            )))
        })?;

        // Derive a transport key for the new device
        let new_device_index = self.storage.next_device_index().await.map_err(|e| {
            AcceptError::from_err(std::io::Error::other(format!(
                "failed to get next device index: {e}"
            )))
        })?;

        let (
            master_secret_key_bytes,
            master_pubkey,
            signing_secret_key_bytes,
            dm_secret_key_bytes,
            delegation,
        ) = {
            let id = self.identity.read().await;
            (
                id.master_secret_key_bytes,
                id.master_pubkey.clone(),
                id.signing_secret_key_bytes,
                id.dm_secret_key_bytes,
                id.delegation.clone(),
            )
        };

        let new_transport_key_bytes =
            derive_transport_key(&master_secret_key_bytes, new_device_index);

        // Build the link bundle
        let master_key_to_send = if link_session.transfer_master_key {
            Some(&master_secret_key_bytes)
        } else {
            None
        };

        let bundle = self
            .storage
            .export_link_bundle(
                &master_pubkey,
                &signing_secret_key_bytes,
                &dm_secret_key_bytes,
                &delegation,
                &new_transport_key_bytes,
                new_device_index,
                master_key_to_send,
            )
            .await
            .map_err(|e| {
                AcceptError::from_err(std::io::Error::other(format!(
                    "failed to export link bundle: {e}"
                )))
            })?;

        // Serialize and encrypt the bundle with Noise transport
        let bundle_json = serde_json::to_vec(&bundle).map_err(AcceptError::from_err)?;
        let encrypted_bundle = crate::crypto::noise_transport_encrypt(&mut transport, &bundle_json)
            .map_err(|e| {
                AcceptError::from_err(std::io::Error::other(format!("noise encrypt failed: {e}")))
            })?;

        // Send the response
        let response = PeerResponse::LinkBundle {
            noise_response,
            encrypted_bundle,
        };
        let resp_bytes = serde_json::to_vec(&response).map_err(AcceptError::from_err)?;
        send.write_all(&resp_bytes)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish().map_err(AcceptError::from_err)?;

        log::info!("[link] sent link bundle to new device");
        let _ = self.app_handle.emit("device-link-progress", "bundle_sent");

        // Register the new device and broadcast updated announcement
        let new_transport_secret = iroh::SecretKey::from_bytes(&new_transport_key_bytes);
        let new_transport_node_id = new_transport_secret.public().to_string();

        let device_name = format!("Device {}", new_device_index);
        let now = now_millis();
        if let Err(e) = self
            .storage
            .upsert_linked_device(&new_transport_node_id, &device_name, false, false, now)
            .await
        {
            log::error!("[link] failed to register new device: {e}");
        } else {
            log::info!(
                "[link] registered new device {} (index={})",
                short_id(&new_transport_node_id),
                new_device_index
            );
        }

        // Build and broadcast updated announcement with all devices
        if let Ok(all_devices) = self.storage.get_linked_devices().await {
            let signing_sk = iroh::SecretKey::from_bytes(&signing_secret_key_bytes);
            let mut announcement = proscenium_types::LinkedDevicesAnnouncement {
                master_pubkey: master_pubkey.clone(),
                delegation: delegation.clone(),
                devices: all_devices,
                version: (new_device_index + 1) as u64,
                timestamp: now,
                signature: String::new(),
            };
            proscenium_types::sign_linked_devices_announcement(&mut announcement, &signing_sk);

            if let Err(e) = self.gossip.broadcast_linked_devices(&announcement).await {
                log::error!("[link] failed to broadcast device announcement: {e}");
            } else {
                log::info!("[link] broadcast updated device announcement");
            }
        }

        conn.closed().await;
        Ok(())
    }
}
