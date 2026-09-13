//! The archive: a database's records and the shape around them, in a form that
//! travels.
//!
//! # What this is not
//!
//! It is deliberately **not** a copy of the storage directory. Copying
//! `db_storage/` is the thing that does not work - writes are buffered for up
//! to `flush_interval_ms`, a flush rewrites every changed group and *then*
//! rewrites `meta`, and a copy that lands in the middle of that sequence gets
//! groups and a `meta` that disagree. Nothing coordinates such a copy across
//! the files of an account either, so even a clean per-file copy is not a
//! coherent account.
//!
//! It is also not a copy of the hashfile *layout*. The modulus is a property of
//! the deployment that holds the data - how many records it has and what its
//! `records_per_group` is - not of the data itself. Carrying it across would
//! restore a file laid out for the machine it came from. An archive carries
//! records under their keys and lets the target rehash them, which is what
//! makes restoring into a different deployment, or into a second account beside
//! the original, an ordinary thing to do rather than a special case.
//!
//! # The frame
//!
//! ```text
//! [magic "SRPARC01"]
//! [manifest_len u64][manifest JSON]        - the shape: which files, and what they are
//!   per file, in the manifest's order:
//!     repeated [tag u8]:
//!       1 -> a data record:      [key_len u64][key][data_len u64][bytes]
//!       2 -> a dictionary entry: the same
//!       0 -> end of this file's records
//! [tag 0xFF]
//! [trailer_len u64][trailer JSON]          - the counts: what was actually written
//! [crc32c u32]                             - over every byte before it
//! ```
//!
//! Tmp-then-rename with a checksum trailer is the discipline every other format
//! here uses - a group file, a transaction intent - and for the same reason: a
//! torn tail has to be distinguishable from a short archive. An archive that
//! does not decode was never a backup, and saying so is the whole of its value.
//!
//! # Why the shape leads and the counts follow
//!
//! The manifest is at the head because a restore needs it before it can do
//! anything: it has to create a file, with the right type and flags, before it
//! has anywhere to put the first record. The counts cannot be up there with it.
//!
//! An ordinary file is exported under its own write lock, so its record count
//! is known before a byte is written. A [directory file](crate::db::directory)
//! is the exception that decides the format: it has **no table**, and therefore
//! no table lock - deliberately, because reading a forty megabyte record must
//! not block every writer to that file for the length of the read. So its
//! records can be added to and removed from underneath an export, and how many
//! were actually written is not knowable until the last one has been. Putting
//! the counts in a trailer is what lets the archive state them exactly rather
//! than state an intention and hope.
//!
//! That is also why the body is tagged rather than counted. A tag per record
//! needs no number in front of the run, so an archive streams to a socket as
//! readily as to a file - nothing has to be seeked back to and patched once the
//! run has ended.
//!
//! # Why a reader never applies as it parses
//!
//! The checksum is over the whole archive, so an archive is only known to be
//! good at its last four bytes. Applying records as they are read would mean a
//! truncated archive had already half-restored itself by the time the
//! truncation was found - the one outcome worth more than all the others to
//! avoid, because a half-restored account looks like a restored one.
//!
//! So an import is two passes over a **seekable** source: verify, then apply.
//! Which in turn is why the streamed form spools to a temporary file before it
//! imports anything, exactly as an inbound
//! [raw transfer](crate::server::transfer) does with a record body.

use crate::db::error::{DbError, DbResult};
use crate::db::hashfile::Crc32c;
use crate::db::models::{DirectoryPolicy, FileAttributes};
use crate::db::queue::QueuePolicy;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Marks the start of an archive, and tells one from any other file a path
/// might name before a byte of it is trusted.
pub const MAGIC: [u8; 8] = *b"SRPARC01";

/// The archive format this build writes.
///
/// Distinct from [`crate::db::format::CURRENT`], which versions the *storage
/// directory*. The two move independently on purpose: an archive outlives the
/// deployment that wrote it and is the thing you reach for when a storage
/// directory cannot be opened at all, so tying its readability to the storage
/// format would defeat the reason it exists.
pub const CURRENT: u32 = 1;

