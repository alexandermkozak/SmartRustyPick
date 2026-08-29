#![allow(dead_code)]

use smart_rusty_pick_core::config::Config;
use smart_rusty_pick_core::db::engine::Database;
use smart_rusty_pick_core::db::models::{Field, Record, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const ACCOUNT: &str = "BENCH";

/// A temporary storage directory that removes itself when dropped, so benches never
/// write into the repository working copy.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut path = std::env::temp_dir();
        path.push(format!("srp_bench_{}_{}_{}", tag, std::process::id(), nanos));
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

/// Config that never touches the working directory's `config.toml`.
///
/// `fsync` defaults to `never` so the numbers stay comparable with the ones
/// measured before group syncing existed; set `SRP_BENCH_FSYNC=always|meta` to
/// measure what the durability guarantee costs.
pub fn bench_config() -> Config {
    // Only the settings a benchmark cares about are named; the rest come from
    // `Config::default()`, so a new setting cannot break this file.
    Config {
        log_detail: Some("none".to_string()),
        max_log_records: Some(10),
        durable_writes: Some(true),
        fsync: Some(std::env::var("SRP_BENCH_FSYNC").unwrap_or_else(|_| "never".to_string())),
        // A benchmark measures the engine, so it starts no listeners.
        web_enabled: Some(false),
        ..Config::default()
    }
}

/// A `Database` rooted in `dir` with the bench account created and logged into.
pub fn new_db(dir: &str) -> Database {
    let mut db = Database::new(dir, Some(bench_config())).unwrap();
    if db.get_account_dir(ACCOUNT).is_none() {
        db.create_account(ACCOUNT, None).unwrap();
    }
    db.logto(ACCOUNT).unwrap();
    db
}

pub fn field(value: &str) -> Field {
    Field { values: vec![Value { sub_values: vec![value.to_string()] }] }
}

/// Dictionary entry mapping `name` to the 1-based attribute `index`.
pub fn dict_entry(name: &str, index: usize) -> Record {
    let mut rec = Record::new();
    rec.fields.push(field(&index.to_string()));
    rec.fields.push(field(name));
    rec
}

/// Record shaped `NAME^CITY^AMOUNT`, where `CITY` cycles over ten values.
pub fn sample_record(i: usize) -> Record {
    let mut rec = Record::new();
    rec.fields.push(field(&format!("NAME{i}")));
    rec.fields.push(field(&format!("CITY{}", i % 10)));
    rec.fields.push(field(&format!("{}", i % 1000)));
    rec
}

/// Creates `TABLE` with a `NAME`/`CITY`/`AMOUNT` dictionary and `count` sample records.
pub fn build_table(db: &mut Database, table_name: &str, count: usize) {
    db.create_table(table_name).unwrap();
    let table = db.get_table_mut(table_name).unwrap();
    table.dictionary.insert("NAME".to_string(), dict_entry("NAME", 1));
    table.dictionary.insert("CITY".to_string(), dict_entry("CITY", 2));
    table.dictionary.insert("AMOUNT".to_string(), dict_entry("AMOUNT", 3));
    for i in 0..count {
        table.records.insert(format!("K{i:06}"), sample_record(i));
    }
    table.touch_all();
}
