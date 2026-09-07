//! Directory files end to end: what a key may be, what survives the round trip,
//! and the two properties the type exists for - the bytes are not framed, and
//! they are not held in memory.

use crate::db::directory;
use crate::db::engine::Database;
use crate::db::models::*;
use crate::db::{Change, ChangeOp, DbError};
use crate::test_support::{TempDir, isolated_config};
use std::path::Path;

/// Bytes an ordinary record cannot hold: all three marks, an embedded NUL, and
/// a lone `0xFF` that is not valid UTF-8. Every round-trip test uses these
/// rather than a friendly string, because a string proves nothing here.
const HOSTILE: &[u8] = &[0xFE, b'a', 0xFD, b'b', 0xFC, 0x00, 0xFF, 0xC3, b'z', b'\n'];

fn open_account(base: &str, account: &str) -> Database {
    let db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account(account, Some(base)).unwrap();
    db.logto(account).unwrap();
    db
}

fn directory_file(db: &Database, account: &str, name: &str) {
    db.create_table_with(
        account,
        name,
        FileAttributes {
            durable: false,
            queue: None,
            directory: Some(DirectoryPolicy::default_path()),
        },
    )
    .unwrap();
}

#[test]
fn arbitrary_bytes_round_trip_through_a_directory_file() {
    let dir = TempDir::new("directory_round_trip");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");

    db.write_directory_record("DIRS", "BLOBS", "scan.bin", HOSTILE).unwrap();
    let back = db.read_directory_record("DIRS", "BLOBS", "scan.bin").unwrap();

    // The whole point of the type. A hashed section splits on 0xFE, 0xFD and
    // 0xFC because those bytes *are* its structure; nothing here frames the
    // record at all, so nothing in it can be read as a separator.
    assert_eq!(back.as_deref(), Some(HOSTILE));
}

#[test]
fn a_record_is_exactly_the_bytes_of_its_host_file() {
    let dir = TempDir::new("directory_host_file");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "scan.bin", HOSTILE).unwrap();

    // Read with no help from the database: a directory file's promise is that
    // the record is the file, so the file is what is checked.
    let root = db.directory_root("DIRS", "BLOBS").unwrap();
    assert_eq!(std::fs::read(root.join("scan.bin")).unwrap(), HOSTILE);
}

#[test]
fn reading_a_record_does_not_put_the_file_in_the_table_cache() {
    let dir = TempDir::new("directory_uncached");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    for i in 0..8 {
        db.write_directory_record("DIRS", "BLOBS", &format!("r{i}"), &vec![b'x'; 4096])
            .unwrap();
    }
    db.clear_loaded_tables();

    for i in 0..8 {
        assert!(
            db.read_directory_record("DIRS", "BLOBS", &format!("r{i}"))
                .unwrap()
                .is_some(),
            "record r{i} should be readable"
        );
    }
    let _ = db.directory_records("DIRS", "BLOBS").unwrap();

    // The burden this file type removes: a hashed section reads every group of
    // the file into the table's map the first time one record is touched, so
    // one read of one photograph makes every photograph resident.
    assert!(
        db.get_table_read_only_for_account("DIRS", "BLOBS").is_none(),
        "a directory file must never be loaded into the table cache"
    );
}

#[test]
fn a_key_that_would_not_be_a_file_name_is_refused_rather_than_repaired() {
    // Every one of these is a name that a filesystem would either reject or
    // accept as something other than what was asked for. Repairing them would
    // mean a write that succeeds and reads back under a different key.
    for key in [
        "",
        "../escape",
        "a/b",
        "a\\b",
        ".hidden",
        ".",
        "..",
        "with\nnewline",
        "with\0nul",
    ] {
        assert!(
            directory::validate_key(key).is_err(),
            "'{}' should not be a usable key",
            key.escape_debug()
        );
    }
    for key in ["scan.pdf", "INVOICE-2026-01", "a b c", "üñî.txt", "x"] {
        assert!(directory::validate_key(key).is_ok(), "'{}' should be a usable key", key);
    }
    // The bound is on bytes, because that is what the filesystem counts.
    assert!(directory::validate_key(&"a".repeat(directory::MAX_KEY_BYTES)).is_ok());
    assert!(directory::validate_key(&"a".repeat(directory::MAX_KEY_BYTES + 1)).is_err());
}