/// The oldest archive this build reads. Raising it strands archives already
/// taken, which for a backup format is a far heavier act than it is for a
/// storage directory - the whole point of one is that it is still readable
/// later.
pub const OLDEST_SUPPORTED: u32 = 1;

/// The conventional extension. Nothing enforces it; the magic is what decides
/// whether a path holds an archive.
pub const EXTENSION: &str = "srp";

/// Bytes moved per read when a record is streamed rather than held. Matches the
/// chunk the raw transfer path uses, for the same reason: large enough that a
/// 64 MiB record is a thousand reads, small enough not to be what bounds
/// memory.
const CHUNK: usize = 64 * 1024;

/// The largest manifest or trailer that will be read.
///
/// A length read off a damaged archive is an allocation request from something
/// that is not trustworthy yet - the checksum that would have caught it is at
/// the far end of the file. This is the bound that keeps such a length from
/// becoming an out-of-memory before it can become an error. Generous enough for
/// the shape of any real database: a manifest is a few hundred bytes per file.
const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;

/// The largest key an archive will read, matching what the engine will store.
const MAX_KEY_BYTES: u64 = 255;

const TAG_RECORD: u8 = 1;
const TAG_DICT: u8 = 2;
const TAG_END_OF_FILE: u8 = 0;
const TAG_END_OF_ARCHIVE: u8 = 0xFF;

/// Which of a file's two record sets an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// A record of the file itself.
    Data,
    /// An entry of the file's dictionary.
    Dictionary,
}

impl RecordKind {
    fn tag(self) -> u8 {
        match self {
            RecordKind::Data => TAG_RECORD,
            RecordKind::Dictionary => TAG_DICT,
        }
    }
}

/// What an export was asked for, recorded so a restore can say where the data
/// came from without the operator having to remember which archive is which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "UPPERCASE")]
pub enum Source {
    /// One file of one account.
    File { account: String, file: String },
    /// Every file of one account.
    Account { account: String },
    /// Every account.
    All,
}

impl Source {
    /// One line naming what was exported, for a listing or a log.
    pub fn describe(&self) -> String {
        match self {
            Source::File { account, file } => format!("file {} of account {}", file, account),
            Source::Account { account } => format!("account {}", account),
            Source::All => "the whole database".to_string(),
        }
    }
}

/// What a file is, carried as the word the `DIR` entry uses rather than as a
/// set of flags to be recombined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum FileKind {
    /// Records in a hashed section of the file's own directory.
    File,
    /// Records in arrival order, claimed one at a time.
    Queue,
    /// Records that are the files of a real host directory.
    Directory,
}

/// A file's shape: everything about it that is not one of its records.
///
/// Held as its own document rather than as a serialized [`FileAttributes`] so
/// that the archive does not inherit the `DIR` entry's attribute positions. An
/// archive is read by builds that will have moved those positions around; the
/// words are what should survive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub account: String,
    pub name: String,
    pub kind: FileKind,
    /// Every write to this file is flushed before it is acknowledged.
    #[serde(default)]
    pub durable: bool,
    /// A keyless `WRITE` on this file is given a key rather than refused.
    #[serde(default)]
    pub autokey: bool,
    /// Seconds a claim is held before it lapses. `None` on anything but a
    /// queue, and on a queue that takes the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_timeout: Option<u64>,
    /// Deliveries before a record is dead-lettered. `None` as above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_deliveries: Option<u32>,
    /// The host directory a directory file's records are the files of, when the
    /// file names one rather than taking the default place.
    ///
    /// Carried because it is part of what the file *is*, and deliberately not
    /// applied on import - see [`FileEntry::attributes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The dictionary fields this file carries an index on, and the values each
    /// index skips.
    ///
    /// Definitions only. An index is a derived structure, and rebuilding one
    /// from the restored records is both cheaper than carrying its postings and
    /// the only way to get an index that matches what was actually restored.
    #[serde(default)]
    pub indexes: Vec<IndexEntry>,
}

/// One index definition: the field, and the values it does not index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub field: String,
    #[serde(default)]
    pub excluded: Vec<String>,
}

