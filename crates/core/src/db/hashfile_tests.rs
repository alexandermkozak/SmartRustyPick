use crate::db::engine::Database;
use crate::db::hashfile;
use crate::db::models::*;
use crate::test_support::{isolated_config, TempDir};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

fn fresh_dir(label: &str) -> TempDir {
    TempDir::new(label)
}

fn record(value: &str) -> Record {
    Record::from_display_string(value)
}

fn sample_map(count: usize) -> HashMap<String, Record> {
    (0..count)
        .map(|i| (format!("K{i:05}"), record(&format!("VALUE{i}"))))
        .collect()
}

#[test]
fn test_modulus_grows_and_shrinks_with_hysteresis() {
    // Small tables stay at the floor.
    assert_eq!(hashfile::target_modulus(0, 16), hashfile::MIN_MODULUS);
    assert_eq!(hashfile::target_modulus(128, 16), 8);
    // Once the load factor is exceeded the modulus doubles.
    assert_eq!(hashfile::target_modulus(129, 16), 16);
    assert_eq!(hashfile::target_modulus(10_000, 16), 1024);

    // Growth is immediate...
    assert_eq!(hashfile::plan_modulus(8, 200, 16), 16);
    // ...but a table that merely dips below its capacity keeps its modulus, so
    // a workload hovering around a boundary does not rehash on every flush.
    assert_eq!(hashfile::plan_modulus(64, 600, 16), 64);
    // Only a substantial shrink is acted on.
    assert_eq!(hashfile::plan_modulus(64, 10, 16), 8);
}

#[test]
fn test_keys_spread_evenly_over_groups() {
    let modulus = 64;
    let mut counts = vec![0usize; modulus as usize];
    for i in 0..6_400 {
        counts[hashfile::group_of(&format!("K{i:05}"), modulus) as usize] += 1;
    }
    let min = *counts.iter().min().unwrap();
    let max = *counts.iter().max().unwrap();
    // A uniform hash would give 100 per group; allow a wide band but catch a
    // hash that piles keys into a few groups, which would reintroduce the
    // rewrite-everything cost this format exists to avoid.
    assert!(min > 50, "group underfilled: {min}");
    assert!(max < 200, "group overfilled: {max}");
}

#[test]
fn test_round_trip_and_incremental_write() {
    let guard = fresh_dir("hashfile_roundtrip");
    let dir = guard.path();
    let section = format!("{}/data", dir);

    let mut map = sample_map(500);
    let meta = hashfile::save(&section, &map, hashfile::SectionMeta::empty(), None, 16).unwrap();
    assert_eq!(meta.records, 500);
    assert!(meta.modulus >= 32, "modulus should scale with the table: {}", meta.modulus);

    let mut loaded = HashMap::new();
    let loaded_meta = hashfile::load(&section, &mut loaded).unwrap();
    assert_eq!(loaded_meta, meta);
    assert_eq!(loaded.len(), 500);
    assert_eq!(loaded["K00042"], record("VALUE42"));

    // Change one record and flush incrementally.
    map.insert("K00042".to_string(), record("CHANGED"));
    let mut dirty = HashSet::new();
    dirty.insert("K00042".to_string());
    let meta2 = hashfile::save(&section, &map, meta, Some(&dirty), 16).unwrap();
    assert_eq!(meta2.version, meta.version + 1);
    assert_eq!(meta2.modulus, meta.modulus);

    let mut reloaded = HashMap::new();
    hashfile::load(&section, &mut reloaded).unwrap();
    assert_eq!(reloaded.len(), 500);
    assert_eq!(reloaded["K00042"], record("CHANGED"));
    assert_eq!(reloaded["K00001"], record("VALUE1"));
}

#[test]
fn test_incremental_write_touches_one_group_only() {
    let guard = fresh_dir("hashfile_one_group");
    let dir = guard.path();
    let section = format!("{}/data", dir);

    let mut map = sample_map(1_000);
    let meta = hashfile::save(&section, &map, hashfile::SectionMeta::empty(), None, 16).unwrap();

    let group_dir = hashfile::section_dir(&section);
    let before: HashMap<String, Vec<u8>> = fs::read_dir(&group_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('g'))
        .map(|e| (e.file_name().to_string_lossy().to_string(), fs::read(e.path()).unwrap()))
        .collect();
    assert!(before.len() > 8, "expected many groups, got {}", before.len());

    map.insert("K00042".to_string(), record("CHANGED"));
    let mut dirty = HashSet::new();
    dirty.insert("K00042".to_string());
    hashfile::save(&section, &map, meta, Some(&dirty), 16).unwrap();

    let expected = format!("g{:08x}", hashfile::group_of("K00042", meta.modulus));
    let mut changed = Vec::new();
    for (name, content) in &before {
        let now = fs::read(group_dir.join(name)).unwrap();
        if &now != content {
            changed.push(name.clone());
        }
    }
    // This is the whole point of the format: a write costs one group, not the
    // entire table, no matter how large the table has grown.
    assert_eq!(changed, vec![expected]);
}

