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
        }
    }
}
