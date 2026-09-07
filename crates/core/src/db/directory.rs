//! Directory files: records that are ordinary files on the host.
//!
//! A hash file holds its records inside group files it owns, framed and
//! checksummed, and reads every group of a section into memory the first time
//! the file is touched. That is the right shape for records made of fields, and
//! the wrong one for a scanned invoice: the marks `FM`, `VM` and `SVM` *are* a
//! record's structure, so a sub-value carrying one of those bytes splits on the
//! way back, and one read of one record in a file of photographs would make
//! every photograph resident.
//!
//! A directory file is the answer PICK already had. It is a pointer to a real
//! directory on the host, and its records are the files in it: the key is the
//! file name and the record is the file's bytes, exactly as they were written.
//! Nothing frames them, so nothing can split them; nothing caches them, so a
//! forty megabyte record costs a forty megabyte read and not a resident table.
//!
//! ```text
//! db_storage/<account>/<file>/records/<key>
//! ```
//!
//! is where they live for a file created without a path of its own; a file
//! created `PATH /srv/scans` points at that directory instead and leaves its
//! contents alone.
//!
//! # What a key may be
//!
//! A key becomes a file name on the host, so it is checked rather than
//! sanitised - see [`validate_key`]. A name that is quietly repaired is a write
//! that succeeds and reads back under a key the caller never asked for, and a
//! key holding `..` is that plus somebody else's directory. Rejecting is the
//! only answer that cannot surprise.
//!
//! # Crash safety
//!
//! A record is written the way a group is: to a temporary file, flushed,
//! `fsync`ed as far as the [`FsyncPolicy`] asks, then renamed over its key. A
//! reader therefore sees the old bytes or the new ones and never a half-written
//! file. Debris from a crash between the two is swept on the read path, as
//! [`crate::db::hashfile`] sweeps its own, so a write never pays for a scan of
//! the directory.
//!
//! # What is not here
//!
//! No index, no dictionary conversion, no query. A directory file has no
//! fields to index or to test a criterion against, and offering the machinery
//! anyway would mean a `WITH` clause that always matches nothing. Enumerating
//! keys and their sizes is what it can honestly answer, and that is what
//! `LIST`, `SELECT` and `QUERY` get from it.

use crate::db::error::{DbError, DbResult};
use crate::db::hashfile::FsyncPolicy;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Longest a key may be, in bytes. `NAME_MAX` is 255 on every filesystem this
/// runs on, and a key that only fails once it reaches the disk would fail
/// differently on each of them.
pub const MAX_KEY_BYTES: usize = 255;

/// Prefix of the temporary file a write goes to before it is renamed. A key may
/// not begin with a dot, so nothing a caller can name collides with one, and
/// enumeration skips them without having to know what they are.
pub const TMP_PREFIX: &str = ".tmp.";

/// Largest record a directory file accepts, for an installation that sets no
/// limit of its own. Large enough for the scans and the media this exists to
/// hold, small enough that a mistake is a refusal rather than an allocation the
/// machine cannot meet.
pub const DEFAULT_MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;

/// Where a directory file's records live, relative to the file's own directory,
/// when it was not created pointing somewhere else.
pub const DEFAULT_RECORDS_DIR: &str = "records";

/// Distinguishes the temporary files of two writes happening at once. Only has
/// to be unique within this process: a second process writing the same key
/// through its own database would be racing on the rename regardless, and the
/// rename is what decides.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// What a directory file holds, without reading any of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryStats {
    /// Files in the directory, not counting the debris of an interrupted write.
    pub record_count: u64,
    /// Their sizes, added up.
    pub bytes: u64,
    /// The largest of them, which is the read a client should be ready for.
    pub largest_bytes: u64,
}

