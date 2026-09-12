//! The record write path: what a key has to be before a record goes under it,
//! and where the key comes from when the caller supplies none.
//!
//! `WRITE` used to be one line - put the record in the map - and that line is
//! the whole of two lost-record bugs. Two clients that both intend to *create*
//! a record write the same key and the second silently wins. Two clients that
//! each read a record, change it and write it back lose one of the two changes,
//! and nothing in the protocol reports either, because from the engine's point
//! of view two perfectly valid writes happened.
//!
//! # One critical section
//!
//! Both fixes need the same thing, and it is already there: a write holds the
//! file's own lock. So the comparison and the write, or the minting and the
//! write, happen inside one guard on that file, with no release in between. A
//! second writer arriving at any point either has not got the lock yet or is
//! looking at a file the first one has already changed - so it sees the key
//! taken, or the version moved on, and is refused. That is the entire
//! concurrency argument, and it is why these are written as one critical
//! section each rather than as a read followed by a write.
//!
//! # Why the condition token is a digest
//!
//! A version counter has to be stored somewhere, and the record section has no
//! room for one: adding it would change the on-disk format and migrate every
//! file, to hold a number the record's own bytes already determine. The bytes
//! are in hand at exactly the point the decision is made, so [`Record::version`]
//! hashes them and costs nothing on disk. What a client sees is opaque either
//! way - it reads a token and hands it back - so the choice is the engine's to
//! make and not part of the interface.
//!
//! # Why a minted key carries the clock
//!
//! An autokey file mints from the same counter a queue does - see
//! [`crate::db::sequence`] - so the key is twenty digits, sorts into arrival
//! order as text, and is safe across a restart because time has moved on in the
//! meantime. Reusing that rather than starting a counter at 1 also means there
//! is one answer in the system to "what does a minted key look like", which is
//! what lets a caller read a range back without knowing which file type minted
//! it.

use super::{Database, TableHandle};
use crate::db::error::{DbError, DbResult};
use crate::db::models::*;
use crate::db::sequence::{self, Sequence};

/// What has to be true of a key before a write or a delete is applied.
///
/// [`Condition::Always`] is the behaviour every write had before there was a
/// choice, and is still what an ordinary `WRITE` asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Condition {
    /// Apply it whatever is there. The default, and what `WRITE` means when a
    /// request names no condition.
    #[default]
    Always,
    /// Apply it only if the key holds no record. Create-if-not-exists, and what
    /// makes any generated-key or append pattern safe.
    IfAbsent,
    /// Apply it only if the record under the key still has this version, as
    /// [`Record::version`] reports it.
    IfMatch(String),
}

impl Condition {
    /// True when this is no condition at all, and the write can take the path
    /// it always took.
    pub fn is_unconditional(&self) -> bool {
        matches!(self, Condition::Always)
    }

    /// Checks the condition against what is stored under the key now, naming
    /// the file and the key so the refusal says which record it is about.
    ///
    /// The message is for a person; a client branches on the
    /// [`DbError::PreconditionFailed`] this returns.
    fn check(&self, existing: Option<&Record>, file: &str, key: &str) -> DbResult<()> {
        match (self, existing) {
            (Condition::Always, _) => Ok(()),
            (Condition::IfAbsent, None) => Ok(()),
            (Condition::IfAbsent, Some(_)) => Err(DbError::PreconditionFailed(format!(
                "Record '{}' in file '{}' already exists, and if_absent asked for it not to",
                key, file
            ))),
            (Condition::IfMatch(_), None) => Err(DbError::PreconditionFailed(format!(
                "Record '{}' in file '{}' does not exist, so it cannot match the version if_match named",
                key, file
            ))),
            (Condition::IfMatch(wanted), Some(record)) => {
                let current = record.version();
                if current == *wanted {
                    Ok(())
                } else {
                    Err(DbError::PreconditionFailed(format!(
                        "Record '{}' in file '{}' has version {}, not the {} if_match named: it was changed since \
                         it was read",
                        key, file, current, wanted
                    )))
                }
            }
        }
    }
}

/// A write that was applied, and what the caller needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The key the record is stored under - the one the caller gave, or the one
    /// this minted for it.
    pub key: String,
    /// The version the record now has, ready to be handed to a later
    /// `if_match` without reading it back.
    pub version: String,
    /// True when the key was minted rather than supplied.
    pub minted: bool,
}

impl Database {
    /// True when the account's `DIR` marks this file as minting its own keys.
    pub fn is_table_autokey_for_account(&self, account: &str, name: &str) -> bool {
        self.file_attributes_for_account(account, name).autokey
    }