#[test]
fn test_rehash_preserves_every_record() {
    let guard = fresh_dir("hashfile_rehash");
    let dir = guard.path();
    let section = format!("{}/data", dir);

    let mut map = HashMap::new();
    let mut meta = hashfile::SectionMeta::empty();
    for batch in 0..20 {
        for i in 0..100 {
            let key = format!("K{:05}", batch * 100 + i);
            map.insert(key.clone(), record(&format!("V{}", batch * 100 + i)));
        }
        let dirty: HashSet<String> = map.keys().cloned().collect();
        meta = hashfile::save(&section, &map, meta, Some(&dirty), 16).unwrap();
    }
    assert_eq!(meta.records, 2_000);
    assert_eq!(meta.modulus, hashfile::target_modulus(2_000, 16));

    let mut loaded = HashMap::new();
    hashfile::load(&section, &mut loaded).unwrap();
    assert_eq!(loaded.len(), 2_000);
    assert_eq!(loaded["K01999"], record("V1999"));

    // Deleting most of the table shrinks it back without losing the rest.
    map.retain(|k, _| k < &"K00010".to_string());
    let meta = hashfile::save(&section, &map, meta, None, 16).unwrap();
    assert_eq!(meta.modulus, hashfile::MIN_MODULUS);
    let mut loaded = HashMap::new();
    hashfile::load(&section, &mut loaded).unwrap();
    assert_eq!(loaded.len(), 10);
}

#[test]
fn test_legacy_flat_file_is_migrated_on_first_write() {
    let guard = fresh_dir("hashfile_migration");
    let base = guard.path();
    let table_dir = format!("{}/LEGACY", base);
    fs::create_dir_all(&table_dir).unwrap();

    // Write a table in the pre-hashfile flat format by hand.
    let mut flat = Vec::new();
    for i in 0..50 {
        let key = format!("OLD{i}");
        flat.extend_from_slice(&(key.len() as u64).to_le_bytes());
        flat.extend_from_slice(key.as_bytes());
        let data = record(&format!("LEGACY{i}")).to_bytes();
        flat.extend_from_slice(&(data.len() as u64).to_le_bytes());
        flat.extend_from_slice(&data);
    }
    fs::write(format!("{}/data", table_dir), &flat).unwrap();
    fs::write(format!("{}/dict", table_dir), b"").unwrap();

    let mut db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account("LEG", Some(base)).unwrap();
    db.logto("LEG").unwrap();

    {
        let table_handle = db.get_table_mut("LEGACY").unwrap();
        let mut table = table_handle.write();
        assert_eq!(table.records.len(), 50, "legacy records must still be readable");
        assert!(table.legacy_data);
        table.insert_record("NEW", record("FRESH"));
    }
    db.save().unwrap();

    let section = format!("{}/data", table_dir);
    assert!(hashfile::is_hashfile(&section), "table should have been converted");
    assert!(!Path::new(&section).is_file(), "the flat file should be gone");

    // Re-read through a second handle to prove the data survived the move.
    db.clear_loaded_tables();
    let table_handle = db.get_table_mut("LEGACY").unwrap();
    let mut table = table_handle.write();
    assert_eq!(table.records.len(), 51);
    assert_eq!(table.records["OLD7"], record("LEGACY7"));
    assert_eq!(table.records["NEW"], record("FRESH"));
    assert!(!table.legacy_data);
}

#[test]
fn test_deferred_flush_batches_writes() {
    let guard = fresh_dir("hashfile_deferred");
    let base = guard.path();
    let mut db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account("BUF", Some(base)).unwrap();
    db.logto("BUF").unwrap();
    db.create_table("T").unwrap();

    // A large batch and a long interval, so nothing flushes on its own.
    db.durable_writes = false;
    db.flush_max_pending = 1_000;
    db.flush_interval = std::time::Duration::from_secs(3_600);

    for i in 0..10 {
        db.get_table_mut("T").unwrap().write().insert_record(&format!("K{i}"), record("V"));
        db.note_write().unwrap();
    }
    assert!(db.has_pending_writes(), "writes should still be buffered");
    assert_eq!(db.pending_write_count(), 10);

    let section = format!("{}/T/data", base);
    let mut on_disk = HashMap::new();
    hashfile::load(&section, &mut on_disk).unwrap();
    assert!(on_disk.is_empty(), "nothing should have reached disk yet");

    // Hitting the batch size flushes.
    db.flush_max_pending = 11;
    db.get_table_mut("T").unwrap().write().insert_record("K10", record("V"));
    db.note_write().unwrap();
    assert!(!db.has_pending_writes());
    assert_eq!(db.pending_write_count(), 0);

    let mut on_disk = HashMap::new();
    hashfile::load(&section, &mut on_disk).unwrap();
    assert_eq!(on_disk.len(), 11);
}

