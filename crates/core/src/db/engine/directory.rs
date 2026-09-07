//! The directory-file commands: where a record's bytes are, and what the engine
//! will and will not do with them.
//!
//! The storage side - key validation, the atomic write, the sweep - is
//! [`crate::db::directory`]. This is how a command reaches the right directory,
//! and it is deliberately the shortest module of its kind, because the whole
//! point of a directory file is that almost nothing happens between the request
//! and the filesystem.
//!
//! # No table, and therefore no table lock
//!
//! Nothing here loads a [`Table`](crate::db::Table), takes a table guard or
//! touches the cache. That is the property the issue this implements asks for:
//! reading a forty megabyte record must not block every writer to that file for
//! the length of the read, and it cannot, because there is no lock to hold. The
//! only shared state consulted is the account's `DIR` entry, read through
//! [`Database::file_attributes_for_account`] from a cache that is warm after
//! the first request.
//!
//! It also means a directory file is never dirty, never flushed and never
//! evicted. A write is on the disk when it returns - `rename` is the commit -
//! so there is nothing for the flush machinery to be told about.
//!
//! # Why the refusals are here rather than at each caller
//!
//! A directory file has no fields, no dictionary conversions and no index, so a
//! `WITH` clause against one can only ever match nothing. Every command that
//! cannot mean anything against a directory file is refused in one place, with
//! a message that says what to use instead, because a command that quietly
//! answers "no records" is the failure this database keeps saying it will not
//! have.

use super::Database;
use crate::db::directory::{self, DirectoryStats};
use crate::db::error::{DbError, DbResult};
use crate::db::models::*;
use std::path::{Path, PathBuf};

/// One record of a directory file, as an enumeration reports it: the key and
/// how large it is, without the bytes.
///
/// `LIST`, `SELECT` and `QUERY` answer with these. Handing back the content of
/// every record instead would make listing a file of scans cost the whole file,
/// which is the burden a directory file exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryRecord {
    pub key: String,
    pub bytes: u64,
}

impl Database {
    /// True when the account's `DIR` marks this file a directory file.
    pub fn is_table_directory_for_account(&self, account: &str, name: &str) -> bool {
        self.file_attributes_for_account(account, name).is_directory()
    }

    /// The host directory holding this file's records.
    ///
    /// `Err` for a file that is not a directory file, so a caller that reached
    /// here by mistake is told rather than handed a path that would create one.
    pub fn directory_root(&self, account: &str, name: &str) -> DbResult<PathBuf> {
        let attributes = self.file_attributes_for_account(account, name);
        let Some(policy) = attributes.directory.as_ref() else {
            return Err(self.not_a_directory_file(account, name));
        };
        Ok(self.directory_root_from(account, name, policy))
    }

    /// Where a policy points, resolved against the file's own directory when it
    /// points nowhere in particular.
    pub(crate) fn directory_root_from(&self, account: &str, name: &str, policy: &DirectoryPolicy) -> PathBuf {
        match policy.explicit_path() {
            Some(path) => PathBuf::from(path),
            None => Path::new(&self.file_dir(account, name)).join(directory::DEFAULT_RECORDS_DIR),
        }
    }

    /// Largest record a directory file will read or write. Configurable so an
    /// installation storing video and one storing signatures can each say what
    /// a mistake looks like.
    pub fn max_directory_record_bytes(&self) -> u64 {
        self.max_directory_record_bytes
    }

    /// One record's bytes, or `None` when the file holds no such record.
    pub fn read_directory_record(&self, account: &str, name: &str, key: &str) -> DbResult<Option<Vec<u8>>> {
        let root = self.directory_root(account, name)?;
        directory::read(&root, key, self.max_directory_record_bytes)
    }

    /// Replaces one record with exactly these bytes.
    ///
    /// No `note_write_for` follows, and that is not an omission: the rename
    /// this ends with *is* the commit, so there is nothing buffered for a
    /// flush to catch up on and nothing for a ticker to write out later.
    pub fn write_directory_record(&self, account: &str, name: &str, key: &str, bytes: &[u8]) -> DbResult<()> {
        let root = self.directory_root(account, name)?;
        directory::ensure_root(&root)?;
        directory::write(
            &root,
            key,
            bytes,
            self.max_directory_record_bytes,
            self.directory_fsync(account, name),
        )
    }

    /// Copies a host file in as one record, streaming it rather than holding it
    /// in memory. Answers with how many bytes were copied.
    pub fn store_directory_record(&self, account: &str, name: &str, key: &str, source: &Path) -> DbResult<u64> {
        let root = self.directory_root(account, name)?;
        directory::ensure_root(&root)?;
        directory::store(
            &root,
            key,
            source,
            self.max_directory_record_bytes,
            self.directory_fsync(account, name),
        )
    }

    /// Copies one record out to a host file, streaming it. `None` when the file
    /// holds no such record, so nothing was written to the destination.
    pub fn extract_directory_record(
        &self,
        account: &str,
        name: &str,
        key: &str,
        destination: &Path,
    ) -> DbResult<Option<u64>> {
        let root = self.directory_root(account, name)?;
        directory::extract(&root, key, destination)
    }

    /// Removes one record. `false` when there was none to remove.
    pub fn delete_directory_record(&self, account: &str, name: &str, key: &str) -> DbResult<bool> {
        let root = self.directory_root(account, name)?;
        directory::remove(&root, key)
    }

    /// How large one record is, without reading it. `None` when the file holds
    /// no such record.
    pub fn read_directory_size(&self, account: &str, name: &str, key: &str) -> DbResult<Option<u64>> {
        let root = self.directory_root(account, name)?;
        directory::size(&root, key)
    }

    /// Every record the file holds, in key order, with its size and not its
    /// bytes.
    pub fn directory_records(&self, account: &str, name: &str) -> DbResult<Vec<DirectoryRecord>> {
        let root = self.directory_root(account, name)?;
        let mut records = Vec::new();
        for key in directory::keys(&root)? {
            let bytes = directory::size(&root, &key)?.unwrap_or(0);
            records.push(DirectoryRecord { key, bytes });
        }
        Ok(records)
    }

    /// What the file holds, from the directory entries alone.
    pub fn directory_statistics(&self, account: &str, name: &str) -> DbResult<DirectoryStats> {
        let root = self.directory_root(account, name)?;
        directory::sweep_tmp(&root);
        directory::stats(&root)
    }

    /// How hard a write to this file is pushed to the platter.
    ///
    /// A directory file carries no durability flag of its own - its `DIR` entry
    /// has no room for one, because attribute 1 already said the records are
    /// somewhere else - so it follows the database's own policy, and the
    /// stricter one when the whole database was asked to be durable.
    fn directory_fsync(&self, _account: &str, _name: &str) -> crate::db::hashfile::FsyncPolicy {
        if self.durable_writes {
            self.durable_fsync
        } else {
            self.fsync
        }
    }

    /// The refusal a command that cannot mean anything against a directory file
    /// answers with.
    ///
    /// `instead` says what to use, because "not supported" leaves a caller
    /// guessing at whether it asked the wrong question or reached the wrong
    /// file.
    pub(crate) fn directory_file_refusal(&self, name: &str, what: &str, instead: &str) -> DbError {
        DbError::InvalidRequest(format!("'{}' is a directory file: {} - {}", name, what, instead))
    }

    fn not_a_directory_file(&self, account: &str, name: &str) -> DbError {
        DbError::InvalidRequest(format!(
            "'{}' in account '{}' is not a directory file; create one with CREATE.FILE {} DIRECTORY",
            name, account, name
        ))
    }
}
