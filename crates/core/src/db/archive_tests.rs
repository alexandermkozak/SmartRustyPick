//! The archive format on its own: what it writes, what it refuses, and the
//! exact ways a damaged one is told apart from a short one.
//!
//! Nothing here touches the engine. An archive that decodes is the foundation
//! everything in `engine/archive.rs` stands on, so it is pinned by itself
//! first - a round trip that passes only because the same engine wrote and read
//! it proves much less.

use crate::db::archive::*;
use crate::db::error::DbError;
use std::io::Read;

fn entry(name: &str) -> FileEntry {
    FileEntry {
        account: "SALES".to_string(),
        name: name.to_string(),
        kind: FileKind::File,
        durable: false,
        autokey: false,
        visibility_timeout: None,
        max_deliveries: None,
        path: None,
        indexes: Vec::new(),
    }
}

fn manifest(files: Vec<FileEntry>) -> Manifest {
    Manifest {
        archive_format: CURRENT,
        storage_format: crate::db::format::CURRENT,
        taken_millis: 1_757_692_800_000,
        source: Source::Account {
            account: "SALES".to_string(),
        },
        files,
    }
}

/// Collects everything an archive carries, so a test can compare what came out
/// against what went in rather than counting.
#[derive(Default)]
struct Collect {
    records: Vec<(String, String, RecordKind, Vec<u8>)>,
    began: Vec<String>,
    ended: Vec<String>,
}

impl Sink for Collect {
    fn begin_file(&mut self, _index: usize, entry: &FileEntry) -> crate::db::DbResult<()> {
        self.began.push(entry.name.clone());
        Ok(())
    }

    fn record(
        &mut self,
        entry: &FileEntry,
        kind: RecordKind,
        key: &str,
        len: u64,
        body: &mut dyn Read,
    ) -> crate::db::DbResult<()> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes)?;
        assert_eq!(bytes.len() as u64, len, "the announced length is what arrives");
        self.records.push((entry.name.clone(), key.to_string(), kind, bytes));
        Ok(())
    }

    fn end_file(&mut self, entry: &FileEntry) -> crate::db::DbResult<()> {
        self.ended.push(entry.name.clone());
        Ok(())
    }
}

/// An archive of two files, the second of them empty, with a dictionary entry
/// and a record whose bytes are not text.
fn sample() -> Vec<u8> {
    let mut out = Vec::new();
    let mut writer = Writer::begin(&mut out, manifest(vec![entry("ORDERS"), entry("EMPTY")])).unwrap();

    writer.begin_file().unwrap();
    writer
        .record(RecordKind::Dictionary, "TOTAL", b"D\xfe3\xfeTotal")
        .unwrap();
    writer
        .record(RecordKind::Data, "1001", b"ACME\xfe250\xfe\xff\x00\xfd")
        .unwrap();
    writer.record(RecordKind::Data, "1002", b"").unwrap();
    writer.end_file().unwrap();

    writer.begin_file().unwrap();
    writer.end_file().unwrap();

    let trailer = writer.finish().unwrap();
    assert_eq!(trailer.records(), 2);
    assert_eq!(trailer.dictionary(), 1);
    out
}

#[test]
fn an_archive_round_trips_every_record_it_was_given() {
    let bytes = sample();
    let mut collected = Collect::default();
    let summary = read(&bytes[..], &mut collected).unwrap();

    assert_eq!(summary.manifest.archive_format, CURRENT);
    assert_eq!(summary.archive_bytes, bytes.len() as u64);
    assert_eq!(collected.began, vec!["ORDERS", "EMPTY"]);
    assert_eq!(collected.ended, vec!["ORDERS", "EMPTY"], "every file is closed");

    // Including the record that is not UTF-8 and the one that is empty: an
    // archive is a byte container, and a backup that quietly normalises is not
    // a backup.
    assert_eq!(
        collected.records,
        vec![
            (
                "ORDERS".to_string(),
                "TOTAL".to_string(),
                RecordKind::Dictionary,
                b"D\xfe3\xfeTotal".to_vec()
            ),
            (
                "ORDERS".to_string(),
                "1001".to_string(),
                RecordKind::Data,
                b"ACME\xfe250\xfe\xff\x00\xfd".to_vec()
            ),
            ("ORDERS".to_string(), "1002".to_string(), RecordKind::Data, Vec::new()),
        ]
    );

    // The counts are the trailer's, and they are per file in the manifest's
    // order - which is what lets a caller pair the two by position.
    let counts: Vec<_> = summary.files().map(|(e, c)| (e.name.as_str(), c.records)).collect();
    assert_eq!(counts, vec![("ORDERS", 2), ("EMPTY", 0)]);
}

