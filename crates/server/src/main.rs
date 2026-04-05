use smart_rusty_pick_core::config::Config;
use smart_rusty_pick_core::db::Database;
use smart_rusty_pick_core::server;
use std::sync::{Arc, Mutex};

fn main() {
    let config = Config::load();

    let _ = config.cert_path.clone().expect("headless mode requires cert_path in config.toml");
    let _ = config.key_path.clone().expect("headless mode requires key_path in config.toml");
    let _ = config.ca_path.clone().expect("headless mode requires ca_path in config.toml");

    if let Err(e) = server::ensure_certificates(&config) {
        eprintln!("Failed to ensure certificates: {}", e);
    }

    // We use a directory "db_storage" to hold our tables
    let db = Arc::new(Mutex::new(Database::new("db_storage", Some(config.clone())).expect("Failed to initialize database")));

    let addr = config.server_addr.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = config.server_port.unwrap_or(8443);
    let full_addr = if addr.contains(':') { addr } else { format!("{}:{}", addr, port) };

    println!("Starting headless database service on {}...", full_addr);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        if let Err(e) = smart_rusty_pick_core::server::start_server(Arc::new(config), db, None).await {
            eprintln!("Server error: {}", e);
        }
    });
}
