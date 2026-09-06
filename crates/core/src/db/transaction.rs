//! A set of record writes and deletes that lands whole or not at all.
//!
//! Every write the engine makes is safe on its own - a group frame is
//! checksummed, written tmp-then-rename and flushed under the file's sync
//! policy - but nothing used to span records. A caller with two records that
//! must change together wrote one, then the other, and hoped; a crash in
//! between left a state no single operation created and no single operation
//! could detect.
//!
//! This module is the format half of the answer. [`Change`] is one write or
//! delete, and an **intent** is the whole set of them written to disk *before*
//! any of it is applied. What the engine does with them - the locks, the order,
//! the replay - is [`crate::db::engine::transaction`].
//!
//! # Why an intent, and why it is enough
//!
//! The set is applied by writing every file it touches, and a crash can land
//! between two of those writes. There is no way to make several file renames
//! one atomic act, so the recovery is forward rather than backward: the intent
//! records what the whole set is, and whatever is found on disk afterwards, the
//! set is applied again from the intent on the next open.
//!
//! That works because every change is idempotent by construction - a write
//! stores given bytes at a given key, a delete removes a key - so applying the
//! set twice leaves exactly what applying it once does. The two rules that keep
//! it true:
//!
//! - the intent is on disk, fsynced, **before** the first file is written, so a
//!   crash either finds a complete intent or finds none and nothing applied;
//! - the intent is removed **after** every file it names is fsynced, so it
//!   never survives to replay stale bytes over a newer write of the same key.
//!
//! # The frame
//!
//! Written tmp-then-rename with a CRC32C trailer, the same discipline as a
//! group file, for the same reason: a torn tail must be distinguishable from a
//! short set. A file that does not decode was never renamed into place and so
//! names a transaction that was never committed - it is discarded, not
//! repaired.
//!
//! ```text
//! [magic "SRPTXN01"][account_len u64][account]
//! [change_count u64]
//!   per change: [op u8][flags u8][file_len u64][file][key_len u64][key]
//!               and, for a write, [data_len u64][record bytes]
//! [crc32c u32]   - over every byte before it
//! ```

use crate::db::hashfile::{crc32c, sync_dir};
use crate::db::models::Record;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The directory, under the database's own storage directory, that holds the
/// intents of transactions currently being applied.
///
/// Hidden rather than named like a file, because an account whose directory
/// *is* the storage directory would otherwise list it as one of its files. The
/// account listing skips names that start with a dot for exactly this reason.
pub const LOG_DIR: &str = ".txn";

/// Extension of a complete intent. A `.tmp` beside it is a write that never
/// finished and is swept like any other.
const INTENT_SUFFIX: &str = ".intent";

const MAGIC: [u8; 8] = *b"SRPTXN01";

/// Changes one transaction may carry.
///
/// A bound rather than no bound, because the whole set is held in memory, every
/// file it names is locked at once and the intent is one write: a set large
/// enough to matter is a bulk load, which is a different operation with
/// different guarantees. A set over the limit is refused, never truncated.
pub const MAX_CHANGES: usize = 1000;

/// What a change does to the key it names.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeOp {
    /// Store these bytes at the key, replacing whatever is there.
    Write(Record),
    /// Remove the key, whether or not it is there.
    Delete,
}

/// One write or delete of one record.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// File within the transaction's account. A transaction never spans
    /// accounts, so a change does not name one.
    pub file: String,
    pub key: String,
    /// Operate on the file's dictionary section rather than on its records,
    /// exactly as `is_dict` does on `WRITE` and `DELETE`.
    pub is_dict: bool,
    pub op: ChangeOp,
}

impl Change {
    pub fn write(file: &str, key: &str, record: Record) -> Self {
        Change {
            file: file.to_string(),
            key: key.to_string(),
            is_dict: false,
            op: ChangeOp::Write(record),
        }
    }

    pub fn delete(file: &str, key: &str) -> Self {
        Change {
            file: file.to_string(),
            key: key.to_string(),
            is_dict: false,
            op: ChangeOp::Delete,
        }
    }

    /// The same change against the file's dictionary section.
    pub fn in_dictionary(mut self) -> Self {
        self.is_dict = true;
        self
    }

    /// What this change writes over: two changes with the same target in one
    /// set are refused, since nothing in the set says which of them wins.
    pub fn target(&self) -> (&str, &str, bool) {
        (self.file.as_str(), self.key.as_str(), self.is_dict)
    }
}

/// A decoded intent: the account it applies to and the whole set.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    pub account: String,
    pub changes: Vec<Change>,
}

