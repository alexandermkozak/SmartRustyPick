//! The small checksummed text files the engine keeps beside its records.
//!
//! Three things use one: a queue's `queue` book, an autokey file's `autokey`
//! counter, and the storage directory's own `.format` stamp. None of them holds
//! a record, all of them are rewritten as a unit, and all of them have to
//! survive a crash in the middle of being written - so all three are one
//! format, written here rather than three times.
//!
//! # The shape
//!
//! ```text
//! checksum=1a2b3c4d
//! next=1764950412345000001
//! ```
//!
//! The checksum goes **first**, covering everything after it. A file cut short
//! at a line boundary would otherwise lose the checksum along with the lines it
//! covers and read back as a perfectly plausible older file. The write goes
//! through a temporary and a rename, so a crash leaves the previous version
//! rather than half of this one.
//!
//! # Absent is not the same as unreadable
//!
//! [`read`] distinguishes them, and callers must. For a queue's book, starting
//! over costs a few redeliveries either way, so both are handled the same. For
//! the format stamp they could not be more different: a missing stamp means a
//! directory written before stamping existed, and an unreadable one means a
//! stamp whose version *cannot be known* - and guessing there is how a
//! directory gets opened by a build that should have refused it.

use crate::db::hashfile::{self, FsyncPolicy};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// What was found where a state file was expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No such file.
    Missing,
    /// The file is there and does not check out: truncated, corrupt, or written
    /// by something else. What it *said* is not recoverable.
    Unreadable,
    /// The body, checksum verified.
    Body(String),
}

impl State {
    /// The body, for a caller that treats an unreadable file as an absent one.
    /// Every caller that cannot afford to do that matches on [`State`] instead.
    pub fn body(self) -> Option<String> {
        match self {
            State::Body(body) => Some(body),
            _ => None,
        }
    }
}

/// Reads a checksummed state file.
pub fn read(path: &Path) -> State {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return State::Missing,
        // A file that is there and cannot be read is not a file that is not
        // there: a permission problem on the format stamp must not read as "no
        // stamp" and adopt the directory.
        Err(_) => return State::Unreadable,
    };
    let Some((checksum_line, body)) = content.split_once('\n') else {
        return State::Unreadable;
    };
    let Some(recorded) = checksum_line.strip_prefix("checksum=") else {
        return State::Unreadable;
    };
    match u32::from_str_radix(recorded.trim(), 16) {
        Ok(recorded) if recorded == hashfile::crc32c(body.as_bytes()) => State::Body(body.to_string()),
        _ => State::Unreadable,
    }
}

/// Writes a checksummed state file, checksum first and through a temporary, so
/// a crash mid-write leaves the previous state rather than half of this one.
pub fn write(path: &Path, body: &str, fsync: FsyncPolicy) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state file has no directory"))?;
    fs::create_dir_all(dir)?;
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "state file has no name"))?
        .to_os_string();
    name.push(".tmp");
    let tmp = dir.join(name);
    {
        let mut file = File::create(&tmp)?;
        writeln!(file, "checksum={:08x}", hashfile::crc32c(body.as_bytes()))?;
        file.write_all(body.as_bytes())?;
        if fsync == FsyncPolicy::Always {
            file.sync_all()?;
        }
    }
    fs::rename(tmp, path)
}

/// Removes a state file, treating one that is not there as removed.
pub fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The value of one `key=value` line, or `None` when the body has no such line.
pub fn field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn a_body_survives_the_round_trip() {
        let guard = TempDir::new("statefile_round_trip");
        let path = Path::new(guard.path()).join("book");
        write(&path, "next=7\nkind=queue\n", FsyncPolicy::Never).unwrap();
        let State::Body(body) = read(&path) else {
            panic!("a file just written must read back");
        };
        assert_eq!(field(&body, "next"), Some("7"));
        assert_eq!(field(&body, "kind"), Some("queue"));
        assert_eq!(field(&body, "missing"), None);
    }

    /// The distinction the format stamp depends on. A file that is damaged must
    /// never read as a file that was never written: the first is a version that
    /// cannot be known, the second is a directory from before stamping.
    #[test]
    fn a_damaged_file_is_unreadable_rather_than_missing() {
        let guard = TempDir::new("statefile_damaged");
        let dir = Path::new(guard.path());
        assert_eq!(read(&dir.join("never-written")), State::Missing);

        for (name, content) in [
            ("no-checksum", "next=7\n"),
            ("truncated", "checksum=deadbeef"),
            ("wrong-checksum", "checksum=00000000\nnext=7\n"),
            ("empty", ""),
        ] {
            let path = dir.join(name);
            fs::write(&path, content).unwrap();
            assert_eq!(read(&path), State::Unreadable, "{} must not read as missing", name);
        }
    }

    #[test]
    fn a_rewrite_leaves_no_temporary_behind() {
        let guard = TempDir::new("statefile_rewrite");
        let path = Path::new(guard.path()).join("book");
        write(&path, "next=1\n", FsyncPolicy::Never).unwrap();
        write(&path, "next=2\n", FsyncPolicy::Never).unwrap();
        assert_eq!(read(&path).body().as_deref(), Some("next=2\n"));
        let leftovers: Vec<String> = fs::read_dir(guard.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{:?}", leftovers);
    }
}
