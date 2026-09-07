//! Applying a set of writes and deletes so that all of it is visible or none
//! of it is.
//!
//! The format - what a [`Change`] is and how an intent is written - is
//! [`crate::db::transaction`]. This is what the engine does with one: which
//! locks it takes, in what order, and what happens to a set the process died in
//! the middle of.
//!
//! # The scope, and what is refused
//!
//! A transaction spans **any number of files within one account**. It does not
//! span accounts: a change names a file, never an account, so the wire has no
//! way to ask for it and there is no half-supported case to get wrong. Two
//! things inside the scope are refused rather than quietly applied
//! non-atomically, both with [`DbError::TransactionScope`]:
//!
//! - a **queue file**, whose records are minted by `ENQUEUE` and handed out by
//!   `DEQUEUE` against a claim book this knows nothing about. Writing one of
//!   its records directly would leave the order describing a record that is not
//!   there, or a record no consumer can ever be given;
//! - a set of more than [`MAX_CHANGES`] changes, which is a bulk load wearing a
//!   transaction's clothes.
//!
//! # The locks
//!
//! Every file the set touches is locked, **all at once**, in file-name order,
//! and none of them is let go until the whole set is applied and written. That
//! is the one place in the engine where a thread holds more than one file lock,
//! and the ordering is what keeps two transactions from deadlocking on each
//! other. Nothing else in the engine holds two: a flush takes each dirty file
//! in turn and releases it, so it can never be the second party to a cycle.
//!
//! Holding the locks across the flush is deliberate. Were they released first,
//! a ticker flush could write one of the files with the *file's* sync policy in
//! the gap, and the commit would then retire the intent over bytes that are
//! only in the page cache. Under the guards, the transaction's own
//! [`FsyncPolicy::Always`] is the only policy that can write them.
//!
//! # Where a crash lands
//!
//! - **Before the intent is on disk**: nothing was applied and nothing was
//!   promised. The set never happened.
//! - **After the intent, before or between the file writes**: the next open
//!   finds the intent and applies the whole set again. Every change is
//!   idempotent, so the files already written are unaffected and the ones that
//!   were not catch up.
//! - **After every file is written, before the intent is removed**: the same
//!   replay runs and changes nothing.
//!
//! A transaction is therefore committed the moment its intent is durable, which
//! is why the intent is fsynced and why the files are too before it is retired.
//! An I/O failure after that point is reported to the caller, but the set is
//! still committed and the next open completes it - saying otherwise would be
//! the one answer that is not true.

use super::{Database, TableHandle, WriteMark, assert_no_table_guard_held, mlock};
use crate::db::error::{DbError, DbResult};
use crate::db::hashfile::FsyncPolicy;
use crate::db::transaction::{self, Change, ChangeOp, Intent, MAX_CHANGES};
use std::collections::{HashMap, HashSet};

/// What to do with a change naming a file that cannot take it.
#[derive(Clone, Copy)]
enum Unusable {
    /// Refuse the whole set. A caller naming a file the account does not have
    /// has a bug, and applying the rest of its set would be exactly the partial
    /// application this exists to rule out.
    Refuse,
    /// Leave it out. Only a replay uses this: the file was there when the
    /// intent was written and has since been dropped, or made a queue, and
    /// there is nothing left to apply the change to. Refusing would mean a
    /// database that cannot be opened because of a file somebody deleted on
    /// purpose.
    Skip,
}