impl FileEntry {
    /// The file's shape as the engine states it, for creating it on restore.
    ///
    /// A directory file's **path is not carried across**. The one in the
    /// archive is a path on the machine that exported it, and honouring it on
    /// another machine would either fail or - much worse - succeed against
    /// somebody else's directory. A restored directory file gets the default
    /// place inside its own file directory, and its records are written there;
    /// an operator who wants it pointed somewhere else says so afterwards,
    /// deliberately, on the machine where that path means something.
    pub fn attributes(&self) -> FileAttributes {
        match self.kind {
            FileKind::Directory => FileAttributes {
                durable: false,
                queue: None,
                autokey: false,
                directory: Some(DirectoryPolicy::default_path()),
            },
            FileKind::Queue => {
                let mut policy = QueuePolicy::default();
                if let Some(timeout) = self.visibility_timeout {
                    policy.visibility = std::time::Duration::from_secs(timeout);
                }
                if let Some(max) = self.max_deliveries {
                    policy.max_deliveries = max;
                }
                FileAttributes {
                    durable: self.durable,
                    queue: Some(policy),
                    autokey: self.autokey,
                    directory: None,
                }
            }
            FileKind::File => FileAttributes {
                durable: self.durable,
                queue: None,
                autokey: self.autokey,
                directory: None,
            },
        }
    }

    /// The archive's description of a file the engine already has.
    pub fn of(account: &str, name: &str, attributes: &FileAttributes, indexes: Vec<IndexEntry>) -> Self {
        let kind = if attributes.directory.is_some() {
            FileKind::Directory
        } else if attributes.queue.is_some() {
            FileKind::Queue
        } else {
            FileKind::File
        };
        FileEntry {
            account: account.to_string(),
            name: name.to_string(),
            kind,
            durable: attributes.durable,
            autokey: attributes.autokey,
            visibility_timeout: attributes.queue.as_ref().map(|q| q.visibility.as_secs()),
            max_deliveries: attributes.queue.as_ref().map(|q| q.max_deliveries),
            path: attributes
                .directory
                .as_ref()
                .and_then(|d| d.explicit_path().map(str::to_string)),
            indexes,
        }
    }

    /// True when this file's records are host files rather than rows in a
    /// hashed section, and so may be far too large to hold in memory.
    pub fn is_directory(&self) -> bool {
        self.kind == FileKind::Directory
    }
}

/// The head of an archive: what it holds, and what wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The archive format, checked before anything else is read.
    pub archive_format: u32,
    /// The storage format of the directory this came out of, recorded for a
    /// reader trying to work out what it has. Nothing branches on it: an
    /// archive is records and words, not a storage layout, so it restores into
    /// any build that can read the archive format.
    pub storage_format: u32,
    /// Milliseconds since the epoch, at the moment the export was taken.
    pub taken_millis: u64,
    /// What was asked for.
    pub source: Source,
    /// The files, in the order their records follow.
    pub files: Vec<FileEntry>,
}

impl Manifest {
    /// When the export was taken, as UTC text.
    ///
    /// Stored as milliseconds because that is what a machine should compare;
    /// rendered here because "which of these two archives is the newer one" is
    /// a question an operator asks of a listing, and epoch milliseconds are not
    /// an answer to it.
    pub fn taken_utc(&self) -> String {
        let seconds = (self.taken_millis / 1000) as i64;
        let Ok(at) = time::OffsetDateTime::from_unix_timestamp(seconds) else {
            return format!("{}ms since the epoch", self.taken_millis);
        };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            at.year(),
            at.month() as u8,
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        )
    }
}

/// The tail of an archive: what was actually written.
///
/// Separate from the manifest because a [directory file](crate::db::directory)
/// has no lock to hold it still, so its count is only known once its last
/// record has been written. See the module documentation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trailer {
    /// Per file, in the manifest's order.
    pub files: Vec<FileCounts>,
}

impl Trailer {
    /// Records across every file, dictionary entries excluded.
    pub fn records(&self) -> u64 {
        self.files.iter().map(|f| f.records).sum()
    }

    /// Dictionary entries across every file.
    pub fn dictionary(&self) -> u64 {
        self.files.iter().map(|f| f.dictionary).sum()
    }

