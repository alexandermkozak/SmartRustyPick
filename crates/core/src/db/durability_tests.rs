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
