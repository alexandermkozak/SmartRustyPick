//! Shared fixtures for benches and unit tests: an isolated, self-cleaning
//! storage directory and a `Config` that never reads the working directory's
//! `config.toml`.
//!
//! Promoted from the Criterion benches (`benches/common/mod.rs`) so unit
//! tests get the same guarantees benches already had: a directory unique
//! enough that tests running in parallel never collide, and cleanup that
//! survives a panicking test because it lives in `Drop` rather than at the
//! end of the test body.
//!
//! Always compiled (not `#[cfg(test)]`) because the benches depend on this
//! crate as an ordinary library and cannot see anything gated on the crate's
//! own test configuration.

use crate::config::Config;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the OS temp dir that removes itself, and everything
/// under it, when dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a fresh, uniquely named directory. `label` identifies it in a
    /// directory listing while a test is being debugged; it has no effect on
    /// uniqueness, which comes from the process id, a timestamp and a
    /// per-process counter.
    pub fn new(label: &str) -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!("srp_{}_{}_{}_{}", label, std::process::id(), nanos, seq));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    pub fn path(&self) -> &str {
        self.path.to_str().unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A configuration that never reads the working directory's `config.toml`, so
/// a test or bench behaves the same wherever `cargo test`/`cargo bench` is
/// invoked from.
pub fn isolated_config() -> Config {
    Config {
        web_enabled: Some(false),
        ..Config::default()
    }
}