    /// Record bytes across every file, as they sit in the archive.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

/// What one file contributed to an archive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCounts {
    pub account: String,
    pub name: String,
    pub records: u64,
    pub dictionary: u64,
    /// Record bytes, dictionary entries included. The size of the data rather
    /// than of the archive, which also carries keys and framing.
    pub bytes: u64,
}

/// Everything an archive says about itself, once it has been read through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub manifest: Manifest,
    pub trailer: Trailer,
    /// The archive's own size in bytes, checksum included.
    pub archive_bytes: u64,
}

impl Summary {
    /// The file entry and the counts that go together, paired by position - the
    /// order the format guarantees.
    pub fn files(&self) -> impl Iterator<Item = (&FileEntry, &FileCounts)> {
        self.manifest.files.iter().zip(self.trailer.files.iter())
    }
}

/// A damaged archive, said in the words an operator needs.
///
/// Always an [`InvalidRequest`](DbError::InvalidRequest) rather than an I/O
/// error: the read succeeded and what came back was not an archive, which is a
/// different problem from a disk that would not talk.
fn refused(detail: impl std::fmt::Display) -> DbError {
    DbError::InvalidRequest(format!("Not a usable archive: {}", detail))
}

// ---------------------------------------------------------------- writing ---

/// Writes an archive to anything that takes bytes - a file, or a socket.
///
/// The order is fixed and checked: [`begin_file`](Writer::begin_file) for each
/// file of the manifest in turn, its records, then
/// [`end_file`](Writer::end_file), and finally [`finish`](Writer::finish).
/// Getting it wrong is a bug in the caller, so it is an error rather than a
/// silently malformed archive.
pub struct Writer<W: Write> {
    sink: W,
    crc: Crc32c,
    written: u64,
    manifest: Manifest,
    /// The file being written, as an index into the manifest, or `None` between
    /// files.
    open: Option<usize>,
    /// The next file expected, so the body cannot drift out of step with the
    /// manifest that describes it.
    next: usize,
    counts: Vec<FileCounts>,
}

impl<W: Write> Writer<W> {
    /// Starts an archive: the magic and the manifest.
    pub fn begin(sink: W, manifest: Manifest) -> DbResult<Self> {
        let mut writer = Writer {
            sink,
            crc: Crc32c::new(),
            written: 0,
            manifest,
            open: None,
            next: 0,
            counts: Vec::new(),
        };
        writer.put(&MAGIC)?;
        let document = serde_json::to_vec(&writer.manifest)
            .map_err(|e| DbError::InvalidRequest(format!("The manifest cannot be written: {}", e)))?;
        writer.put_blob(&document)?;
        Ok(writer)
    }

    /// Opens the records of the next file in the manifest.
    pub fn begin_file(&mut self) -> DbResult<&FileEntry> {
        if self.open.is_some() {
            return Err(refused("a file's records were left open"));
        }
        let index = self.next;
        let (account, name) = {
            let entry = self
                .manifest
                .files
                .get(index)
                .ok_or_else(|| refused("more files were written than the manifest describes"))?;
            (entry.account.clone(), entry.name.clone())
        };
        self.counts.push(FileCounts {
            account,
            name,
            ..Default::default()
        });
        self.open = Some(index);
        Ok(&self.manifest.files[index])
    }

    /// Writes one record whose bytes are in hand.
    ///
    /// For an ordinary record, which is a row and not a photograph. A directory
    /// file's records go through [`record_from`](Writer::record_from), which
    /// never holds one whole.
    pub fn record(&mut self, kind: RecordKind, key: &str, bytes: &[u8]) -> DbResult<()> {
        self.open_record(kind, key, bytes.len() as u64)?;
        self.put(bytes)?;
        self.note(kind, bytes.len() as u64);
        Ok(())
    }