#[test]
fn a_record_past_the_configured_maximum_is_refused_rather_than_read() {
    let dir = TempDir::new("directory_max_size");
    let mut config = isolated_config();
    config.max_directory_record_bytes = Some(64);
    let db = Database::new(dir.path(), Some(config)).unwrap();
    db.create_account("DIRS", Some(dir.path())).unwrap();
    db.logto("DIRS").unwrap();
    directory_file(&db, "DIRS", "BLOBS");

    let refusal = db.write_directory_record("DIRS", "BLOBS", "big", &[b'x'; 65]);
    assert!(matches!(refusal, Err(DbError::InvalidRequest(_))), "{refusal:?}");
    // Refused, so nothing was written - not truncated, and no debris left.
    let root = db.directory_root("DIRS", "BLOBS").unwrap();
    assert!(!root.join("big").exists());
    assert_eq!(directory::keys(&root).unwrap(), Vec::<String>::new());

    // A file that grew past the limit out of band is refused on the way out
    // too, rather than being read into an allocation nobody bounded.
    std::fs::write(root.join("grown"), vec![b'x'; 65]).unwrap();
    let refusal = db.read_directory_record("DIRS", "BLOBS", "grown");
    assert!(matches!(refusal, Err(DbError::InvalidRequest(_))), "{refusal:?}");
}

#[test]
fn store_and_extract_move_a_host_file_byte_for_byte() {
    let dir = TempDir::new("directory_store");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");

    // Large enough that it could not have travelled as one protocol request:
    // the request line is capped at a megabyte and base64 inflates by 4/3.
    let mut content = Vec::new();
    while content.len() < 3 * 1024 * 1024 {
        content.extend_from_slice(HOSTILE);
    }
    let source = Path::new(dir.path()).join("incoming.bin");
    std::fs::write(&source, &content).unwrap();

    let stored = db
        .store_directory_record("DIRS", "BLOBS", "invoice.bin", &source)
        .unwrap();
    assert_eq!(stored as usize, content.len());

    let destination = Path::new(dir.path()).join("outgoing.bin");
    let extracted = db
        .extract_directory_record("DIRS", "BLOBS", "invoice.bin", &destination)
        .unwrap();
    assert_eq!(extracted, Some(content.len() as u64));
    assert_eq!(std::fs::read(&destination).unwrap(), content);
}

#[test]
fn extracting_a_record_that_is_not_there_leaves_the_destination_alone() {
    let dir = TempDir::new("directory_extract_missing");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");

    let destination = Path::new(dir.path()).join("existing.txt");
    std::fs::write(&destination, b"do not clobber me").unwrap();
    assert_eq!(
        db.extract_directory_record("DIRS", "BLOBS", "absent", &destination)
            .unwrap(),
        None
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"do not clobber me");
}

#[test]
fn a_write_leaves_no_temporary_behind_and_a_crash_between_the_two_is_swept() {
    let dir = TempDir::new("directory_tmp");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "one", b"content").unwrap();
    let root = db.directory_root("DIRS", "BLOBS").unwrap();

    let debris: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(directory::TMP_PREFIX))
        .collect();
    assert!(debris.is_empty(), "a completed write left {debris:?}");

    // What a crash between `create` and `rename` leaves. It is not a record -
    // it has no key a caller could name - and the read path clears it away.
    std::fs::write(root.join(format!("{}999.0", directory::TMP_PREFIX)), b"half").unwrap();
    assert_eq!(directory::keys(&root).unwrap(), vec!["one".to_string()]);
    assert!(!root.join(format!("{}999.0", directory::TMP_PREFIX)).exists());
}

#[test]
fn overwriting_a_record_replaces_it_whole() {
    let dir = TempDir::new("directory_overwrite");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "r", &[b'a'; 100]).unwrap();
    db.write_directory_record("DIRS", "BLOBS", "r", b"short").unwrap();
    // Renamed over rather than written into, so nothing of the old record can
    // be left past the end of the new one.
    assert_eq!(
        db.read_directory_record("DIRS", "BLOBS", "r").unwrap().as_deref(),
        Some(&b"short"[..])
    );
}

