//! Backup and restore against a live engine: what an export captures, what a
//! restore puts back, and the refusals that keep a damaged archive from
//! becoming a damaged database.
//!
//! The format's own guarantees - truncation, alteration, framing - are pinned
//! in `archive_tests.rs` against bytes alone. What is here is everything that
//! needs a database to be true of it.

use crate::db::archive::{self, FileKind, Source};
use crate::db::engine::Database;
use crate::db::engine::archive::{FileAction, ImportPlan};
use crate::db::models::*;
use crate::db::{Condition, DbError, DirectoryPolicy, FileAttributes, QueuePolicy};
use crate::test_support::{TempDir, isolated_config};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn record(value: &str) -> Record {
    Record::from_display_string(value)
}

fn open(base: &str) -> Database {
    Database::new(base, Some(isolated_config())).unwrap()
}

/// An account with a record file, a dictionary, an index and a queue - enough
/// that "the account came back" means something.
fn populated(db: &Database, account: &str) {
    db.create_account(account, None).unwrap();

    db.create_table_for_account(account, "ORDERS").unwrap();
    let handle = db.get_table_mut_for_account(account, "ORDERS").unwrap();
    {
        let mut table = handle.write();
        table
            .dictionary
            .insert("CUSTOMER".to_string(), record("1^Customer^L^20"));
        table.dictionary.insert("STATE".to_string(), record("2^State^L^10"));
        table.mark_dict_dirty();
        for (key, value) in [("O-1", "ACME^PLACED"), ("O-2", "BETA^SHIPPED"), ("O-3", "ACME^PLACED")] {
            table.insert_record(key, record(value));
        }
    }
    db.create_index_for_account(account, "ORDERS", "STATE").unwrap();

    db.create_table_with(
        account,
        "JOBS",
        FileAttributes {
            durable: true,
            queue: Some(QueuePolicy {
                visibility: std::time::Duration::from_secs(45),
                max_deliveries: 7,
            }),
            autokey: false,
            directory: None,
        },
    )
    .unwrap();

    db.create_table_with(
        account,
        "EVENTS",
        FileAttributes {
            durable: false,
            queue: None,
            autokey: true,
            directory: None,
        },
    )
    .unwrap();
    let handle = db.get_table_mut_for_account(account, "EVENTS").unwrap();
    for value in ["first", "second", "third"] {
        db.write_record_in(
            account,
            "EVENTS",
            &handle,
            None,
            record(value),
            false,
            &Condition::Always,
        )
        .unwrap();
    }

    db.save().unwrap();
}

fn records_of(db: &Database, account: &str, file: &str) -> Vec<(String, Vec<u8>)> {
    let handle = db.get_table_mut_for_account(account, file).unwrap();
    let table = handle.read();
    let mut out: Vec<(String, Vec<u8>)> = table
        .records
        .iter()
        .map(|(key, record)| (key.clone(), record.to_bytes()))
        .collect();
    out.sort();
    out
}

fn dictionary_of(db: &Database, account: &str, file: &str) -> Vec<(String, Vec<u8>)> {
    let handle = db.get_table_mut_for_account(account, file).unwrap();
    let table = handle.read();
    let mut out: Vec<(String, Vec<u8>)> = table
        .dictionary
        .iter()
        .map(|(key, record)| (key.clone(), record.to_bytes()))
        .collect();
    out.sort();
    out
}

fn archive_at(dir: &TempDir, name: &str) -> PathBuf {
    PathBuf::from(format!("{}/{}.srp", dir.path(), name))
}

// ------------------------------------------------------------- round trips ---

