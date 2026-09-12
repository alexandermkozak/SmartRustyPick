use smart_rusty_pick_core::config::Config;
use smart_rusty_pick_core::db::Database;
use smart_rusty_pick_core::server;
use std::sync::{Arc, RwLock};

fn main() {
    let config = Config::load();

    let _ = config
        .cert_path
        .clone()
        .expect("headless mode requires cert_path in config.toml");
    let _ = config
        .key_path
        .clone()
        .expect("headless mode requires key_path in config.toml");
    let _ = config
        .ca_path
        .clone()
        .expect("headless mode requires ca_path in config.toml");

    if let Err(e) = server::ensure_certificates(&config) {
        eprintln!("Failed to ensure certificates: {}", e);
    }

    // We use a directory "db_storage" to hold our tables.
    //
    // Opened rather than unwrapped: the one failure worth spelling out here is
    // a storage directory written in a format this build does not understand,
    // which is what an operator meets after swapping the image over a mounted
    // volume. A panic would bury the explanation under a backtrace and a "the
    // program panicked" line that reads like a bug in the server rather than a
    // decision it made. See `docs/deployment.md`.
    let db = match Database::new("db_storage", Some(config.clone())) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };
    // Said once, on the starts where something happened to the directory, and
    // not at all on the ordinary ones.
    if let Some(note) = db.storage_format_note() {
        println!("{}", note);
    }
    let db = Arc::new(RwLock::new(db));

    let addr = config.server_addr.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = config.server_port.unwrap_or(8443);
    let full_addr = if addr.contains(':') {
        addr
    } else {
        format!("{}:{}", addr, port)
    };

    println!("Starting headless database service on {}...", full_addr);
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let serving = smart_rusty_pick_core::server::start_server(Arc::new(config), db.clone(), None);
        tokio::select! {
            result = serving => {
                if let Err(e) = result {
                    eprintln!("Server error: {}", e);
                }
            }
            _ = shutdown_signal() => {
                println!("Shutting down, flushing pending writes...");
            }
        }
        // Writes are buffered in memory between flushes, so a shutdown must
        // persist whatever has not been written out yet.
        // Flushing needs no more than a shared borrow now that each file
        // carries its own lock, so a shutdown does not queue behind a request.
        if let Ok(db_lock) = db.read()
            && let Err(e) = db_lock.save()
        {
            eprintln!("Failed to flush on shutdown: {}", e);
        }
    });
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