#[test]
fn listing_gives_keys_and_sizes_in_name_order_and_never_content() {
    let dir = TempDir::new("directory_listing");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "beta", b"22").unwrap();
    db.write_directory_record("DIRS", "BLOBS", "alpha", b"1").unwrap();
    db.write_directory_record("DIRS", "BLOBS", "gamma", b"333").unwrap();

    let records = db.directory_records("DIRS", "BLOBS").unwrap();
    let listed: Vec<(String, u64)> = records.into_iter().map(|r| (r.key, r.bytes)).collect();
    assert_eq!(
        listed,
        vec![
            ("alpha".to_string(), 1),
            ("beta".to_string(), 2),
            ("gamma".to_string(), 3)
        ]
    );

    assert!(db.delete_directory_record("DIRS", "BLOBS", "beta").unwrap());
    assert!(!db.delete_directory_record("DIRS", "BLOBS", "beta").unwrap());
    assert_eq!(db.directory_records("DIRS", "BLOBS").unwrap().len(), 2);
}

#[test]
fn a_file_created_with_a_path_keeps_its_records_there() {
    let dir = TempDir::new("directory_path");
    let elsewhere = Path::new(dir.path()).join("spool");
    let db = open_account(dir.path(), "DIRS");
    db.create_table_with(
        "DIRS",
        "SPOOL",
        FileAttributes {
            durable: false,
            queue: None,
            directory: Some(DirectoryPolicy::at(elsewhere.to_string_lossy().into_owned())),
        },
    )
    .unwrap();

    db.write_directory_record("DIRS", "SPOOL", "job", b"work").unwrap();
    assert_eq!(std::fs::read(elsewhere.join("job")).unwrap(), b"work");

    // And a file already in that directory is a record, which is what makes the
    // type a pointer at a real tree rather than a private store with a path.
    std::fs::write(elsewhere.join("arrived"), b"from outside").unwrap();
    assert_eq!(
        db.read_directory_record("DIRS", "SPOOL", "arrived").unwrap().as_deref(),
        Some(&b"from outside"[..])
    );
}

#[test]
fn the_dir_entry_survives_a_rebuild_of_the_listing() {
    let dir = TempDir::new("directory_dir_entry");
    let db = open_account(dir.path(), "DIRS");
    let elsewhere = Path::new(dir.path()).join("spool");
    let attributes = FileAttributes {
        durable: false,
        queue: None,
        directory: Some(DirectoryPolicy::at(elsewhere.to_string_lossy().into_owned())),
    };
    db.create_table_with("DIRS", "SPOOL", attributes.clone()).unwrap();

    // The listing is rebuilt from the filesystem, which knows nothing about
    // types or paths, so every attribute has to survive the round trip through
    // the DIR record or be lost.
    assert_eq!(FileAttributes::of(&attributes.to_record()), attributes);
    db.sync_dir_file_for_account("DIRS").unwrap();
    let rebuilt = db.file_attributes_for_account("DIRS", "SPOOL");
    assert_eq!(rebuilt, attributes);
    assert_eq!(db.directory_root("DIRS", "SPOOL").unwrap(), elsewhere);
}

#[test]
fn the_type_decides_what_the_rest_of_the_entry_can_say() {
    // A hand-edited entry claiming to be a directory file *and* a durable queue
    // has said two contradictory things. The type is the half that says where
    // the records are, so it wins and the rest reads as what it now is.
    let mut record = Record::from_display_string("D^Y^Y^90^3^/srv/scans");
    assert_eq!(
        FileAttributes::of(&record),
        FileAttributes {
            durable: false,
            queue: None,
            directory: Some(DirectoryPolicy::at("/srv/scans")),
        }
    );
    // And an ordinary entry is read exactly as it was before directory files
    // existed, six attributes or five.
    record = Record::from_display_string("F^Y^^^");
    assert!(!FileAttributes::of(&record).is_directory());
    assert!(FileAttributes::of(&record).durable);
}

