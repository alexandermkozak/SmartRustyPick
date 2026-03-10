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
        }
    }
}
