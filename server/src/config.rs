use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_listen")]
    pub listen_addr: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Base URL clients use for this server (`http://` or `https://`). Registration
    /// requests are signed over this value. Required.
    pub public_url: String,
    #[serde(default = "default_true")]
    pub registration_open: bool,
    /// Take the client address for rate limiting from the last `X-Forwarded-For` entry.
    /// Set only when every request passes through a reverse proxy that appends it.
    #[serde(default)]
    pub trust_forwarded_for: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    /// Maximum number of active registrations.
    #[serde(default = "default_max_users")]
    pub max_registered_users: u32,
    /// Maximum number of posts stored per user.
    #[serde(default = "default_max_posts")]
    pub max_posts_per_user: u32,
    /// Maximum number of interactions stored per user.
    #[serde(default = "default_max_interactions")]
    pub max_interactions_per_user: u32,
    /// How long to keep posts and interactions (in days). 0 means keep forever.
    #[serde(default)]
    pub retention_days: u64,
    /// Days without an accepted signed request or completed sync after which a
    /// registration is deactivated. Clients send their profile at least weekly. 0 means
    /// never.
    #[serde(default = "default_registration_expiry_days")]
    pub registration_expiry_days: u64,
    /// Maximum number of open HTTP connections.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// Maximum number of HTTP requests handled at the same time.
    #[serde(default = "default_max_requests")]
    pub max_concurrent_requests: usize,
    /// Maximum number of requests to `/api/v1/register` per client address per minute.
    #[serde(default = "default_register_rate")]
    pub register_requests_per_minute: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    #[serde(default = "default_sync_interval")]
    pub interval_minutes: u64,
    #[serde(default = "default_true")]
    pub startup_sync: bool,
    /// Maximum number of peer syncs running at the same time.
    #[serde(default = "default_max_syncs")]
    pub max_concurrent_syncs: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrendingConfig {
    #[serde(default = "default_trending_interval")]
    pub recompute_interval_minutes: u64,
    #[serde(default = "default_trending_window")]
    pub window_hours: u64,
}

fn default_name() -> String {
    "Proscenium Gateway".to_string()
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
    10_000
}
fn default_registration_expiry_days() -> u64 {
    30
}
fn default_max_posts() -> u32 {
    10000
}
fn default_max_interactions() -> u32 {
    50000
}
fn default_max_connections() -> usize {
    1024
}
fn default_max_requests() -> usize {
    256
}
fn default_register_rate() -> u32 {
    10
}
fn default_sync_interval() -> u64 {
    5
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
            max_interactions_per_user: default_max_interactions(),
            retention_days: 0,
            registration_expiry_days: default_registration_expiry_days(),
            max_connections: default_max_connections(),
            max_concurrent_requests: default_max_requests(),
            register_requests_per_minute: default_register_rate(),
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

/// Normalize `url` as the client does before it signs a request: parse it (which lowercases
/// the scheme and host and drops a default port) and remove trailing `/`. A string that
/// does not parse is returned with trailing `/` removed.
pub fn normalize_url(url: &str) -> String {
    match url::Url::parse(url.trim()) {
        Ok(parsed) => parsed.as_str().trim_end_matches('/').to_string(),
        Err(_) => url.trim().trim_end_matches('/').to_string(),
    }
}

impl Config {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse and validate a TOML document. Unknown keys are errors.
    pub fn parse(content: &str) -> anyhow::Result<Self> {
        let mut config: Config = toml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&mut self) -> anyhow::Result<()> {
        let url = normalize_url(&self.server.public_url);
        let host = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .ok_or_else(|| {
                anyhow::anyhow!("server.public_url must start with http:// or https://")
            })?;
        if host.is_empty() || host.starts_with('/') || host.chars().any(char::is_whitespace) {
            anyhow::bail!("server.public_url has no host");
        }
        self.server.public_url = url;

        let at_least_one = [
            ("limits.max_connections", self.limits.max_connections as u64),
            (
                "limits.max_concurrent_requests",
                self.limits.max_concurrent_requests as u64,
            ),
            (
                "limits.register_requests_per_minute",
                u64::from(self.limits.register_requests_per_minute),
            ),
            (
                "sync.max_concurrent_syncs",
                self.sync.max_concurrent_syncs as u64,
            ),
            ("sync.interval_minutes", self.sync.interval_minutes),
            (
                "trending.recompute_interval_minutes",
                self.trending.recompute_interval_minutes,
            ),
            ("trending.window_hours", self.trending.window_hours),
        ];
        for (name, value) in at_least_one {
            if value == 0 {
                anyhow::bail!("{name} must be at least 1");
            }
        }
        Ok(())
    }

    /// The address the HTTP listener binds. `port` replaces only the port of
    /// `server.listen_addr`; the configured host is kept.
    pub fn listen_addr(&self, port: Option<u16>) -> anyhow::Result<String> {
        let Some(port) = port else {
            return Ok(self.server.listen_addr.clone());
        };
        let (host, _) =
            self.server.listen_addr.rsplit_once(':').ok_or_else(|| {
                anyhow::anyhow!("server.listen_addr must have the form host:port")
            })?;
        Ok(format!("{host}:{port}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_is_required_and_normalized() {
        assert!(Config::parse("[server]\n").is_err());
        assert!(Config::parse("[server]\npublic_url = \"example.test\"\n").is_err());
        assert!(Config::parse("[server]\npublic_url = \"https://\"\n").is_err());
        let config = Config::parse("[server]\npublic_url = \"https://example.test/\"\n").unwrap();
        assert_eq!(config.server.public_url, "https://example.test");
        assert_eq!(config.limits.max_registered_users, 10_000);
        assert_eq!(config.limits.registration_expiry_days, 30);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let doc = "[server]\npublic_url = \"https://example.test\"\nregistration_opne = false\n";
        assert!(Config::parse(doc).is_err());
        let doc = "[server]\npublic_url = \"https://example.test\"\n[limit]\nretention_days = 1\n";
        assert!(Config::parse(doc).is_err());
    }

    #[test]
    fn zero_limits_are_rejected() {
        let doc =
            "[server]\npublic_url = \"https://example.test\"\n[sync]\nmax_concurrent_syncs = 0\n";
        assert!(Config::parse(doc).is_err());
    }

    #[test]
    fn port_override_keeps_configured_host() {
        let doc =
            "[server]\npublic_url = \"https://example.test\"\nlisten_addr = \"127.0.0.1:3000\"\n";
        let config = Config::parse(doc).unwrap();
        assert_eq!(config.listen_addr(None).unwrap(), "127.0.0.1:3000");
        assert_eq!(config.listen_addr(Some(8080)).unwrap(), "127.0.0.1:8080");

        let doc = "[server]\npublic_url = \"https://example.test\"\nlisten_addr = \"[::1]:3000\"\n";
        let config = Config::parse(doc).unwrap();
        assert_eq!(config.listen_addr(Some(8080)).unwrap(), "[::1]:8080");
    }

    #[test]
    fn normalize_url_matches_the_client_normalization() {
        assert_eq!(normalize_url("https://a.test/"), "https://a.test");
        assert_eq!(normalize_url("https://a.test"), "https://a.test");
        assert_eq!(normalize_url("https://a.test//"), "https://a.test");
        assert_eq!(
            normalize_url("HTTPS://A.Test:443/base/"),
            "https://a.test/base"
        );
        assert_eq!(normalize_url(" http://a.test:8080 "), "http://a.test:8080");
    }
}