/// The acceptance criterion in one test: an account exported, dropped and
/// imported is the account it was - records, dictionaries, file types and
/// per-file flags.
#[test]
fn an_account_restores_to_exactly_what_was_exported() {
    let guard = TempDir::new("archive_round_trip");
    let db = open(guard.path());
    populated(&db, "SALES");

    let before_records = records_of(&db, "SALES", "ORDERS");
    let before_dictionary = dictionary_of(&db, "SALES", "ORDERS");

    let path = archive_at(&guard, "sales");
    let trailer = db
        .export_to_path(
            &Source::Account {
                account: "SALES".to_string(),
            },
            &path,
        )
        .unwrap();
    assert_eq!(trailer.records(), 6, "three ORDERS and three EVENTS");
    assert_eq!(trailer.dictionary(), 2);

    db.delete_account("SALES").unwrap();
    assert!(db.get_account_dir("SALES").is_none(), "dropped");

    let report = db.import(&path, &ImportPlan::default()).unwrap();
    assert_eq!(report.records(), 6);
    assert_eq!(report.accounts_created, vec!["SALES"]);
    assert!(report.files.iter().all(|f| f.action == FileAction::Created));

    assert_eq!(records_of(&db, "SALES", "ORDERS"), before_records);
    assert_eq!(dictionary_of(&db, "SALES", "ORDERS"), before_dictionary);

    // The flags travel with the file, not just its records. A queue restored as
    // an ordinary file is a restore that looks right and is not.
    let jobs = db.file_attributes_for_account("SALES", "JOBS");
    assert!(jobs.durable, "the durable flag came back");
    let policy = jobs.queue.expect("JOBS is still a queue");
    assert_eq!(policy.visibility.as_secs(), 45);
    assert_eq!(policy.max_deliveries, 7);

    // An index is a definition plus postings derived from the records that
    // actually arrived.
    let handle = db.get_table_mut_for_account("SALES", "ORDERS").unwrap();
    assert!(handle.read().has_index("STATE"), "the index definition came back");
    assert_eq!(
        handle.read().index_candidates("STATE", "PLACED").map(|k| k.len()),
        Some(2),
        "and its postings describe the restored records"
    );
}

/// An autokey file mints its own keys, so a restore has two things to get
/// right: the flag, and a counter that cannot hand out a key the restore just
/// put back. The second is the one that would corrupt data rather than merely
/// annoy - a minted key landing on a restored record overwrites it.
#[test]
fn an_autokey_file_restores_its_flag_and_mints_past_the_records_that_came_back() {
    let guard = TempDir::new("archive_autokey");
    let db = open(guard.path());
    populated(&db, "SALES");

    let before: Vec<String> = records_of(&db, "SALES", "EVENTS")
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(before.len(), 3, "the fixture minted three keys");

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();
    db.delete_account("SALES").unwrap();
    db.import(&path, &ImportPlan::default()).unwrap();

    assert!(
        db.is_table_autokey_for_account("SALES", "EVENTS"),
        "the flag came back, so a keyless write is still minted rather than refused"
    );
    assert_eq!(
        records_of(&db, "SALES", "EVENTS")
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>(),
        before,
        "under the keys they were minted with"
    );

    // The counter is not carried in the archive - it is derived from the keys
    // that are actually there, which is what makes it right rather than merely
    // restored. A minted key that collided would silently replace a record the
    // restore had just put back.
    let handle = db.get_table_mut_for_account("SALES", "EVENTS").unwrap();
    let minted = db
        .write_record_in(
            "SALES",
            "EVENTS",
            &handle,
            None,
            record("after"),
            false,
            &Condition::Always,
        )
        .unwrap();
    assert!(minted.minted, "it minted rather than taking a key from the caller");
    assert!(
        !before.contains(&minted.key),
        "the minted key {} is past every restored key {:?}",
        minted.key,
        before
    );
    assert_eq!(records_of(&db, "SALES", "EVENTS").len(), 4, "nothing was overwritten");
}