/// Checks a key against what a host file name may be.
///
/// Every rule here is about a name reaching a filesystem, which is why they are
/// checked and not repaired:
///
/// - **Empty, or longer than [`MAX_KEY_BYTES`].** `NAME_MAX` would refuse the
///   long one anyway, and refusing it here means it is refused the same way
///   everywhere.
/// - **A path separator, `/` or `\`.** A key names one record in one directory.
///   A key that names a path names a record somewhere else.
/// - **`.` or `..`, or any name beginning with a dot.** The first two are the
///   directory itself and its parent. The rest is what keeps a caller's key
///   from colliding with the [`TMP_PREFIX`] a write uses.
/// - **A control byte, `NUL` included.** A name the shell, the log and the
///   filesystem each disagree about is a name nobody can act on.
///
/// Keys are compared as the host filesystem compares them, so on a
/// case-insensitive one `Invoice` and `INVOICE` are the same record. That is
/// the host's answer rather than this database's, and it is why a directory
/// file is documented as a pointer to a real directory.
pub fn validate_key(key: &str) -> DbResult<()> {
    let refuse = |why: &str| {
        Err(DbError::InvalidRequest(format!(
            "'{}' is not a usable key for a directory file: {}",
            key, why
        )))
    };
    if key.is_empty() {
        return refuse("it is empty, and a record is a file that has to be called something");
    }
    if key.len() > MAX_KEY_BYTES {
        return refuse(&format!(
            "it is {} bytes, and a file name may be at most {}",
            key.len(),
            MAX_KEY_BYTES
        ));
    }
    if key.contains('/') || key.contains('\\') {
        return refuse("it holds a path separator, and a key names one record rather than a path");
    }
    if key.starts_with('.') {
        return refuse("it begins with a dot, which names the directory itself, its parent, or an unfinished write");
    }
    if let Some(bad) = key.chars().find(|c| c.is_control()) {
        return refuse(&format!("it holds the control character {:?}", bad));
    }
    Ok(())
}

/// The file one key names, once the key has been checked.
pub fn record_path(root: &Path, key: &str) -> DbResult<PathBuf> {
    validate_key(key)?;
    Ok(root.join(key))
}

/// Creates the directory a file's records live in, and everything above it.
pub fn ensure_root(root: &Path) -> DbResult<()> {
    fs::create_dir_all(root).map_err(DbError::Io)
}

/// One record's bytes, or `None` when the directory holds no such file.
///
/// The size is checked against `max_bytes` before a byte is read, so a file
/// that grew past the limit out of band is a refusal and not an allocation the
/// machine cannot meet. `Ok(None)` is reserved for a record that is genuinely
/// not there: every other failure keeps its own error, because reporting one as
/// "no such record" is the silent data loss the rest of this format exists to
/// rule out.
pub fn read(root: &Path, key: &str, max_bytes: u64) -> DbResult<Option<Vec<u8>>> {
    let path = record_path(root, key)?;
    let length = match fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => return Err(not_a_record(key, "is a directory, not a record")),
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DbError::Io(e)),
    };
    if length > max_bytes {
        return Err(oversize(key, length, max_bytes, "read"));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    File::open(&path)?.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// How large a record is, without reading it. `None` when there is no such
/// record.
pub fn size(root: &Path, key: &str) -> DbResult<Option<u64>> {
    let path = record_path(root, key)?;
    match fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(Some(meta.len())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DbError::Io(e)),
    }
}

/// Replaces one record with exactly these bytes.
pub fn write(root: &Path, key: &str, bytes: &[u8], max_bytes: u64, fsync: FsyncPolicy) -> DbResult<()> {
    let length = bytes.len() as u64;
    if length > max_bytes {
        return Err(oversize(key, length, max_bytes, "written"));
    }
    let path = record_path(root, key)?;
    let tmp = tmp_path(root);
    let result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        if fsync == FsyncPolicy::Always {
            file.sync_all()?;
        }
        drop(file);
        fs::rename(&tmp, &path)
    })();
    finish(root, &tmp, result, fsync)
}

/// Copies a host file in as one record, without holding it in memory.
///
/// This is what makes a directory file worth having: `STORE` moves a gigabyte
/// through a fixed-size buffer, where a record travelling as a protocol request
/// would have to fit in one. The length is checked before the copy starts and
/// again from what was actually copied, because a file being appended to while
/// it is read would otherwise cross the limit unnoticed.
pub fn store(root: &Path, key: &str, source: &Path, max_bytes: u64, fsync: FsyncPolicy) -> DbResult<u64> {
    let declared = fs::metadata(source)?.len();
    if declared > max_bytes {
        return Err(oversize(key, declared, max_bytes, "stored"));
    }
    let path = record_path(root, key)?;
    let tmp = tmp_path(root);
    let mut copied = 0u64;
    let result = (|| -> io::Result<()> {
        let mut input = File::open(source)?;
        let mut file = File::create(&tmp)?;
        copied = io::copy(&mut input, &mut file)?;
        file.flush()?;
        if fsync == FsyncPolicy::Always {
            file.sync_all()?;
        }
        drop(file);
        Ok(())
    })();
    if result.is_ok() && copied > max_bytes {
        let _ = fs::remove_file(&tmp);
        return Err(oversize(key, copied, max_bytes, "stored"));
    }
    let result = result.and_then(|()| fs::rename(&tmp, &path));
    finish(root, &tmp, result, fsync)?;
    Ok(copied)
}