/// The property the whole format exists for. Every prefix of an archive is
/// refused - there is no length at which a truncated archive starts looking
/// like a complete one.
#[test]
fn every_truncation_of_an_archive_is_refused() {
    let bytes = sample();
    for cut in 0..bytes.len() {
        let refused = verify(&bytes[..cut]).expect_err("a prefix is not an archive");
        assert!(
            matches!(refused, DbError::InvalidRequest(_)),
            "truncation at {} gave {:?}",
            cut,
            refused
        );
    }
    // And the whole of it is fine, so the loop above is testing truncation
    // rather than a format that never decodes.
    verify(&bytes[..]).expect("the untruncated archive verifies");
}

/// A flipped bit anywhere is caught. Not one byte of an archive is outside the
/// checksum except the checksum itself - and altering that is caught too.
#[test]
fn a_single_altered_byte_anywhere_is_caught() {
    let bytes = sample();
    for at in 0..bytes.len() {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x01;
        let refused = verify(&damaged[..]).expect_err("an altered archive is not a usable one");
        assert!(
            matches!(refused, DbError::InvalidRequest(_)),
            "a flip at {} gave {:?}",
            at,
            refused
        );
    }
}

#[test]
fn something_that_is_not_an_archive_is_said_to_be_not_an_archive() {
    let refused = verify(&b"{\"status\":\"OK\"}\n"[..]).unwrap_err();
    assert!(
        refused.to_string().contains("does not start with an archive header"),
        "an operator who named the wrong path needs to be told that, not \
         given a checksum mismatch: {}",
        refused
    );
}

#[test]
fn an_archive_from_a_newer_build_is_refused_by_version_rather_than_misread() {
    let mut head = manifest(vec![]);
    head.archive_format = CURRENT + 1;
    let mut out = Vec::new();
    Writer::begin(&mut out, head).unwrap().finish().unwrap();

    let refused = verify(&out[..]).unwrap_err();
    let message = refused.to_string();
    assert!(message.contains(&(CURRENT + 1).to_string()), "{}", message);
    assert!(
        message.contains(&CURRENT.to_string()),
        "both halves, as the storage stamp does: what the archive is and what this build reads. {}",
        message
    );
}

/// A length is read before the bytes it counts, off an archive whose checksum
/// has not been reached yet. So it is bounded first: a damaged one must become
/// an error, not an allocation.
#[test]
fn an_absurd_document_length_is_refused_rather_than_allocated() {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&u64::MAX.to_le_bytes());
    let refused = verify(&out[..]).unwrap_err();
    assert!(refused.to_string().contains("damaged"), "{}", refused);
}

/// A record streamed out of a reader is byte-identical to one handed over
/// whole. The directory-file path uses the first and every other file uses the
/// second, so they have to agree.
#[test]
fn a_streamed_record_and_a_held_record_produce_the_same_archive() {
    let body: Vec<u8> = (0u16..5000).map(|n| (n % 251) as u8).collect();

    let mut held = Vec::new();
    let mut writer = Writer::begin(&mut held, manifest(vec![entry("SCANS")])).unwrap();
    writer.begin_file().unwrap();
    writer.record(RecordKind::Data, "scan.png", &body).unwrap();
    writer.end_file().unwrap();
    writer.finish().unwrap();

    let mut streamed = Vec::new();
    let mut writer = Writer::begin(&mut streamed, manifest(vec![entry("SCANS")])).unwrap();
    writer.begin_file().unwrap();
    writer
        .record_from(RecordKind::Data, "scan.png", body.len() as u64, &mut &body[..])
        .unwrap();
    writer.end_file().unwrap();
    writer.finish().unwrap();

    assert_eq!(held, streamed);
}

/// A sink that wants none of a record still leaves the stream on the next tag.
/// This is what makes a verify pass over a whole-database archive cost a read
/// rather than a read and a hold.
#[test]
fn a_sink_that_reads_nothing_still_parses_the_rest_of_the_archive() {
    struct Skip(usize);
    impl Sink for Skip {
        fn record(
            &mut self,
            _entry: &FileEntry,
            _kind: RecordKind,
            _key: &str,
            _len: u64,
            _body: &mut dyn Read,
        ) -> crate::db::DbResult<()> {
            self.0 += 1;
            Ok(())
        }
    }

    let mut skipped = Skip(0);
    let summary = read(&sample()[..], &mut skipped).unwrap();
    assert_eq!(skipped.0, 3, "every record was offered");
    assert_eq!(summary.trailer.records(), 2);
}