#[test]
fn test_durable_writes_flush_immediately() {
    let guard = fresh_dir("hashfile_durable");
    let base = guard.path();
    let mut db = Database::new(base, Some(isolated_config())).unwrap();
    db.create_account("DUR", Some(base)).unwrap();
    db.logto("DUR").unwrap();
    db.create_table("T").unwrap();
    db.durable_writes = true;

    db.get_table_mut("T").unwrap().write().insert_record("K1", record("V1"));
    db.note_write().unwrap();
    assert!(!db.has_pending_writes());

    let mut on_disk = HashMap::new();
    hashfile::load(&format!("{}/T/data", base), &mut on_disk).unwrap();
    assert_eq!(on_disk.len(), 1);
}

/// Path of the first non-empty group of a section.
fn a_group(section: &str) -> std::path::PathBuf {
    let dir = hashfile::section_dir(section);
    let mut groups: Vec<std::path::PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with('g'))
        .collect();
    groups.sort();
    groups.into_iter().next().expect("the section should have at least one group")
}

fn truncate(path: &std::path::Path, drop_bytes: u64) {
    let len = fs::metadata(path).unwrap().len();
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(len - drop_bytes).unwrap();
}

#[test]
fn test_torn_group_is_detected_not_reported_as_missing_records() {
    let guard = fresh_dir("hashfile_torn_group");
    let dir = guard.path();
    let section = format!("{}/data", dir);
    let map = sample_map(200);
    hashfile::save(&section, &map, hashfile::SectionMeta::empty(), None, 16).unwrap();

    // A write that reached the filesystem only in part: the tail of the group,
    // trailer included, never made it.
    let group = a_group(&section);
    let before = fs::metadata(&group).unwrap().len();
    truncate(&group, before / 2);

    let mut loaded = HashMap::new();
    let err = hashfile::load(&section, &mut loaded).expect_err("a torn group must not load quietly");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("Corrupt group file"), "unhelpful error: {err}");
}

#[test]
fn test_flipped_byte_in_a_group_is_detected_by_the_checksum() {
    let guard = fresh_dir("hashfile_bitflip");
    let dir = guard.path();
    let section = format!("{}/data", dir);
    let map = sample_map(200);
    hashfile::save(&section, &map, hashfile::SectionMeta::empty(), None, 16).unwrap();

    // Damage a record's payload while leaving every length intact, so only the
    // checksum can tell that the group is no longer what was written.
    let group = a_group(&section);
    let mut bytes = fs::read(&group).unwrap();
    let victim = bytes.len() / 2;
    bytes[victim] ^= 0xFF;
    fs::write(&group, &bytes).unwrap();

    let mut loaded = HashMap::new();
    let err = hashfile::load(&section, &mut loaded).expect_err("a damaged group must not load");
    assert!(err.to_string().contains("checksum mismatch"), "unhelpful error: {err}");
    assert!(loaded.is_empty(), "a rejected group must not leave records behind");
}

#[test]
fn test_truncated_meta_is_detected() {
    let guard = fresh_dir("hashfile_torn_meta");
    let dir = guard.path();
    let section = format!("{}/data", dir);
    let map = sample_map(50);
    hashfile::save(&section, &map, hashfile::SectionMeta::empty(), None, 16).unwrap();

    // Half a `meta` is the dangerous case: a surviving but wrong modulus makes
    // records hash into groups that do not hold them, which reads back as
    // silent data loss rather than as an error.
    let meta = hashfile::section_dir(&section).join("meta");
    let len = fs::metadata(&meta).unwrap().len();
    truncate(&meta, len / 2);

    let mut loaded = HashMap::new();
    let err = hashfile::load(&section, &mut loaded).expect_err("a torn meta must not load");
    assert!(err.to_string().contains("Corrupt section metadata"), "unhelpful error: {err}");
}

