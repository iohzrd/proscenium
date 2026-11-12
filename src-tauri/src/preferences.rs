use crate::storage::Storage;

pub const MDNS_DISCOVERY: &str = "mdns_discovery";
pub const DHT_DISCOVERY: &str = "dht_discovery";
pub const SHARE_FOLLOWS: &str = "share_follows";
pub const SHARE_FOLLOWERS: &str = "share_followers";
pub const AUDIO_INPUT_DEVICE: &str = "audio_input_device";
pub const AUDIO_OUTPUT_DEVICE: &str = "audio_output_device";
pub const ACCENT_COLOR: &str = "accent_color";

pub async fn get_mdns_discovery(storage: &Storage) -> bool {
    storage
        .get_bool_preference(MDNS_DISCOVERY)
        .await
        .unwrap_or(false)
}

pub async fn get_dht_discovery(storage: &Storage) -> bool {
    storage
        .get_bool_preference(DHT_DISCOVERY)
        .await
        .unwrap_or(false)
}

/// True unless the stored value is "0" or the preference cannot be read: a list is
/// shared by default, and not shared when its setting is unknown.
async fn get_share_preference(storage: &Storage, key: &str) -> bool {
    match storage.get_preference(key).await {
        Ok(value) => value.is_none_or(|v| v != "0"),
        Err(e) => {
            log::error!("[preferences] failed to read {key}: {e}");
            false
        }
    }
}

/// See `get_share_preference`.
pub async fn get_share_follows(storage: &Storage) -> bool {
    get_share_preference(storage, SHARE_FOLLOWS).await
}

/// See `get_share_preference`.
pub async fn get_share_followers(storage: &Storage) -> bool {
    get_share_preference(storage, SHARE_FOLLOWERS).await
}