/// A queue's records, order and keys come back; its **delivery counts do not**.
///
/// The `queue` file beside the records holds two things, and they restore
/// differently. The sequence self-heals - it is pulled past every key that came
/// back, so a restored queue cannot mint a key that lands on one of its own
/// records. The delivery counts are simply gone, so a record that had used four
/// of its five attempts starts again at zero.
///
/// That is the documented behaviour of a lost `queue` file rather than anything
/// new here, but a restore is the most likely way to meet it, and a poison
/// record quietly getting its retries back is worth a test and a line in the
/// documentation rather than a discovery.
#[test]
fn a_restored_queue_keeps_its_records_and_starts_their_delivery_counts_again() {
    let guard = TempDir::new("archive_queue");
    let db = open(guard.path());
    populated(&db, "SALES");

    let first = db.enqueue("SALES", "JOBS", record("work^one")).unwrap();
    db.enqueue("SALES", "JOBS", record("work^two")).unwrap();

    // Take the head and hand it back, so it carries a delivery count that a
    // restore has no way to know about.
    let claimed = db.dequeue("SALES", "JOBS", "worker", None).unwrap().unwrap();
    assert_eq!(claimed.key, first);
    db.nack("SALES", "JOBS", &first, "worker").unwrap();
    let delivered = db.peek("SALES", "JOBS", None).unwrap().unwrap();
    assert_eq!(delivered.deliveries, 1, "it has been delivered once");
    db.save().unwrap();

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();
    db.delete_account("SALES").unwrap();
    db.import(&path, &ImportPlan::default()).unwrap();

    let policy = db
        .file_attributes_for_account("SALES", "JOBS")
        .queue
        .expect("it came back a queue");
    assert_eq!(policy.max_deliveries, 7, "with its policy intact");

    let restored = db.peek("SALES", "JOBS", None).unwrap().unwrap();
    assert_eq!(restored.key, first, "the same record is at the head, under its own key");
    assert_eq!(
        restored.deliveries, 0,
        "but its delivery count starts again - the counts are not carried"
    );

    // And the sequence is past the restored keys, so the next enqueue cannot
    // land on one of them. This is the half that would be data loss.
    let next = db.enqueue("SALES", "JOBS", record("work^three")).unwrap();
    assert!(next > first, "{} should sort after {}", next, first);
    assert_eq!(records_of(&db, "SALES", "JOBS").len(), 3, "nothing was overwritten");
}

/// A record that is not UTF-8 is exactly the record nobody can retype, so it is
/// the one a backup most has to carry untouched.
#[test]
fn a_record_that_is_not_text_survives_the_round_trip() {
    let guard = TempDir::new("archive_bytes");
    let db = open(guard.path());
    db.create_account("ODD", None).unwrap();
    db.create_table_for_account("ODD", "BLOBS").unwrap();

    let awkward = Record::from_attributes(["\u{fffd}".to_string()]);
    let raw = Record {
        fields: vec![Field {
            values: vec![Value {
                sub_values: vec![vec![0xff, 0x00, 0xfd, 0x41]],
            }],
        }],
    };
    {
        let handle = db.get_table_mut_for_account("ODD", "BLOBS").unwrap();
        let mut table = handle.write();
        table.insert_record("raw", raw.clone());
        table.insert_record("replacement", awkward.clone());
    }
    db.save().unwrap();

    let path = archive_at(&guard, "odd");
    db.export_to_path(
        &Source::Account {
            account: "ODD".to_string(),
        },
        &path,
    )
    .unwrap();
    db.delete_account("ODD").unwrap();
    db.import(&path, &ImportPlan::default()).unwrap();

    let handle = db.get_table_mut_for_account("ODD", "BLOBS").unwrap();
    let table = handle.read();
    assert_eq!(table.records.get("raw").map(Record::to_bytes), Some(raw.to_bytes()));
    assert_eq!(
        table.records.get("replacement").map(Record::to_bytes),
        Some(awkward.to_bytes())
    );
}

/// A directory file's records are host files with no framing at all, so they
/// have to travel as bytes rather than as records.
#[test]
fn a_directory_files_records_survive_byte_for_byte() {
    let guard = TempDir::new("archive_directory");
    let db = open(guard.path());
    db.create_account("DOCS", None).unwrap();
    db.create_table_with(
        "DOCS",
        "SCANS",
        FileAttributes {
            durable: false,
            queue: None,
            autokey: false,
            directory: Some(DirectoryPolicy::default_path()),
        },
    )
    .unwrap();

    // Every mark byte and an embedded NUL: content an ordinary record cannot
    // carry, which is the whole reason the file type exists.
    let awkward: Vec<u8> = vec![0xFE, 0xFD, 0xFC, 0x00, b'P', b'N', b'G', 0x0A, 0xFF];
    let large: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
    db.write_directory_record("DOCS", "SCANS", "marks.bin", &awkward)
        .unwrap();
    db.write_directory_record("DOCS", "SCANS", "big.bin", &large).unwrap();

    let path = archive_at(&guard, "docs");
    let trailer = db
        .export_to_path(
            &Source::Account {
                account: "DOCS".to_string(),
            },
            &path,
        )
        .unwrap();
    assert_eq!(trailer.records(), 2);
    assert_eq!(trailer.bytes(), (awkward.len() + large.len()) as u64);

    db.delete_account("DOCS").unwrap();
    db.import(&path, &ImportPlan::default()).unwrap();

    assert!(
        db.is_table_directory_for_account("DOCS", "SCANS"),
        "it came back a directory file, not an ordinary one"
    );
    assert_eq!(
        db.read_directory_record("DOCS", "SCANS", "marks.bin").unwrap(),
        Some(awkward)
    );
    assert_eq!(
        db.read_directory_record("DOCS", "SCANS", "big.bin").unwrap(),
        Some(large)
    );
}