    /// Writes one record, applying `condition` and minting the key when the
    /// caller supplied none.
    ///
    /// `handle` is the file, already resolved by the caller: deserializing the
    /// record needs the dictionary and the write needs the records, and
    /// resolving twice would take this file's lock again - on a file several
    /// connections are writing at once, that is the contended one.
    ///
    /// The guard is released before the flush, because [`note_write_for`] may
    /// decide to save and a save locks every dirty file in turn.
    ///
    /// [`note_write_for`]: Database::note_write_for
    // Eight arguments, and every one of them is something only the caller
    // knows: where the record goes, what it is, and what has to be true first.
    // Bundling them into a struct would move the same list one line up and cost
    // a name for it at every call site.
    #[allow(clippy::too_many_arguments)]
    pub fn write_record_in(
        &self,
        account: &str,
        name: &str,
        handle: &TableHandle,
        key: Option<&str>,
        record: Record,
        is_dict: bool,
        condition: &Condition,
    ) -> DbResult<Written> {
        if is_dict {
            // A dictionary entry's key is the field name it defines, so there
            // is nothing for a counter to mint and no order for it to be in.
            let Some(key) = key else {
                return Err(DbError::InvalidRequest(format!(
                    "A dictionary entry of '{}' is keyed by the field name it defines, so there is no key to mint",
                    name
                )));
            };
            let version = record.version();
            let mut table = handle.write();
            condition.check(table.dictionary.get(key), name, key)?;
            table.dictionary.insert(key.to_string(), record);
            table.mark_dict_dirty();
            drop(table);
            self.note_write_for(account, name)?;
            return Ok(Written {
                key: key.to_string(),
                version,
                minted: false,
            });
        }

        // Read before the guard is taken: it may need the `autokey` file from
        // disk, and a file's guard is not the place to do I/O. A counter
        // attached twice is harmless - the re-check under the guard drops the
        // loser - and reading one that turns out not to be needed costs a
        // stat of a file that is not there.
        let mints = key.is_none() && self.is_table_autokey_for_account(account, name);
        let persisted = mints.then(|| sequence::read_autokey(&self.file_dir(account, name)));

        let version = record.version();
        let (key, minted) = {
            let mut table = handle.write();
            let key = match key {
                Some(key) => {
                    // A key written by hand into an autokey file is a perfectly
                    // good record, and the counter has to step over it or it
                    // will mint that same key later and overwrite it.
                    if let Some(counter) = table.autokey.as_mut() {
                        counter.raise_past(key);
                    }
                    key.to_string()
                }
                None => {
                    if !mints {
                        return Err(Self::keyless_write_refused(name));
                    }
                    Self::attach_autokey(&mut table, persisted.unwrap_or_default());
                    let counter = table.autokey.as_mut().expect("just attached");
                    sequence::format_key(counter.mint(sequence::now_millis()))
                }
            };
            condition.check(table.records.get(&key), name, &key)?;
            table.insert_record(&key, record);
            (key, mints)
        };
        self.note_write_for(account, name)?;
        Ok(Written { key, version, minted })
    }

    /// Deletes one record, applying `condition`.
    ///
    /// Returns whether a record was removed. An unconditional delete of a key
    /// that is not there is not an error here - it is the behaviour `DELETE`
    /// has always had, and the protocol layer is where "there was nothing to
    /// delete" becomes an answer.
    pub fn delete_record_in(
        &self,
        account: &str,
        name: &str,
        handle: &TableHandle,
        key: &str,
        is_dict: bool,
        condition: &Condition,
    ) -> DbResult<bool> {
        let removed = {
            let mut table = handle.write();
            if is_dict {
                condition.check(table.dictionary.get(key), name, key)?;
                let removed = table.dictionary.remove(key).is_some();
                table.mark_dict_dirty();
                removed
            } else {
                condition.check(table.records.get(key), name, key)?;
                table.remove_record(key).is_some()
            }
        };
        self.note_write_for(account, name)?;
        Ok(removed)
    }

    /// Why a `WRITE` that named no key was refused.
    ///
    /// Said once, because the protocol and the CLI both reach it, and because
    /// the useful half of the refusal is what to do instead: a file that does
    /// not mint keys has to be told to, and that is a property of the file
    /// rather than of the request.
    fn keyless_write_refused(name: &str) -> DbError {
        DbError::InvalidRequest(format!(
            "'{}' does not mint keys, so a write to it has to name one. Create it with CREATE.FILE {} AUTOKEY, or \
             use SET.FILE to turn minting on",
            name, name
        ))
    }

    /// Attaches a table's counter if it has not got one, pulled past every key
    /// already in the file.
    ///
    /// The two sources are deliberately both consulted. The `autokey` file is
    /// authoritative when it is there, because a key it has already handed out
    /// may since have been deleted and must not come round again. The records
    /// are the backstop for when it is not - lost, or never written because the
    /// flag was turned on by hand - and between them a minted key collides with
    /// an existing record only if both are wrong at once.
    fn attach_autokey(table: &mut Table, persisted: Option<u64>) {
        if table.autokey.is_some() {
            return;
        }
        let mut counter = Sequence::restored(persisted.unwrap_or(0));
        for key in table.records.keys() {
            counter.raise_past(key);
        }
        table.autokey = Some(counter);
    }

    /// Attaches or drops a file's counter after its `DIR` entry changed.
    ///
    /// A file that stops minting keys loses the `autokey` file with it, so
    /// nothing is left on disk describing a counter the file no longer has -
    /// and a file switched back on later starts from the records, which is the
    /// only thing still true about it.
    pub(crate) fn reattach_autokey(&self, account: &str, name: &str, autokey: bool) -> DbResult<()> {
        if !autokey {
            sequence::remove_autokey(&self.file_dir(account, name))?;
        }
        let Some(handle) = self.get_table_read_only_for_account(account, name) else {
            return Ok(());
        };
        let mut table = handle.write();
        if !autokey {
            table.autokey = None;
        }
        // Turning it *on* attaches nothing here: the first keyless write does
        // that, and doing it now would read the `autokey` file under a guard
        // for a file that may never get one.
        Ok(())
    }
}

/// Writes a file's counter during a flush, and marks it clean.
///
/// Kept here rather than inline in the flush so that everything the `autokey`
/// file means sits in one place.
pub(super) fn persist(
    file_dir: &str,
    counter: &mut Sequence,
    fsync: crate::db::hashfile::FsyncPolicy,
) -> std::io::Result<()> {
    sequence::write_autokey(file_dir, counter.peek(), fsync)?;
    counter.clear_dirty();
    Ok(())
}