impl Database {
    /// Applies `changes` to one account so that either all of them are visible
    /// or none of them are, and does not return until the whole set is on disk.
    ///
    /// The set is refused, with nothing applied, when it names a file the
    /// account does not have, names a queue file, carries more than
    /// [`MAX_CHANGES`] changes, or changes one key twice - see the module
    /// documentation for why each of those is a refusal rather than a
    /// best-effort.
    pub fn apply_transaction(&self, account: &str, changes: Vec<Change>) -> DbResult<usize> {
        assert_no_table_guard_held("A transaction");
        if changes.is_empty() {
            return Err(DbError::InvalidRequest(
                "A transaction must carry at least one change".to_string(),
            ));
        }
        if changes.len() > MAX_CHANGES {
            return Err(DbError::TransactionScope(format!(
                "A transaction may carry at most {} changes, and this one carries {}",
                MAX_CHANGES,
                changes.len()
            )));
        }
        let mut targets = HashSet::new();
        for change in &changes {
            if !targets.insert(change.target()) {
                return Err(DbError::InvalidRequest(format!(
                    "'{}' of file '{}' is changed twice in one transaction, and nothing says which change wins",
                    change.key, change.file
                )));
            }
        }

        // Everything that can refuse the set happens before the intent is
        // written: an intent on disk is a promise that the set will be applied,
        // and a promise made before the files are known to be there is one the
        // replay would have to break.
        let files = self.stage(account, &changes, Unusable::Refuse)?;
        crash_point("before-intent");
        let intent = transaction::write_intent(&self.storage_dir, account, &changes)?;
        crash_point("after-intent");
        self.apply_staged(account, &files, &changes)?;
        transaction::remove_intent(&self.storage_dir, &intent)?;
        self.settle(account, &files);
        Ok(changes.len())
    }

    /// Finishes the transactions a previous run of the database did not, in the
    /// order they were committed in.
    ///
    /// Runs once, at open, beside the sweep that removes the `.tmp` files a
    /// crash leaves in a section directory - and for the same reason: this is
    /// the one moment when nothing else is reading the files, so a half-applied
    /// set can be completed without anybody having seen it.
    ///
    /// An intent that does not decode was never renamed into place, so it names
    /// a transaction that was never committed and is discarded. One that fails
    /// to apply is *not* swallowed: refusing to open is the honest answer,
    /// because the alternative is a database that quietly starts without a
    /// change it acknowledged.
    pub(super) fn replay_transaction_log(&self) -> DbResult<()> {
        for path in transaction::pending(&self.storage_dir) {
            if let Some(intent) = transaction::read_intent(&path) {
                self.replay(&intent)?;
            }
            transaction::remove_intent(&self.storage_dir, &path)?;
        }
        Ok(())
    }

    fn replay(&self, intent: &Intent) -> DbResult<()> {
        // The account was dropped after the intent was written; its files went
        // with it and there is nothing to apply the set to.
        if self.get_account_dir(&intent.account).is_none() {
            return Ok(());
        }
        let files = self.stage(&intent.account, &intent.changes, Unusable::Skip)?;
        if files.is_empty() {
            return Ok(());
        }
        self.apply_staged(&intent.account, &files, &intent.changes)?;
        self.settle(&intent.account, &files);
        Ok(())
    }

    /// Resolves every file the set names, once each, in the order their locks
    /// will be taken.
    ///
    /// Loading happens here, with no file lock held, so the apply below is a
    /// pure memory operation under the guards. Holding the handles is also what
    /// keeps the files in the cache: eviction skips a table another thread
    /// still has a handle to.
    fn stage(&self, account: &str, changes: &[Change], unusable: Unusable) -> DbResult<Vec<(String, TableHandle)>> {
        let mut names: Vec<&str> = changes.iter().map(|change| change.file.as_str()).collect();
        names.sort_unstable();
        names.dedup();

        let mut files = Vec::with_capacity(names.len());
        for name in names {
            let attributes = self.file_attributes_for_account(account, name);
            let outside = if attributes.queue.is_some() {
                Some(format!(
                    "'{}' is a queue file: its records are minted by ENQUEUE and claimed by DEQUEUE, \
                     so a transaction cannot write them",
                    name
                ))
            } else if attributes.is_directory() {
                // A directory file commits with `rename`, which is atomic for
                // the one record and reaches nothing else. There is no way to
                // hold that write back until the rest of the set is ready, so a
                // set naming one is refused rather than applied in a way that
                // could not be undone if a later change failed.
                Some(format!(
                    "'{}' is a directory file: each of its records is committed on its own by the write that \
                     renames it, so a transaction cannot hold one back until the rest of the set is ready",
                    name
                ))
            } else {
                None
            };
            if let Some(why) = outside {
                match unusable {
                    Unusable::Refuse => return Err(DbError::TransactionScope(why)),
                    Unusable::Skip => continue,
                }
            }
            match self.get_table_mut_for_account(account, name) {
                Ok(handle) => files.push((name.to_string(), handle)),
                Err(e) => match unusable {
                    Unusable::Refuse => return Err(e),
                    Unusable::Skip => continue,
                },
            }
        }
        Ok(files)
    }