/// Restoring beside the original rather than over it: the case an operator
/// reaches for to inspect a production file without touching production.
#[test]
fn an_archive_restores_into_a_different_account_without_disturbing_the_original() {
    let guard = TempDir::new("archive_rename");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();

    // The original carries on changing while the copy is restored.
    {
        let handle = db.get_table_mut_for_account("SALES", "ORDERS").unwrap();
        handle.write().insert_record("O-4", record("GAMMA^NEW"));
    }
    db.save().unwrap();

    let report = db
        .import(
            &path,
            &ImportPlan {
                into_account: Some("SALES.COPY".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(report.accounts_created, vec!["SALES.COPY"]);

    assert_eq!(records_of(&db, "SALES.COPY", "ORDERS").len(), 3, "the copy is as taken");
    assert_eq!(
        records_of(&db, "SALES", "ORDERS").len(),
        4,
        "and the original is untouched, later record included"
    );
}

// --------------------------------------------------------------- refusals ---

/// Nothing is half-imported. The collision is found in the plan, before a byte
/// is written, and the message names every file rather than the first.
#[test]
fn an_import_onto_existing_files_is_refused_whole_unless_overwrite_is_given() {
    let guard = TempDir::new("archive_overwrite");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();

    // The live account has moved on.
    {
        let handle = db.get_table_mut_for_account("SALES", "ORDERS").unwrap();
        handle.write().insert_record("O-9", record("LATER^PLACED"));
    }
    db.save().unwrap();

    let refused = db.import(&path, &ImportPlan::default()).unwrap_err();
    let message = refused.to_string();
    assert!(message.contains("OVERWRITE"), "{}", message);
    assert!(message.contains("SALES/ORDERS"), "{}", message);
    assert!(
        message.contains("SALES/JOBS"),
        "every collision, not the first: {}",
        message
    );
    assert_eq!(
        records_of(&db, "SALES", "ORDERS").len(),
        4,
        "and the refusal changed nothing"
    );

    // With the flag, the file is replaced rather than merged: the record the
    // live account gained is gone, because a restore restores.
    let report = db
        .import(
            &path,
            &ImportPlan {
                overwrite: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(report.files.iter().all(|f| f.action == FileAction::Replaced));
    let restored = records_of(&db, "SALES", "ORDERS");
    assert_eq!(restored.len(), 3);
    assert!(
        !restored.iter().any(|(key, _)| key == "O-9"),
        "a restore is not a merge"
    );
}

/// A truncated archive must be refused before it can half-restore an account.
/// The format catches the truncation; what this pins is that the engine has not
/// written anything by the time it does.
#[test]
fn a_truncated_archive_imports_nothing_at_all() {
    let guard = TempDir::new("archive_truncated");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();
    db.delete_account("SALES").unwrap();

    let whole = std::fs::read(&path).unwrap();
    let cut = path.with_extension("cut");
    std::fs::write(&cut, &whole[..whole.len() - 8]).unwrap();

    let refused = db.import(&cut, &ImportPlan::default()).unwrap_err();
    assert!(matches!(refused, DbError::InvalidRequest(_)), "{:?}", refused);
    assert!(
        db.get_account_dir("SALES").is_none(),
        "a refused import created no account, let alone a file"
    );

    // An altered one, the same. The account is still not there afterwards.
    let mut altered = whole.clone();
    let middle = altered.len() / 2;
    altered[middle] ^= 0x20;
    let bad = path.with_extension("altered");
    std::fs::write(&bad, &altered).unwrap();
    assert!(db.import(&bad, &ImportPlan::default()).is_err());
    assert!(db.get_account_dir("SALES").is_none());
}

/// A whole-database archive names several accounts, so there is no single
/// account to rename it into. Refused rather than merged.
#[test]
fn a_multi_account_archive_cannot_be_renamed_onto_one_account() {
    let guard = TempDir::new("archive_multi");
    let db = open(guard.path());
    populated(&db, "SALES");
    populated(&db, "STOCK");

    let path = archive_at(&guard, "all");
    db.export_to_path(&Source::All, &path).unwrap();

    let refused = db
        .import(
            &path,
            &ImportPlan {
                into_account: Some("EVERYTHING".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
    let message = refused.to_string();
    assert!(message.contains("SALES") && message.contains("STOCK"), "{}", message);
    assert!(db.get_account_dir("EVERYTHING").is_none());
}

/// A dry run is the mode most people will use most of the time. It has to
/// report exactly what the real one would do, and write nothing.
#[test]
fn a_dry_run_reports_what_would_happen_and_changes_nothing() {
    let guard = TempDir::new("archive_dry_run");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();

    let report = db
        .import(
            &path,
            &ImportPlan {
                into_account: Some("SALES.COPY".to_string()),
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(report.dry_run);
    assert_eq!(report.records(), 6);
    assert!(report.accounts_created.is_empty());
    assert!(
        report.files.iter().all(|f| f.action == FileAction::Created),
        "it says they would be created"
    );
    assert!(db.get_account_dir("SALES.COPY").is_none(), "and creates none of them");

    // Against files that are there, the dry run says `replaced` - and still
    // needs the overwrite flag, because reporting a destructive plan as
    // acceptable and then refusing it would be the worst of both.
    assert!(
        db.import(
            &path,
            &ImportPlan {
                dry_run: true,
                ..Default::default()
            }
        )
        .is_err()
    );
    let report = db
        .import(
            &path,
            &ImportPlan {
                dry_run: true,
                overwrite: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(report.files.iter().all(|f| f.action == FileAction::Replaced));
    assert_eq!(records_of(&db, "SALES", "ORDERS").len(), 3, "still untouched");
}

// ------------------------------------------------------------ consistency ---

/// The criterion the issue is most specific about: an export taken while
/// writers are working produces an archive that restores to a state the writers
/// actually passed through - no torn records, no file caught half way.
///
/// The writer runs in a thread throughout, appending records to one file and
/// mirroring each one into a second file. The invariant across the two is what
/// makes a smeared export visible: if the archive ever caught the account
/// between the two writes of one pair, the restored files disagree.
#[test]
fn an_export_under_continuous_writes_restores_to_a_state_the_writers_passed_through() {
    let guard = TempDir::new("archive_concurrent");
    let db = Arc::new(open(guard.path()));
    db.create_account("LIVE", None).unwrap();
    db.create_table_for_account("LIVE", "LEFT").unwrap();
    db.create_table_for_account("LIVE", "RIGHT").unwrap();
    db.save().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) {
                let key = format!("K-{:05}", n);
                // The pair a smear would break: both files, or neither, and an
                // export that lands between them would restore a LEFT record
                // with no RIGHT partner.
                let left = db.get_table_mut_for_account("LIVE", "LEFT").unwrap();
                let right = db.get_table_mut_for_account("LIVE", "RIGHT").unwrap();
                {
                    let mut a = left.write();
                    let mut b = right.write();
                    a.insert_record(&key, record(&format!("L^{}", n)));
                    b.insert_record(&key, record(&format!("R^{}", n)));
                }
                n += 1;
                if n.is_multiple_of(32) {
                    let _ = db.save();
                }
                std::thread::yield_now();
            }
            n
        })
    };

    let live_count = || {
        db.get_table_mut_for_account("LIVE", "LEFT")
            .map(|h| h.read().records.len())
            .unwrap_or(0)
    };
    // Let the writer get going, so the export is genuinely concurrent rather
    // than racing an empty account.
    while live_count() < 50 {
        std::thread::yield_now();
    }

    let path = archive_at(&guard, "live");
    db.export_to_path(
        &Source::Account {
            account: "LIVE".to_string(),
        },
        &path,
    )
    .unwrap();

    // The writer is still going after the export let the locks go. Waiting for
    // it to make progress proves the thread was live across the export, rather
    // than inferring it from a count that a slow machine can reach just as the
    // export starts.
    let before = live_count();
    while live_count() < before + 10 {
        std::thread::yield_now();
    }

    stop.store(true, Ordering::Relaxed);
    let written = writer.join().unwrap();
    assert!(
        written as usize >= before + 10,
        "the writer ran throughout: {}",
        written
    );

    // The archive decodes, which is the first thing a smear would break.
    let summary = archive::verify(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    let counts: Vec<u64> = summary.trailer.files.iter().map(|f| f.records).collect();
    assert_eq!(
        counts.len(),
        2,
        "both files are in the archive: {:?}",
        summary.trailer.files
    );

    let db = open(guard.path());
    db.import(
        &path,
        &ImportPlan {
            into_account: Some("RESTORED".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let left = records_of(&db, "RESTORED", "LEFT");
    let right = records_of(&db, "RESTORED", "RIGHT");
    assert!(!left.is_empty(), "the export caught a running account");
    let left_keys: Vec<&String> = left.iter().map(|(key, _)| key).collect();
    let right_keys: Vec<&String> = right.iter().map(|(key, _)| key).collect();
    assert_eq!(
        left_keys, right_keys,
        "the two files were captured at one instant: a record in one has its partner in the other"
    );
    // And each record is whole, rather than a value caught mid-write.
    for (key, bytes) in &left {
        let n = key.trim_start_matches("K-").trim_start_matches('0');
        let n = if n.is_empty() { "0" } else { n };
        // Compared as bytes: the record's two attributes are joined by the FM
        // mark, which is the byte 0xFE and not a character.
        assert_eq!(
            bytes,
            &record(&format!("L^{}", n)).to_bytes(),
            "record {} is whole",
            key
        );
    }
}

// ------------------------------------------------------------------ scope ---

/// An export of one file is one file, and its shape rather than its account's.
#[test]
fn exporting_one_file_carries_that_file_and_nothing_else() {
    let guard = TempDir::new("archive_one_file");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "orders");
    db.export_to_path(
        &Source::File {
            account: "SALES".to_string(),
            file: "ORDERS".to_string(),
        },
        &path,
    )
    .unwrap();

    let summary = archive::verify(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    assert_eq!(summary.manifest.files.len(), 1);
    assert_eq!(summary.manifest.files[0].name, "ORDERS");
    assert_eq!(summary.manifest.files[0].kind, FileKind::File);
    assert_eq!(
        summary.manifest.source,
        Source::File {
            account: "SALES".to_string(),
            file: "ORDERS".to_string()
        }
    );
}

/// `DIR` is the account's listing of its own files, not data. Restoring it
/// would put back a listing describing whatever the source account had, beside
/// files the restore actually created.
#[test]
fn the_listing_file_is_not_exported() {
    let guard = TempDir::new("archive_dir");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "sales");
    db.export_to_path(
        &Source::Account {
            account: "SALES".to_string(),
        },
        &path,
    )
    .unwrap();

    let summary = archive::verify(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    assert!(
        !summary.manifest.files.iter().any(|f| f.name == "DIR"),
        "DIR travels as the files that are restored, not as a record file"
    );
    assert!(
        db.export_to_path(
            &Source::File {
                account: "SALES".to_string(),
                file: "DIR".to_string()
            },
            &archive_at(&guard, "dir"),
        )
        .is_err(),
        "and asking for it by name says so rather than producing a useless archive"
    );
}

/// A whole-database export leaves `SYSTEM` out. It holds the client certificate
/// thumbprints, and an archive that carried them would grant the source
/// machine's authorizations to wherever it was restored.
#[test]
fn a_whole_database_export_leaves_system_out() {
    let guard = TempDir::new("archive_all");
    let db = open(guard.path());
    populated(&db, "SALES");
    populated(&db, "STOCK");

    let path = archive_at(&guard, "all");
    db.export_to_path(&Source::All, &path).unwrap();

    let summary = archive::verify(std::io::BufReader::new(std::fs::File::open(&path).unwrap())).unwrap();
    let accounts: Vec<&str> = summary.manifest.files.iter().map(|f| f.account.as_str()).collect();
    assert!(accounts.contains(&"SALES") && accounts.contains(&"STOCK"));
    assert!(
        !accounts.contains(&"SYSTEM"),
        "SYSTEM is not data, and $CLIENTS is not something a backup should carry silently: {:?}",
        accounts
    );

    // It is still exportable on purpose, which is the point of leaving it out
    // of the default rather than refusing it outright.
    db.export_to_path(
        &Source::Account {
            account: "SYSTEM".to_string(),
        },
        &archive_at(&guard, "system"),
    )
    .unwrap();
}

/// An export names a path an operator will reach for in an emergency. A failed
/// one must not be sitting at that name looking like a backup.
#[test]
fn a_failed_export_leaves_nothing_at_the_path() {
    let guard = TempDir::new("archive_failed");
    let db = open(guard.path());
    populated(&db, "SALES");

    let path = archive_at(&guard, "missing");
    let refused = db
        .export_to_path(
            &Source::Account {
                account: "NOPE".to_string(),
            },
            &path,
        )
        .unwrap_err();
    assert!(matches!(refused, DbError::AccountNotFound(_)), "{:?}", refused);
    assert!(!path.exists(), "no archive, and no staging file left behind either");
    let strays: Vec<_> = std::fs::read_dir(guard.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.contains("tmp"))
        .collect();
    assert!(strays.is_empty(), "{:?}", strays);
}

/// Two exports of data that has not changed differ **only** in the moment they
/// were taken.
///
/// Worth pinning because it is not free: a hash map's iteration order is not a
/// property anything should depend on, so both record sets are walked in sorted
/// key order specifically to make this true. It is what lets an operator answer
/// "has this backup actually changed?" by comparing two archives instead of
/// restoring them.
///
/// The timestamp is the one legitimate difference, and the checksum over it is
/// the second - a stamp that differs by a millisecond changes the CRC32C of the
/// whole archive, which is the trailer doing its job. Both are excluded here
/// rather than left to make the comparison vacuous, and nothing else is.
#[test]
fn two_exports_of_unchanged_data_differ_only_in_when_they_were_taken() {
    let guard = TempDir::new("archive_stable");
    let db = open(guard.path());
    populated(&db, "SALES");

    let source = Source::Account {
        account: "SALES".to_string(),
    };
    let first = archive_at(&guard, "first");
    let second = archive_at(&guard, "second");
    db.export_to_path(&source, &first).unwrap();
    db.export_to_path(&source, &second).unwrap();

    let a = blank_timestamp(&std::fs::read(&first).unwrap());
    let b = blank_timestamp(&std::fs::read(&second).unwrap());
    assert_eq!(a.len(), b.len(), "the two archives are the same size");
    assert_eq!(
        a, b,
        "two exports of the same data are the same archive once the moment they were taken is set aside"
    );
}

/// Replaces the digits of `taken_millis` with zeroes, in place, and drops the
/// trailing CRC32C - which is over those digits and so differs with them.
///
/// Byte-wise rather than through a lossy string, so a record that is not UTF-8
/// cannot be normalised into matching one that is. Written by hand because a
/// regex crate for one substitution in one test is a dependency the project
/// would carry for ever.
fn blank_timestamp(archive: &[u8]) -> Vec<u8> {
    const FIELD: &[u8] = b"\"taken_millis\":";
    let mut out = archive[..archive.len() - 4].to_vec();
    let Some(at) = out.windows(FIELD.len()).position(|window| window == FIELD) else {
        panic!("every archive records when it was taken");
    };
    let mut cursor = at + FIELD.len();
    let digits = cursor;
    while out.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    assert!(cursor > digits, "and records it as a number");
    for byte in &mut out[digits..cursor] {
        *byte = b'0';
    }
    out
}
