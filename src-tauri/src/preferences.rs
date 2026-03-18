use crate::storage::Storage;

pub const MDNS_DISCOVERY: &str = "mdns_discovery";
pub const DHT_DISCOVERY: &str = "dht_discovery";

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