    /// Writes one record by streaming `len` bytes out of `body`.
    ///
    /// The length is announced rather than discovered, for the reason the raw
    /// transfer path announces one: the bytes may contain anything, so there is
    /// no terminator to look for. A `body` that runs out early is an error and
    /// the archive is abandoned - it has already had a length written that the
    /// content does not match, and no amount of padding makes that a backup.
    pub fn record_from(&mut self, kind: RecordKind, key: &str, len: u64, body: &mut dyn Read) -> DbResult<()> {
        self.open_record(kind, key, len)?;
        let mut buffer = vec![0u8; len.clamp(1, CHUNK as u64) as usize];
        let mut left = len;
        while left > 0 {
            let want = buffer.len().min(left as usize);
            let got = body.read(&mut buffer[..want])?;
            if got == 0 {
                return Err(refused(format!(
                    "record '{}' announced {} bytes and ran out {} short",
                    key, len, left
                )));
            }
            self.put(&buffer[..got])?;
            left -= got as u64;
        }
        self.note(kind, len);
        Ok(())
    }

    fn open_record(&mut self, kind: RecordKind, key: &str, len: u64) -> DbResult<()> {
        if self.open.is_none() {
            return Err(refused("a record was written outside any file"));
        }
        self.put(&[kind.tag()])?;
        self.put_blob(key.as_bytes())?;
        self.put(&len.to_le_bytes())?;
        Ok(())
    }

    fn note(&mut self, kind: RecordKind, len: u64) {
        let Some(counts) = self.counts.last_mut() else {
            return;
        };
        match kind {
            RecordKind::Data => counts.records += 1,
            RecordKind::Dictionary => counts.dictionary += 1,
        }
        counts.bytes += len;
    }

    /// Closes the current file's records.
    pub fn end_file(&mut self) -> DbResult<()> {
        if self.open.take().is_none() {
            return Err(refused("a file's records were ended without being begun"));
        }
        self.put(&[TAG_END_OF_FILE])?;
        self.next += 1;
        Ok(())
    }

    /// Closes the archive: the trailer and the checksum, and the counts for the
    /// caller to report.
    pub fn finish(mut self) -> DbResult<Trailer> {
        if self.open.is_some() {
            return Err(refused("the archive was finished with a file's records still open"));
        }
        if self.next != self.manifest.files.len() {
            return Err(refused(format!(
                "the manifest describes {} files and {} were written",
                self.manifest.files.len(),
                self.next
            )));
        }
        let trailer = Trailer {
            files: std::mem::take(&mut self.counts),
        };
        self.put(&[TAG_END_OF_ARCHIVE])?;
        let document = serde_json::to_vec(&trailer)
            .map_err(|e| DbError::InvalidRequest(format!("The trailer cannot be written: {}", e)))?;
        self.put_blob(&document)?;
        // Outside the checksum, because it is the checksum.
        let checksum = self.crc.finish();
        self.sink.write_all(&checksum.to_le_bytes())?;
        self.sink.flush()?;
        Ok(trailer)
    }

    /// Bytes written so far, checksum excluded.
    pub fn written(&self) -> u64 {
        self.written
    }

    fn put_blob(&mut self, bytes: &[u8]) -> DbResult<()> {
        self.put(&(bytes.len() as u64).to_le_bytes())?;
        self.put(bytes)
    }

    fn put(&mut self, bytes: &[u8]) -> DbResult<()> {
        self.sink.write_all(bytes)?;
        self.crc.update(bytes);
        self.written += bytes.len() as u64;
        Ok(())
    }
}

// ---------------------------------------------------------------- reading ---

/// Where an archive's records go as it is read.
///
/// A trait rather than a return value because an archive does not fit in
/// memory: a whole-database export of a deployment with directory files is
/// however large that deployment is. One parser serves every caller - the
/// verify pass discards, the import applies - so an archive that verifies and
/// an archive that restores can never be parsed by two pieces of code that
/// disagree.
pub trait Sink {
    /// A file is about to deliver its records. `index` is its position in the
    /// manifest.
    fn begin_file(&mut self, index: usize, entry: &FileEntry) -> DbResult<()> {
        let _ = (index, entry);
        Ok(())
    }

    /// One record. `body` yields exactly `len` bytes.
    ///
    /// An implementation may read fewer; the parser skips whatever is left, so
    /// a sink that does not want a record does not have to consume it. What it
    /// must not do is read past the end, and the bounded reader it is handed
    /// makes that impossible rather than merely forbidden.
    fn record(&mut self, entry: &FileEntry, kind: RecordKind, key: &str, len: u64, body: &mut dyn Read)
    -> DbResult<()>;