#[test]
fn the_commands_a_directory_file_cannot_answer_are_refused_rather_than_answered_emptily() {
    let dir = TempDir::new("directory_refusals");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "one", b"content").unwrap();

    // An index maps a dictionary field's values to keys, and there are no
    // fields: building one would read every record to index nothing.
    assert!(matches!(
        db.create_index_for_account("DIRS", "BLOBS", "ANYTHING"),
        Err(DbError::InvalidRequest(_))
    ));
    // A queue mints sequence keys and hands records out under a claim. Neither
    // exists here, and the general "not a queue file" refusal would send the
    // operator to a SET.FILE that is also refused.
    assert!(matches!(
        db.enqueue("DIRS", "BLOBS", Record::from_display_string("work")),
        Err(DbError::InvalidRequest(_))
    ));
    // A transaction holds every file it touches until the whole set is written.
    // A directory record commits on its own `rename`, so it cannot be held.
    let refusal = db.apply_transaction(
        "DIRS",
        vec![Change {
            file: "BLOBS".to_string(),
            key: "two".to_string(),
            is_dict: false,
            op: ChangeOp::Write(Record::from_display_string("x")),
        }],
    );
    assert!(matches!(refusal, Err(DbError::TransactionScope(_))), "{refusal:?}");
    // And nothing was applied.
    assert!(db.read_directory_record("DIRS", "BLOBS", "two").unwrap().is_none());
}

#[test]
fn asking_a_file_that_is_not_a_directory_file_for_its_records_is_refused() {
    let dir = TempDir::new("directory_wrong_type");
    let db = open_account(dir.path(), "DIRS");
    db.create_table_for_account("DIRS", "USERS").unwrap();
    assert!(matches!(
        db.read_directory_record("DIRS", "USERS", "anything"),
        Err(DbError::InvalidRequest(_))
    ));
    assert!(matches!(
        db.directory_root("DIRS", "USERS"),
        Err(DbError::InvalidRequest(_))
    ));
}

#[test]
fn file_statistics_count_the_records_without_reading_one() {
    let dir = TempDir::new("directory_stats");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "small", &[b'x'; 10])
        .unwrap();
    db.write_directory_record("DIRS", "BLOBS", "large", &vec![b'x'; 1000])
        .unwrap();
    db.clear_loaded_tables();

    let stats = db.file_statistics("DIRS", "BLOBS").unwrap();
    let held = stats.directory.expect("a directory file reports what it holds");
    assert_eq!(held.record_count, 2);
    assert_eq!(held.bytes, 1010);
    assert_eq!(held.largest_bytes, 1000);
    assert_eq!(stats.record_count, 2, "the headline count is the records it really has");
    assert!(
        db.get_table_read_only_for_account("DIRS", "BLOBS").is_none(),
        "FILE.STATS must not load the file"
    );

    // The verdicts are about a directory file rather than about a hash it has
    // not got: no modulus, no skew, no "legacy flat file".
    let ids: Vec<&str> = stats.health.measures.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["format", "largest_record"]);
    assert_eq!(stats.health.verdict, crate::db::Verdict::Good);
}

#[test]
fn a_largest_record_near_the_limit_is_worth_watching_before_it_is_refused() {
    let dir = TempDir::new("directory_health");
    let mut config = isolated_config();
    config.max_directory_record_bytes = Some(1000);
    let db = Database::new(dir.path(), Some(config)).unwrap();
    db.create_account("DIRS", Some(dir.path())).unwrap();
    db.logto("DIRS").unwrap();
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "nearly", &vec![b'x'; 900])
        .unwrap();

    let stats = db.file_statistics("DIRS", "BLOBS").unwrap();
    // The refusal arrives on a write; the dashboard is where an operator would
    // rather hear about it, which is why the verdict comes before the limit.
    assert_eq!(stats.health.verdict, crate::db::Verdict::Watch);
}

#[test]
fn deleting_the_file_takes_the_records_it_owns_with_it() {
    let dir = TempDir::new("directory_delete_file");
    let db = open_account(dir.path(), "DIRS");
    directory_file(&db, "DIRS", "BLOBS");
    db.write_directory_record("DIRS", "BLOBS", "one", b"content").unwrap();
    let root = db.directory_root("DIRS", "BLOBS").unwrap();
    assert!(root.exists());

    db.delete_table_for_account("DIRS", "BLOBS").unwrap();
    // Owned because they are inside the file's own directory. A file pointing
    // at a tree of somebody else's is a different case, and is not this one.
    assert!(!root.exists());
}