const OP_WRITE: u8 = 0;
const OP_DELETE: u8 = 1;
/// Bit 0 of a change's flags: the change is against the dictionary section.
const FLAG_DICT: u8 = 1;

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// The bytes an intent is written as. See the module documentation for the
/// frame.
pub fn encode(account: &str, changes: &[Change]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    put_bytes(&mut out, account.as_bytes());
    out.extend_from_slice(&(changes.len() as u64).to_le_bytes());
    for change in changes {
        match &change.op {
            ChangeOp::Write(_) => out.push(OP_WRITE),
            ChangeOp::Delete => out.push(OP_DELETE),
        }
        out.push(if change.is_dict { FLAG_DICT } else { 0 });
        put_bytes(&mut out, change.file.as_bytes());
        put_bytes(&mut out, change.key.as_bytes());
        if let ChangeOp::Write(record) = &change.op {
            put_bytes(&mut out, &record.to_bytes());
        }
    }
    let checksum = crc32c(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    out
}

/// Reads back what [`encode`] wrote, or `None` for anything that is not an
/// intact intent.
///
/// Every failure is one answer - "this is not a committed transaction" - so
/// they are not told apart: a truncated file, a bad checksum and a file from a
/// format this build does not know are all discarded rather than guessed at.
pub fn decode(bytes: &[u8]) -> Option<Intent> {
    if bytes.len() < MAGIC.len() + 4 || bytes[..MAGIC.len()] != MAGIC {
        return None;
    }
    let (body, trailer) = bytes.split_at(bytes.len() - 4);
    if crc32c(body) != u32::from_le_bytes(trailer.try_into().ok()?) {
        return None;
    }

    let mut cursor = Cursor {
        bytes: body,
        at: MAGIC.len(),
    };
    let account = cursor.text()?;
    let count = cursor.u64()? as usize;
    // The count is read from a file: a corrupt one must not be able to ask for
    // an allocation the size it claims before the frames are seen to be there.
    if count > MAX_CHANGES {
        return None;
    }
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        let op = cursor.byte()?;
        let flags = cursor.byte()?;
        let file = cursor.text()?;
        let key = cursor.text()?;
        let op = match op {
            OP_WRITE => ChangeOp::Write(Record::from_bytes(cursor.slice()?)),
            OP_DELETE => ChangeOp::Delete,
            _ => return None,
        };
        changes.push(Change {
            file,
            key,
            is_dict: flags & FLAG_DICT != 0,
            op,
        });
    }
    // Trailing bytes mean this is not the frame it claims to be.
    cursor.at.eq(&body.len()).then_some(Intent { account, changes })
}

/// A position in an intent's bytes. Every read is bounds checked and answers
/// `None` rather than panicking, because the bytes come off a disk that may
/// have torn them.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn u64(&mut self) -> Option<u64> {
        let end = self.at.checked_add(8)?;
        let value = u64::from_le_bytes(self.bytes.get(self.at..end)?.try_into().ok()?);
        self.at = end;
        Some(value)
    }

    fn slice(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u64()?).ok()?;
        let end = self.at.checked_add(len)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.slice()?.to_vec()).ok()
    }
}

/// Where the intents live for a database rooted at `storage_dir`.
///
/// Beside `accounts.reg` rather than inside an account's own directory: two
/// accounts may be registered against the same directory, and the replay reads
/// one log for the whole database anyway.
pub fn log_dir(storage_dir: &str) -> PathBuf {
    Path::new(storage_dir).join(LOG_DIR)
}

/// Names an intent so that a directory listing sorted by name is the order the
/// intents were written in.
///
/// Milliseconds first, then a counter that makes two intents from one process
/// distinct even inside one millisecond. Two *processes* writing the same key
/// in the same millisecond are not ordered by this - but nor are they ordered
/// by anything else in the engine, so nothing is being given up.
fn intent_name() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    format!(
        "{:020}-{:010}-{}{}",
        millis,
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        std::process::id(),
        INTENT_SUFFIX
    )
}

/// Puts the set on disk and does not return until it is really there.
///
/// The rename is what publishes it, and the fsyncs are what make the promise
/// worth anything: the bytes before the rename, the directory after it. A
/// crash at any point leaves either no intent at all or a complete one.
pub fn write_intent(storage_dir: &str, account: &str, changes: &[Change]) -> io::Result<PathBuf> {
    let dir = log_dir(storage_dir);
    fs::create_dir_all(&dir)?;
    let path = dir.join(intent_name());
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&encode(account, changes))?;
        file.flush()?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    sync_dir(&dir)?;
    Ok(path)
}

/// The intent at `path`, or `None` if it is not one - see [`decode`].
pub fn read_intent(path: &Path) -> Option<Intent> {
    decode(&fs::read(path).ok()?)
}

/// Retires an intent whose changes are all on disk.
///
/// The directory is fsynced after the unlink for the same reason it is after
/// the rename: until it is, the removal is a promise about the page cache, and
/// a crash could bring the intent back to replay over whatever was written
/// next.
pub fn remove_intent(storage_dir: &str, path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    sync_dir(&log_dir(storage_dir))
}

/// Every intent waiting to be replayed, oldest first, sweeping the debris of a
/// write that never finished on the way.
///
/// A missing log directory is the ordinary case - most databases never write an
/// intent at all - so it answers "nothing pending" rather than failing.
pub fn pending(storage_dir: &str) -> Vec<PathBuf> {
    let dir = log_dir(storage_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut intents = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(|ext| ext.to_str()) {
            // Never renamed into place, so it names nothing that was committed.
            Some("tmp") => {
                let _ = fs::remove_file(&path);
            }
            Some("intent") => intents.push(path),
            _ => {}
        }
    }
    intents.sort();
    intents
}
