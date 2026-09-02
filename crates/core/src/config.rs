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
    /// How many files may be held in memory at once. Each is locked
    /// individually, so a larger cache is what lets writers to different files
    /// run in parallel instead of taking turns being loaded and evicted.
    pub max_loaded_tables: Option<usize>,
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
    /// Maximum size, in bytes, of a single request line. A client that streams
    /// bytes without a newline is cut off once it crosses this, instead of
    /// growing the read buffer without bound.
    pub max_request_bytes: Option<usize>,
    /// Maximum time allowed to complete the TLS handshake before the connection
    /// is dropped.
    pub handshake_timeout_ms: Option<u64>,
    /// Maximum time a connection may sit idle, with no request in flight,
    /// before it is closed. `0` disables the idle timeout.
    pub idle_timeout_ms: Option<u64>,
    /// Maximum number of connections the server holds open at once. Additional
    /// connections are rejected until one of the existing ones closes.
    pub max_connections: Option<usize>,
}

/// A single misbehaving (or compromised) authorised client should not be able to
/// take the server down for everyone; these are the connection-level defaults
/// that contain that damage. See `docs/admin_commands.md`'s server section and
/// the README's configuration table for what each one guards against.
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 1024 * 1024; // 1 MiB
pub const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 0; // disabled
pub const DEFAULT_MAX_CONNECTIONS: usize = 1024;

/// Files kept in memory at once. Generous, because eviction is what forces two
/// connections working on different files to interfere with each other, and a
/// cached table is only as large as the records that have been read into it.
pub const DEFAULT_MAX_LOADED_TABLES: usize = 64;

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

    pub fn max_request_bytes(&self) -> usize {
        self.max_request_bytes.unwrap_or(DEFAULT_MAX_REQUEST_BYTES)
    }

    pub fn handshake_timeout_ms(&self) -> u64 {
        self.handshake_timeout_ms.unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS)
    }

    /// `None` means disabled (config value `0`).
    pub fn idle_timeout(&self) -> Option<std::time::Duration> {
        match self.idle_timeout_ms.unwrap_or(DEFAULT_IDLE_TIMEOUT_MS) {
            0 => None,
            ms => Some(std::time::Duration::from_millis(ms)),
        }
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS)
    }
}

/// The settings a fresh installation runs with.
///
/// Written once, here, so that every other place needing a configuration -
/// `load`'s fallback, the benchmarks, a test that wants one setting changed -
/// says `..Config::default()` instead of listing every field. Adding a setting
/// is then a single line in the struct above, rather than a compile error in
/// each copy of the list.
impl Default for Config {
    fn default() -> Self {
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
            max_loaded_tables: None,
            durable_writes: None,
            fsync: None,
            flush_interval_ms: None,
            flush_max_pending: None,
            web_enabled: None,
            web_addr: None,
            web_port: None,
            web_token: None,
            max_request_bytes: None,
            handshake_timeout_ms: None,
            idle_timeout_ms: None,
            max_connections: None,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_path = Path::new("config.toml");
        if config_path.exists()
            && let Ok(content) = fs::read_to_string(config_path)
            && let Ok(config) = toml::from_str::<Config>(&content)
        {
            return config;
        }
        // No file, or one that does not parse: run on the defaults rather than
        // refusing to start.
        Config::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration with nothing set, whatever the working directory's
    /// `config.toml` happens to say.
    fn empty() -> Config {
        Config::default()
    }

    #[test]
    fn every_setting_is_optional() {
        // `Config::default()` is what a missing or unparsable file falls back
        // to, and what the benchmarks build on: it has to be usable as it is.
        let config = Config::default();
        assert_eq!(config.server_port, Some(8443));
        assert_eq!(config.server_addr.as_deref(), Some("127.0.0.1"));
        assert!(config.cert_path.is_none(), "TLS is opt in");
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

    #[test]
    fn connection_limits_have_sane_defaults() {
        let config = empty();
        assert_eq!(config.max_request_bytes(), 1024 * 1024);
        assert_eq!(config.handshake_timeout_ms(), 10_000);
        assert_eq!(config.idle_timeout(), None, "disabled unless configured");
        assert_eq!(config.max_connections(), 1024);
    }

    #[test]
    fn idle_timeout_zero_means_disabled() {
        let mut config = empty();
        config.idle_timeout_ms = Some(0);
        assert_eq!(config.idle_timeout(), None);

        config.idle_timeout_ms = Some(500);
        assert_eq!(config.idle_timeout(), Some(std::time::Duration::from_millis(500)));
    }
}
