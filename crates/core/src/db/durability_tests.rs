use crate::db::engine::Database;
use crate::db::hashfile;
use crate::db::models::*;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

fn fresh_dir(name: &str) -> String {
    if Path::new(name).exists() {
        fs::remove_dir_all(name).unwrap();
    }
    fs::create_dir_all(name).unwrap();
    name.to_string()
}

fn record(value: &str) -> Record {
    Record::from_display_string(value)
}

fn open_account(base: &str, account: &str) -> Database {
    let mut db = Database::new(base, None).unwrap();
    db.create_account(account, Some(base)).unwrap();
    db.logto(account).unwrap();
    db.create_dir_file().unwrap();
    db
}

fn dir_flag(db: &mut Database, table: &str) -> String {
    db.get_table_mut("DIR").unwrap().records[table]
        .get_field_display_string(DIR_DURABLE_IDX)
}

fn on_disk_count(base: &str, table: &str) -> usize {
    let mut map = HashMap::new();
    hashfile::load(&format!("{}/{}/data", base, table), &mut map).unwrap();
    map.len()
}

#[test]
fn test_create_file_records_durable_flag_in_dir() {
    let base = fresh_dir("test_durability_flag");
    let mut db = open_account(&base, "DUR1");
    db.create_table("NORMAL").unwrap();
    db.create_table_durable("CRITICAL", true).unwrap();

    assert_eq!(dir_flag(&mut db, "CRITICAL"), "Y");
    assert_eq!(dir_flag(&mut db, "NORMAL"), "");
    assert!(db.is_table_durable("CRITICAL"));
    assert!(!db.is_table_durable("NORMAL"));
    // The flag is described in the DIR dictionary, so it shows up in listings.
    assert!(db.get_table_mut("DIR").unwrap().dictionary.contains_key("DURABLE"));

    drop(db);
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn test_dir_sync_preserves_durable_flag() {
    let base = fresh_dir("test_durability_sync");
    let mut db = open_account(&base, "DUR2");
    db.create_table_durable("CRITICAL", true).unwrap();

    // Creating another file rebuilds the whole listing; the flag is not derived
    // from the filesystem, so it has to survive that rebuild.
    db.create_table("OTHER").unwrap();
    db.sync_dir_file().unwrap();
    assert_eq!(dir_flag(&mut db, "CRITICAL"), "Y");
    assert!(db.is_table_durable("CRITICAL"));

    drop(db);
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn test_durable_flag_survives_reopen_and_can_be_cleared() {
    let base = fresh_dir("test_durability_reopen");
    {
        let mut db = open_account(&base, "DUR3");
        db.create_table_durable("CRITICAL", true).unwrap();
        db.save().unwrap();
    }

    let mut db = Database::new(&base, None).unwrap();
    db.logto("DUR3").unwrap();
    assert!(db.is_table_durable("CRITICAL"), "flag must be read back from DIR");

    db.set_table_durable("CRITICAL", false).unwrap();
    assert!(!db.is_table_durable("CRITICAL"));
    assert_eq!(dir_flag(&mut db, "CRITICAL"), "");

    drop(db);
    fs::remove_dir_all(&base).unwrap();
}

fn config_with_fsync(fsync: Option<&str>) -> crate::config::Config {
    let mut config = crate::config::Config::load();
    config.log_detail = Some("none".to_string());
    config.fsync = fsync.map(|v| v.to_string());
    config
}

#[test]
fn test_durable_writes_are_fsynced_unless_the_operator_says_otherwise() {
    let base = fresh_dir("test_durability_policy");

    // Out of the box a durable file is really synced, while an ordinary
    // buffered flush keeps the throughput it always had.
    let db = Database::new(&base, Some(config_with_fsync(None))).unwrap();
    assert_eq!(db.durable_fsync, hashfile::FsyncPolicy::Always);
    assert_eq!(db.fsync, hashfile::FsyncPolicy::Never);
    drop(db);

    // An explicit setting wins, including for durable files: that is the knob
    // for someone who knowingly trades the guarantee for speed.
    let db = Database::new(&base, Some(config_with_fsync(Some("meta")))).unwrap();
    assert_eq!(db.durable_fsync, hashfile::FsyncPolicy::Meta);
    assert_eq!(db.fsync, hashfile::FsyncPolicy::Meta);
    drop(db);

    // Nonsense falls back to the default rather than refusing to open.
    let db = Database::new(&base, Some(config_with_fsync(Some("sometimes")))).unwrap();
    assert_eq!(db.fsync, hashfile::FsyncPolicy::Never);

    drop(db);
    fs::remove_dir_all(&base).unwrap();
}

/// Set on the re-executed test binary to make it play the victim: write one
/// record to a durable file and then have itself SIGKILLed.
const KILL_CHILD_DIR: &str = "SRP_KILL9_CHILD_DIR";

/// Writes an acknowledged record to a durable file and never returns: the
/// process is killed with SIGKILL, so nothing is unwound, no destructor runs
/// and no further flush can happen. Whatever is on disk afterwards is exactly
/// what the acknowledged write left there.
fn write_then_die(base: &str) -> ! {
    let mut db = Database::new(base, None).unwrap();
    db.create_account("KILL", Some(base)).unwrap();
    db.logto("KILL").unwrap();
    db.create_dir_file().unwrap();
    db.create_table_durable("LEDGER", true).unwrap();

    // Buffering set so that only the durable flag can get this to disk.
    db.durable_writes = false;
    db.flush_max_pending = 1_000_000;
    db.flush_interval = std::time::Duration::from_secs(3_600);

    db.get_table_mut("LEDGER").unwrap().insert_record("K1", record("ACKED"));
    db.note_write_for("KILL", "LEDGER").unwrap();

    let pid = std::process::id().to_string();
    std::process::Command::new("kill").args(["-9", &pid]).status().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(30));
    unreachable!("the process should have been killed");
}

#[test]
fn test_durable_write_survives_sigkill() {
    // The child is this very test binary, re-executed with only this test
    // selected, so the victim runs the real engine rather than a stand-in.
    if let Ok(base) = std::env::var(KILL_CHILD_DIR) {
        write_then_die(&base);
    }

    let base = fresh_dir("test_durability_kill9");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "db::durability_tests::test_durable_write_survives_sigkill", "--nocapture"])
        .env(KILL_CHILD_DIR, &base)
        .status()
        .unwrap();
    assert!(status.code().is_none(), "the child should have died from a signal, not exited: {:?}", status);

    // Nothing acknowledged may be lost, and nothing half-written may be left.
    let dir = hashfile::section_dir(&format!("{}/LEDGER/data", base));
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "a crash left a temporary file behind: {:?}", leftovers);

    let mut db = Database::new(&base, None).unwrap();
    db.logto("KILL").unwrap();
    let table = db.get_table_mut("LEDGER").unwrap();
    assert_eq!(table.records.len(), 1, "the acknowledged write did not survive the kill");
    assert_eq!(table.records["K1"], record("ACKED"));

    drop(db);
    fs::remove_dir_all(&base).unwrap();
}

#[test]
fn test_durable_file_flushes_while_others_stay_buffered() {
    let base = fresh_dir("test_durability_flush");
    let mut db = open_account(&base, "DUR4");
    db.create_table("NORMAL").unwrap();
    db.create_table_durable("CRITICAL", true).unwrap();

    // Buffering configured so that nothing flushes on its own.
    db.durable_writes = false;
    db.flush_max_pending = 1_000;
    db.flush_interval = std::time::Duration::from_secs(3_600);
    db.save().unwrap();

    db.get_table_mut("NORMAL").unwrap().insert_record("K1", record("V1"));
    db.note_write_for("DUR4", "NORMAL").unwrap();
    assert!(db.has_pending_writes(), "a normal file should still be buffered");
    assert_eq!(on_disk_count(&base, "NORMAL"), 0);

    db.get_table_mut("CRITICAL").unwrap().insert_record("K1", record("V1"));
    db.note_write_for("DUR4", "CRITICAL").unwrap();
    assert!(!db.has_pending_writes(), "a durable file must flush before acknowledging");
    assert_eq!(on_disk_count(&base, "CRITICAL"), 1);

    drop(db);
    fs::remove_dir_all(&base).unwrap();
}
