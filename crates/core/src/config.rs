use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub editor: Option<String>,
    pub server_port: Option<u16>,
    pub cert_path: Option<String>,
    pub key_path: Option<String>,
    pub ca_path: Option<String>,
    pub server_addr: Option<String>,
    pub log_detail: Option<String>,
    pub max_log_records: Option<usize>,
    /// Target number of records per hashfile group. Lower means smaller, faster
    /// group rewrites but more files; higher means the opposite.
    pub records_per_group: Option<usize>,
    /// Flush every write to disk before acknowledging it. Safest, slowest.
    pub durable_writes: Option<bool>,
    /// How much of a flush is forced to the platter: `"always"`, `"meta"` or
    /// `"never"` (default). Files marked durable use `"always"` unless this is
    /// set explicitly.
    pub fsync: Option<String>,
    /// How long a change may stay in memory before being flushed.
    pub flush_interval_ms: Option<u64>,
    /// Flush once this many writes are pending, regardless of the interval.
    pub flush_max_pending: Option<usize>,
    /// Start the web management dashboard alongside the database server.
    /// Defaults to on; set `false` for a server that should expose nothing but
    /// the remote protocol.
    pub web_enabled: Option<bool>,
    /// Address the dashboard binds to (default `127.0.0.1`). The dashboard can
    /// authorize clients and hand out private keys, so it stays on the loopback
    /// interface unless this is changed deliberately.
    pub web_addr: Option<String>,
    /// Port the dashboard listens on (default 8080).
    pub web_port: Option<u16>,
    /// Fixed dashboard access token. Left unset, a new one is generated on every
    /// boot and printed with the dashboard URL.
    pub web_token: Option<String>,
}

/// Dashboard defaults, applied wherever the config leaves them unset.
pub const DEFAULT_WEB_ADDR: &str = "127.0.0.1";
pub const DEFAULT_WEB_PORT: u16 = 8080;

impl Config {
    /// Whether the dashboard should be started. On unless switched off.
    pub fn web_enabled(&self) -> bool {
        self.web_enabled.unwrap_or(true)
    }

    /// The `addr:port` the dashboard binds to.
    pub fn web_bind_addr(&self) -> String {
        let addr = self.web_addr.clone().unwrap_or_else(|| DEFAULT_WEB_ADDR.to_string());
        if addr.contains(':') {
            addr
        } else {
            format!("{}:{}", addr, self.web_port.unwrap_or(DEFAULT_WEB_PORT))
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_path = Path::new("config.toml");
        if config_path.exists() {
            if let Ok(content) = fs::read_to_string(config_path) {
                if let Ok(config) = toml::from_str::<Config>(&content) {
                    return config;
                }
            }
        }
        // Return default if file doesn't exist or is invalid
        Config {
            editor: Some("nano".to_string()),
            server_port: Some(8443),
            cert_path: None,
            key_path: None,
            ca_path: None,
            server_addr: Some("127.0.0.1".to_string()),
            log_detail: Some("normal".to_string()),
            max_log_records: Some(100),
            records_per_group: None,
            durable_writes: None,
            fsync: None,
            flush_interval_ms: None,
            flush_max_pending: None,
            web_enabled: None,
            web_addr: None,
            web_port: None,
            web_token: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Config {
        let mut config = Config::load();
        config.web_enabled = None;
        config.web_addr = None;
        config.web_port = None;
        config
    }

    #[test]
    fn dashboard_is_on_by_default_and_binds_to_loopback() {
        let config = empty();
        assert!(config.web_enabled());
        assert_eq!(config.web_bind_addr(), "127.0.0.1:8080");
    }

    #[test]
    fn dashboard_address_may_carry_its_own_port() {
        let mut config = empty();
        config.web_addr = Some("0.0.0.0:9000".to_string());
        config.web_port = Some(8080);
        assert_eq!(config.web_bind_addr(), "0.0.0.0:9000");
    }

    #[test]
    fn dashboard_can_be_switched_off() {
        let mut config = empty();
        config.web_enabled = Some(false);
        assert!(!config.web_enabled());
    }
}