    /// The file's records are done.
    fn end_file(&mut self, entry: &FileEntry) -> DbResult<()> {
        let _ = entry;
        Ok(())
    }
}

/// A sink that reads an archive and keeps none of it: the verify pass, and the
/// first of an import's two.
#[derive(Debug, Default)]
pub struct Discard;

impl Sink for Discard {
    fn record(
        &mut self,
        _entry: &FileEntry,
        _kind: RecordKind,
        _key: &str,
        _len: u64,
        _body: &mut dyn Read,
    ) -> DbResult<()> {
        Ok(())
    }
}

/// Reads `source` through, handing every record to `sink`, and returns what the
/// archive says about itself.
///
/// The checksum is verified before this returns, and **not** before the sink
/// has seen the records - there is no way to check the last four bytes of a
/// stream without reading the ones in front of them. A caller that must not act
/// on an archive until it is known good reads it twice: once into [`Discard`],
/// then again into the sink that applies it. [`verify`] is the first of those.
pub fn read<R: Read>(source: R, sink: &mut dyn Sink) -> DbResult<Summary> {
    let mut reader = Reader {
        source,
        crc: Crc32c::new(),
        read: 0,
    };

    let mut magic = [0u8; MAGIC.len()];
    reader.take_exact(&mut magic)?;
    if magic != MAGIC {
        return Err(refused(
            "it does not start with an archive header. Check the path names an archive and not something else",
        ));
    }

    let manifest: Manifest = reader.document("manifest")?;
    if manifest.archive_format > CURRENT || manifest.archive_format < OLDEST_SUPPORTED {
        return Err(refused(format!(
            "it is archive format {}, and this build reads {} to {}",
            manifest.archive_format, OLDEST_SUPPORTED, CURRENT
        )));
    }

    for (index, entry) in manifest.files.iter().enumerate() {
        sink.begin_file(index, entry)?;
        loop {
            let tag = reader.byte()?;
            let kind = match tag {
                TAG_RECORD => RecordKind::Data,
                TAG_DICT => RecordKind::Dictionary,
                TAG_END_OF_FILE => break,
                other => {
                    return Err(refused(format!(
                        "an unknown record tag {:#04x} inside file '{}'",
                        other, entry.name
                    )));
                }
            };
            let key = reader.key()?;
            let len = reader.length("a record")?;
            reader.bounded(len, |body| sink.record(entry, kind, &key, len, body))?;
        }
        sink.end_file(entry)?;
    }

    let tag = reader.byte()?;
    if tag != TAG_END_OF_ARCHIVE {
        return Err(refused(format!(
            "the file list ended and the archive did not - found tag {:#04x}",
            tag
        )));
    }
    let trailer: Trailer = reader.document("trailer")?;
    if trailer.files.len() != manifest.files.len() {
        return Err(refused(format!(
            "the manifest describes {} files and the trailer counts {}",
            manifest.files.len(),
            trailer.files.len()
        )));
    }

    // Everything before the checksum is now folded in, so the comparison is
    // against exactly what was read.
    let computed = reader.crc.finish();
    let mut stored = [0u8; 4];
    reader
        .source
        .read_exact(&mut stored)
        .map_err(|e| short("the checksum", e))?;
    let stored = u32::from_le_bytes(stored);
    if computed != stored {
        return Err(refused(format!(
            "its checksum does not match its contents ({:08x} stored, {:08x} computed). It was truncated, altered, or \
             written by a failed export",
            stored, computed
        )));
    }

    Ok(Summary {
        manifest,
        trailer,
        archive_bytes: reader.read + 4,
    })
}

/// Reads an archive through without keeping any of it, to prove it decodes and
/// its checksum holds.
///
/// What `--verify` runs, and what an import runs first.
pub fn verify<R: Read>(source: R) -> DbResult<Summary> {
    read(source, &mut Discard)
}

/// An archive that stops early, said as the thing it is rather than as
/// "unexpected end of file".
fn short(what: &str, e: io::Error) -> DbError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        refused(format!("it ends part way through {}. It is truncated", what))
    } else {
        DbError::Io(e)
    }
}

