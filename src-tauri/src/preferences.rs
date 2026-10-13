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

/// True unless the stored value is "0": the follow list is shared by default.
pub async fn get_share_follows(storage: &Storage) -> bool {
    storage
        .get_preference(SHARE_FOLLOWS)
        .await
        .ok()
        .flatten()
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// True unless the stored value is "0": the followers list is shared by default.
pub async fn get_share_followers(storage: &Storage) -> bool {
    storage
        .get_preference(SHARE_FOLLOWERS)
        .await
        .ok()
        .flatten()
        .map(|v| v != "0")
        .unwrap_or(true)
}