#[test]
fn test_stale_tmp_file_is_cleaned_up_and_never_read() {
    let guard = fresh_dir("hashfile_stale_tmp");
    let dir = guard.path();
    let section = format!("{}/data", dir);
    let mut map = sample_map(50);
    let meta = hashfile::save(&section, &map, hashfile::SectionMeta::empty(), None, 16).unwrap();

    // What a crash between `create` and `rename` leaves behind.
    let section_dir = hashfile::section_dir(&section);
    fs::write(section_dir.join("g00000000.tmp"), b"half a group").unwrap();
    fs::write(section_dir.join("meta.tmp"), b"half a meta").unwrap();

    let mut loaded = HashMap::new();
    hashfile::load(&section, &mut loaded).unwrap();
    assert_eq!(loaded.len(), 50, "a leftover temporary file is not part of the section");

    map.insert("K00001".to_string(), record("CHANGED"));
    let dirty: HashSet<String> = ["K00001".to_string()].into_iter().collect();
    hashfile::save(&section, &map, meta, Some(&dirty), 16).unwrap();

    let leftovers: Vec<String> = fs::read_dir(&section_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "stale temporary files should be swept: {:?}", leftovers);
}

#[test]
fn test_meta_is_written_after_the_groups_it_describes() {
    let guard = fresh_dir("hashfile_ordering");
    let dir = guard.path();
    let section = format!("{}/data", dir);
    let map = sample_map(300);
    let meta = hashfile::save_with_fsync(
        &section,
        &map,
        hashfile::SectionMeta::empty(),
        None,
        16,
        hashfile::FsyncPolicy::Always,
    )
        .unwrap();

    // `meta` names the modulus the group files implement, so it must never be
    // the older file of the two: a `meta` from the future describes data that
    // may not be on disk yet.
    let section_dir = hashfile::section_dir(&section);
    let meta_time = fs::metadata(section_dir.join("meta")).unwrap().modified().unwrap();
    for group in 0..meta.modulus {
        let path = section_dir.join(format!("g{:08x}", group));
        if let Ok(stat) = fs::metadata(&path) {
            assert!(
                stat.modified().unwrap() <= meta_time,
                "{} was written after meta",
                path.display()
            );
        }
    }
    assert!(meta.checksums, "a full rewrite puts a trailer on every group");
}

#[test]
fn test_a_group_without_a_trailer_still_loads_before_the_first_full_rewrite() {
    let guard = fresh_dir("hashfile_pre_checksum");
    let dir = guard.path();
    let section = format!("{}/data", dir);
    let section_dir = hashfile::section_dir(&section);
    fs::create_dir_all(&section_dir).unwrap();

    // A section as an older version left it: no trailer, no `checksums` flag.
    let mut frames = Vec::new();
    for i in 0..3 {
        let key = format!("OLD{i}");
        frames.extend_from_slice(&(key.len() as u64).to_le_bytes());
        frames.extend_from_slice(key.as_bytes());
        let data = record(&format!("V{i}")).to_bytes();
        frames.extend_from_slice(&(data.len() as u64).to_le_bytes());
        frames.extend_from_slice(&data);
    }
    for group in 0..hashfile::MIN_MODULUS {
        fs::write(section_dir.join(format!("g{:08x}", group)), if group == 0 { &frames[..] } else { &[][..] }).unwrap();
    }
    fs::write(section_dir.join("meta"), b"version=7\nmodulus=8\nrecords=3\n").unwrap();

    let mut loaded = HashMap::new();
    let meta = hashfile::load(&section, &mut loaded).unwrap();
    assert!(!meta.checksums);
    assert_eq!(loaded.len(), 3, "an upgrade must not declare an intact section corrupt");
}

#[test]
fn test_another_process_sees_flushed_changes() {
    let guard = fresh_dir("hashfile_visibility");
    let base = guard.path();
    let mut writer = Database::new(base, Some(isolated_config())).unwrap();
    writer.create_account("VIS", Some(base)).unwrap();
    writer.logto("VIS").unwrap();
    writer.create_table("T").unwrap();
    writer.get_table_mut("T").unwrap().write().insert_record("K1", record("FIRST"));
    writer.save().unwrap();

    let mut reader = Database::new(base, Some(isolated_config())).unwrap();
    reader.logto("VIS").unwrap();
    assert_eq!(reader.get_table_mut("T").unwrap().write().records.len(), 1);

    writer.get_table_mut("T").unwrap().write().insert_record("K2", record("SECOND"));
    writer.save().unwrap();

    // The meta file's flush counter changes on every write, so the reader's
    // cached snapshot is detected as stale even within a filesystem timestamp
    // granularity.
    let table_handle = reader.get_table_mut("T").unwrap();
    let mut table = table_handle.write();
    assert_eq!(table.records.len(), 2);
    assert_eq!(table.records["K2"], record("SECOND"));
}