struct Reader<R: Read> {
    source: R,
    crc: Crc32c,
    read: u64,
}

impl<R: Read> Reader<R> {
    fn take_exact(&mut self, into: &mut [u8]) -> DbResult<()> {
        self.source.read_exact(into).map_err(|e| short("its header", e))?;
        self.crc.update(into);
        self.read += into.len() as u64;
        Ok(())
    }

    fn byte(&mut self) -> DbResult<u8> {
        let mut one = [0u8; 1];
        self.source.read_exact(&mut one).map_err(|e| short("a record tag", e))?;
        self.crc.update(&one);
        self.read += 1;
        Ok(one[0])
    }

    fn length(&mut self, what: &str) -> DbResult<u64> {
        let mut bytes = [0u8; 8];
        self.source
            .read_exact(&mut bytes)
            .map_err(|e| short(&format!("the length of {}", what), e))?;
        self.crc.update(&bytes);
        self.read += 8;
        Ok(u64::from_le_bytes(bytes))
    }

    fn key(&mut self) -> DbResult<String> {
        let len = self.length("a key")?;
        if len > MAX_KEY_BYTES {
            return Err(refused(format!(
                "a key of {} bytes, past the {} a key may be",
                len, MAX_KEY_BYTES
            )));
        }
        let mut bytes = vec![0u8; len as usize];
        self.source.read_exact(&mut bytes).map_err(|e| short("a key", e))?;
        self.crc.update(&bytes);
        self.read += len;
        String::from_utf8(bytes).map_err(|_| refused("a key that is not text"))
    }

    /// A length-prefixed JSON document, bounded before it is allocated.
    fn document<T: for<'de> Deserialize<'de>>(&mut self, what: &str) -> DbResult<T> {
        let len = self.length(what)?;
        if len > MAX_DOCUMENT_BYTES {
            return Err(refused(format!(
                "its {} claims to be {} bytes, past the {} one may be. It is damaged",
                what, len, MAX_DOCUMENT_BYTES
            )));
        }
        let mut bytes = vec![0u8; len as usize];
        self.source.read_exact(&mut bytes).map_err(|e| short(what, e))?;
        self.crc.update(&bytes);
        self.read += len;
        serde_json::from_slice(&bytes).map_err(|e| refused(format!("its {} does not decode: {}", what, e)))
    }

    /// Runs `f` over exactly the next `len` bytes, then consumes whatever `f`
    /// left, so the stream is positioned at the next record either way.
    fn bounded<T>(&mut self, len: u64, f: impl FnOnce(&mut dyn Read) -> DbResult<T>) -> DbResult<T> {
        let mut bounded = Bounded {
            source: &mut self.source,
            crc: &mut self.crc,
            left: len,
        };
        let outcome = f(&mut bounded);
        // Drained even when `f` failed. The bytes of this record are not the
        // sink's to leave behind: whatever it did or did not take, the stream
        // has to be positioned at the next tag, or every tag after this one is
        // read out of the middle of a record.
        let drained = bounded.drain();
        let left = bounded.left;
        self.read += len - left;
        // The sink's own failure first, because it says what actually went
        // wrong with the work; a drain that also failed is a truncated archive
        // and says so when the sink had nothing to report.
        let value = outcome?;
        drained.map_err(|e| short("a record", e))?;
        Ok(value)
    }
}

/// A reader over exactly `left` more bytes of an archive, folding what it hands
/// out into the archive's checksum as it goes.
struct Bounded<'a, R: Read> {
    source: &'a mut R,
    crc: &'a mut Crc32c,
    left: u64,
}

impl<R: Read> Bounded<'_, R> {
    /// Reads and discards whatever the sink did not take.
    fn drain(&mut self) -> io::Result<()> {
        let mut buffer = vec![0u8; CHUNK];
        while self.left > 0 {
            let want = buffer.len().min(self.left as usize);
            let got = self.read(&mut buffer[..want])?;
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a record ended early"));
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for Bounded<'_, R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = into.len().min(self.left as usize);
        let got = self.source.read(&mut into[..want])?;
        self.crc.update(&into[..got]);
        self.left -= got as u64;
        Ok(got)
    }
}
