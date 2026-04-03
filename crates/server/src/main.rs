use smart_rusty_pick_core::config::Config;
use smart_rusty_pick_core::db::Database;
use smart_rusty_pick_core::server;
use std::sync::{Arc, Mutex};

fn main() {
    let config = Config::load();

    let cert_path = config.cert_path.clone().expect("headless mode requires cert_path in config.toml");
    let key_path = config.key_path.clone().expect("headless mode requires key_path in config.toml");
    let ca_path = config.ca_path.clone().expect("headless mode requires ca_path in config.toml");

    if let Err(e) = server::ensure_certificates(&config) {
        eprintln!("Failed to ensure certificates: {}", e);
    }

    // We use a directory "db_storage" to hold our tables
    let db = Arc::new(Mutex::new(Database::new("db_storage").expect("Failed to initialize database")));

    let addr = config.server_addr.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = config.server_port.unwrap_or(8443);
    let full_addr = if addr.contains(':') { addr } else { format!("{}:{}", addr, port) };

    println!("Starting headless database service on {}...", full_addr);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        if let Err(e) = server::run_server(&full_addr, db, &cert_path, &key_path, &ca_path).await {
            eprintln!("Server error: {}", e);
        }
    });
}