/// Half a record read is the case in between, and the one a partially
/// interested sink actually hits.
#[test]
fn a_sink_that_reads_part_of_a_record_leaves_the_stream_aligned() {
    struct Nibble(Vec<u8>);
    impl Sink for Nibble {
        fn record(
            &mut self,
            _entry: &FileEntry,
            _kind: RecordKind,
            _key: &str,
            _len: u64,
            body: &mut dyn Read,
        ) -> crate::db::DbResult<()> {
            let mut one = [0u8; 1];
            if body.read(&mut one)? == 1 {
                self.0.push(one[0]);
            }
            Ok(())
        }
    }

    let mut nibbled = Nibble(Vec::new());
    let summary = read(&sample()[..], &mut nibbled).unwrap();
    // The first byte of each non-empty record, and nothing from the empty one.
    assert_eq!(nibbled.0, vec![b'D', b'A']);
    assert_eq!(summary.trailer.records(), 2);
}

/// The writer's own ordering rules. Each of these is a bug in the caller, and
/// an archive that is silently malformed is worth much less than a panic-free
/// refusal at the point the mistake was made.
#[test]
fn the_writer_refuses_an_order_that_would_produce_a_malformed_archive() {
    let mut out = Vec::new();
    let mut writer = Writer::begin(&mut out, manifest(vec![entry("ORDERS")])).unwrap();
    assert!(
        writer.record(RecordKind::Data, "1", b"x").is_err(),
        "a record outside any file"
    );
    writer.begin_file().unwrap();
    assert!(writer.begin_file().is_err(), "two files open at once");

    let mut out = Vec::new();
    let mut writer = Writer::begin(&mut out, manifest(vec![entry("ORDERS"), entry("LINES")])).unwrap();
    writer.begin_file().unwrap();
    writer.end_file().unwrap();
    assert!(
        writer.finish().is_err(),
        "the manifest describes two files and one was written"
    );
}

/// A body that ends early is the export half of truncation: the length is
/// already on the stream when the content runs out, so the archive is abandoned
/// rather than padded.
#[test]
fn a_record_body_that_runs_out_early_fails_the_export() {
    let mut out = Vec::new();
    let mut writer = Writer::begin(&mut out, manifest(vec![entry("SCANS")])).unwrap();
    writer.begin_file().unwrap();
    let refused = writer
        .record_from(RecordKind::Data, "scan.png", 4096, &mut &b"short"[..])
        .unwrap_err();
    assert!(refused.to_string().contains("short"), "{}", refused);
}

/// A queue's policy and an index's exclusions are shape, not records, and they
/// survive the round trip through the manifest.
#[test]
fn a_files_shape_survives_the_manifest() {
    let mut queue = entry("JOBS");
    queue.kind = FileKind::Queue;
    queue.durable = true;
    queue.visibility_timeout = Some(45);
    queue.max_deliveries = Some(7);
    queue.indexes = vec![IndexEntry {
        field: "STATE".to_string(),
        excluded: vec!["DONE".to_string()],
    }];

    let mut out = Vec::new();
    let mut writer = Writer::begin(&mut out, manifest(vec![queue.clone()])).unwrap();
    writer.begin_file().unwrap();
    writer.end_file().unwrap();
    writer.finish().unwrap();

    let summary = verify(&out[..]).unwrap();
    assert_eq!(summary.manifest.files[0], queue);

    let attributes = summary.manifest.files[0].attributes();
    assert!(attributes.durable);
    let policy = attributes.queue.expect("a queue comes back a queue");
    assert_eq!(policy.visibility.as_secs(), 45);
    assert_eq!(policy.max_deliveries, 7);
}

/// A directory file's host path is recorded and deliberately not applied. The
/// path belongs to the machine that exported it; honouring it elsewhere either
/// fails or writes into somebody else's directory.
#[test]
fn a_directory_files_path_is_carried_but_not_restored() {
    let mut scans = entry("SCANS");
    scans.kind = FileKind::Directory;
    scans.path = Some("/srv/invoices".to_string());

    let mut out = Vec::new();
    let mut writer = Writer::begin(&mut out, manifest(vec![scans])).unwrap();
    writer.begin_file().unwrap();
    writer.end_file().unwrap();
    writer.finish().unwrap();

    let summary = verify(&out[..]).unwrap();
    let restored = summary.manifest.files[0].clone();
    assert_eq!(
        restored.path.as_deref(),
        Some("/srv/invoices"),
        "the archive still says where it came from"
    );
    let policy = restored.attributes().directory.expect("it is still a directory file");
    assert_eq!(
        policy.explicit_path(),
        None,
        "but a restore puts its records in the default place"
    );
}