/// Copies one record out to a host file, without holding it in memory.
///
/// The destination is written the same way a record is - a temporary beside it,
/// then a rename - so an `EXTRACT` interrupted half way leaves the caller's own
/// file either untouched or complete, rather than truncated to whatever arrived.
pub fn extract(root: &Path, key: &str, destination: &Path) -> DbResult<Option<u64>> {
    let path = record_path(root, key)?;
    let mut input = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DbError::Io(e)),
    };
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = match parent {
        Some(dir) => tmp_path(dir),
        None => tmp_path(Path::new(".")),
    };
    let copied = (|| -> io::Result<u64> {
        let mut out = File::create(&tmp)?;
        let copied = io::copy(&mut input, &mut out)?;
        out.flush()?;
        drop(out);
        fs::rename(&tmp, destination)?;
        Ok(copied)
    })();
    match copied {
        Ok(copied) => Ok(Some(copied)),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(DbError::Io(e))
        }
    }
}

/// Removes one record. `false` when there was none to remove.
pub fn remove(root: &Path, key: &str) -> DbResult<bool> {
    let path = record_path(root, key)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(DbError::Io(e)),
    }
}

/// Every key the directory holds, in name order, sweeping the debris of an
/// interrupted write on the way past.
///
/// Anything that is not a plain file is skipped rather than reported: a
/// sub-directory somebody put there is not a record, and a name that is not
/// valid UTF-8 is not a key any client could ask for by name. Both are left
/// where they are - this owns the records it wrote, not the directory an
/// operator pointed it at.
pub fn keys(root: &Path) -> DbResult<Vec<String>> {
    let mut keys = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with(TMP_PREFIX) {
            sweep_one(&entry.path());
            continue;
        }
        if name.starts_with('.') || !entry.path().is_file() {
            continue;
        }
        keys.push(name);
    }
    keys.sort();
    Ok(keys)
}

/// The count and the byte total, from the directory entries alone.
pub fn stats(root: &Path) -> DbResult<DirectoryStats> {
    let mut stats = DirectoryStats::default();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        stats.record_count += 1;
        stats.bytes += meta.len();
        stats.largest_bytes = stats.largest_bytes.max(meta.len());
    }
    Ok(stats)
}

/// Removes what an interrupted write left behind. Called from the read path,
/// where one directory scan is already being paid for.
pub fn sweep_tmp(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            sweep_one(&entry.path());
        }
    }
}

fn sweep_one(path: &Path) {
    let _ = fs::remove_file(path);
}

/// A name in `root` that no key can collide with, because a key may not begin
/// with a dot.
fn tmp_path(root: &Path) -> PathBuf {
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    root.join(format!("{}{}.{}", TMP_PREFIX, std::process::id(), nonce))
}

/// Clears up after a write, whichever way it went, and syncs the directory when
/// the policy asks for it.
///
/// The directory sync is what makes the rename itself durable: syncing the file
/// puts its bytes on the platter, and only syncing the directory puts the name
/// that reaches them there too. It happens under `Meta` as well as `Always`,
/// exactly as a section's `meta` does, because a name that survives pointing at
/// bytes that do not is the one ordering that cannot be recovered from.
fn finish(root: &Path, tmp: &Path, result: io::Result<()>, fsync: FsyncPolicy) -> DbResult<()> {
    match result {
        Ok(()) => {
            if fsync != FsyncPolicy::Never
                && let Ok(dir) = File::open(root)
            {
                let _ = dir.sync_all();
            }
            Ok(())
        }
        Err(e) => {
            sweep_one(tmp);
            Err(DbError::Io(e))
        }
    }
}

fn oversize(key: &str, length: u64, max_bytes: u64, verb: &str) -> DbError {
    DbError::InvalidRequest(format!(
        "Record '{}' is {} bytes and cannot be {}: a directory file's records are limited to {} bytes",
        key, length, verb, max_bytes
    ))
}

fn not_a_record(key: &str, why: &str) -> DbError {
    DbError::InvalidRequest(format!("'{}' {}", key, why))
}
