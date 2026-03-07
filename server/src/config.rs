use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub trending: TrendingConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_listen")]
    pub listen_addr: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    pub public_url: String,
    #[serde(default = "default_true")]
    pub registration_open: bool,
}

#[derive(Debug, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_max_users")]
    pub max_registered_users: u32,
    #[serde(default = "default_max_posts")]
    pub max_posts_per_user: u32,
}

#[derive(Debug, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "default_sync_interval")]
    pub interval_minutes: u64,
    #[serde(default = "default_true")]
    pub startup_sync: bool,
    #[serde(default = "default_max_syncs")]
    pub max_concurrent_syncs: usize,
}

#[derive(Debug, Deserialize)]
pub struct TrendingConfig {
    #[serde(default = "default_trending_interval")]
    pub recompute_interval_minutes: u64,
    #[serde(default = "default_trending_window")]
    pub window_hours: u64,
}

fn default_name() -> String {
    "Iroh Social Server".to_string()
}
fn default_listen() -> String {
    "0.0.0.0:3000".to_string()
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}
fn default_true() -> bool {
    true
}
fn default_max_users() -> u32 {
    1000
}
fn default_max_posts() -> u32 {
    10000
}
fn default_sync_interval() -> u64 {
    15
}
fn default_max_syncs() -> usize {
    10
}
fn default_trending_interval() -> u64 {
    5
}
fn default_trending_window() -> u64 {
    24
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_registered_users: default_max_users(),
            max_posts_per_user: default_max_posts(),
        }
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            interval_minutes: default_sync_interval(),
            startup_sync: true,
            max_concurrent_syncs: default_max_syncs(),
        }
    }
}

impl Default for TrendingConfig {
    fn default() -> Self {
        Self {
            recompute_interval_minutes: default_trending_interval(),
            window_hours: default_trending_window(),
        }
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