    /// The critical section: every file locked at once, the whole set applied,
    /// and every file written out before a single lock is released.
    ///
    /// A change naming a file that is not in `files` is skipped, which is how a
    /// replay leaves out a file that has since been dropped. On the ordinary
    /// path `stage` has already refused such a set, so nothing is skipped
    /// there.
    fn apply_staged(&self, account: &str, files: &[(String, TableHandle)], changes: &[Change]) -> DbResult<()> {
        // Looked up before a single guard is taken: it reads the account
        // registry, which sits above a file lock in the lock order.
        let storage = self.account_storage_dir(account);
        let at: HashMap<&str, usize> = files
            .iter()
            .enumerate()
            .map(|(position, (name, _))| (name.as_str(), position))
            .collect();
        // Taken in the order `stage` put them in, which is file-name order.
        let mut guards: Vec<_> = files.iter().map(|(_, handle)| handle.write()).collect();

        for change in changes {
            let Some(position) = at.get(change.file.as_str()) else {
                continue;
            };
            let table = &mut *guards[*position];
            match (&change.op, change.is_dict) {
                (ChangeOp::Write(record), false) => table.insert_record(&change.key, record.clone()),
                (ChangeOp::Write(record), true) => {
                    table.dictionary.insert(change.key.clone(), record.clone());
                    table.mark_dict_dirty();
                }
                (ChangeOp::Delete, false) => {
                    table.remove_record(&change.key);
                }
                (ChangeOp::Delete, true) => {
                    table.dictionary.remove(&change.key);
                    table.mark_dict_dirty();
                }
            }
        }

        // Always, whatever the files' own durability says: the intent is
        // retired on the strength of these writes, so "in the page cache" is
        // not good enough for any of them.
        for (position, (name, _)) in files.iter().enumerate() {
            let key = (account.to_string(), name.clone());
            self.flush_locked(&key, &mut guards[position], &storage, FsyncPolicy::Always)?;
            if position == 0 {
                crash_point("between-files");
            }
        }
        Ok(())
    }

    /// The bookkeeping that cannot run under the guards: the per-file flush
    /// batches these writes belong to are now empty, and `SYSTEM/$CLIENTS`
    /// changing means the authorizations have to be read back.
    fn settle(&self, account: &str, files: &[(String, TableHandle)]) {
        {
            let mut marks = mlock(&self.write_marks);
            for (name, _) in files {
                marks.insert((account.to_string(), name.clone()), WriteMark::fresh());
            }
        }
        if account == "SYSTEM" && files.iter().any(|(name, _)| name == "$CLIENTS") {
            let _ = self.load_clients_from_table();
        }
    }
}

/// Where a test may have the process die, to prove that a set half-written to
/// disk is still applied whole.
///
/// A crash is the only way to test the property, and a crash cannot be faked
/// from outside: the test re-executes the test binary with this variable set to
/// the stage it wants, and the child SIGKILLs itself there - nothing is
/// unwound, no destructor runs, and whatever is on disk afterwards is what a
/// power loss would have left. Compiled out of every build but a test one.
#[cfg(test)]
pub(crate) const CRASH_AT: &str = "SRP_TXN_CRASH";

#[cfg(test)]
fn crash_point(stage: &str) {
    if std::env::var(CRASH_AT).ok().as_deref() != Some(stage) {
        return;
    }
    let pid = std::process::id().to_string();
    let _ = std::process::Command::new("kill").args(["-9", &pid]).status();
    std::thread::sleep(std::time::Duration::from_secs(30));
    unreachable!("the process should have been killed");
}

#[cfg(not(test))]
fn crash_point(_stage: &str) {}
