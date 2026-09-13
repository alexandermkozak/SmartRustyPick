//! Taking a consistent copy of a running database, and putting one back.
//!
//! The format - what an archive is and how it decodes - is
//! [`crate::db::archive`]. This is what the engine does with one: what it locks
//! while it reads, what a restore creates, and what it refuses to do quietly.
//!
//! # The unit of consistency, said plainly
//!
//! An export **flushes, then holds every file it names** for as long as it is
//! reading them - shared guards, so other readers are unaffected and writers to
//! those files wait. The flush comes first and with no guard held, because a
//! flush locks each dirty file in turn and would otherwise wait on a lock this
//! thread is already holding.
//!
//! What that buys, and what it costs, is different at each scope, and an
//! operator should choose with both in view:
//!
//! - [`Source::File`] - one coherent file. Writers to that one file wait;
//!   nothing else in the database notices.
//! - [`Source::Account`] - every file of the account, coherent *with each
//!   other*: a record written to one file and a record written to another in
//!   the same act are either both in the archive or neither is. This is the
//!   scope worth running routinely.
//! - [`Source::All`] - the same, across every account. It blocks writes to the
//!   whole database for the length of the export, which is why it is the
//!   maintenance-window option rather than the nightly one.
//!
//! Locks are taken in `(account, file)` order, the same discipline
//! [`super::transaction`] uses and for the same reason: those are the only two
//! places in the engine that hold more than one file lock, and a shared order
//! is what keeps them from deadlocking against each other.
//!
//! # The one thing that cannot be held still
//!
//! A [directory file](crate::db::directory) has **no table**, and therefore no
//! table lock - deliberately, because reading a forty megabyte record must not
//! block every writer to that file for the length of the read. So there is
//! nothing to hold, and its records can be added and removed while the export
//! walks them.
//!
//! This is not papered over. The walk lists the file's keys and then reads each
//! one; a record deleted in between is left out rather than written as zero
//! bytes, and one created after the listing is not in the archive. The archive
//! then states, in its trailer, how many records it actually carries - which is
//! the whole reason the counts are in a trailer and not in the manifest. An
//! archive that says "eleven records" always holds eleven records.
//!
//! # Why `SYSTEM` is not in a whole-database export
//!
//! [`Source::All`] walks the account registry, which does not list `SYSTEM`.
//! That is the right default and worth being explicit about, because `SYSTEM`
//! is not a data account: it holds the registry itself, which a restore
//! rebuilds as it creates accounts; `$LOGS`, which describes the deployment
//! that wrote them and not the data; and `$CLIENTS`, the authorized client
//! certificate thumbprints. Carrying that last one into an archive means a
//! restore elsewhere silently grants the certificates of the machine it came
//! from. An operator who genuinely wants it asks for it by name with
//! [`Source::Account`], which is a deliberate act rather than a surprise inside
//! a backup.
//!
//! # Why a restore reads the archive twice
//!
//! An archive's checksum is over the whole of it, so it is only known to be
//! good at its last four bytes. Applying as it parsed would mean a truncated
//! archive had already half-restored itself by the time the truncation was
//! found, and a half-restored account looks exactly like a restored one.
//!
//! So an import is: **verify, plan, apply**. The verify pass reads the archive
//! through and discards it. The plan resolves every file against what is
//! already there and refuses the whole import if any of it would overwrite
//! something without [`ImportPlan::overwrite`] - before a byte is written,
//! because an import that fails half way is the outcome with no good answer.
//! Only then does the apply pass run, and it reads the archive a second time
//! from the start.
//!
//! That is also why an import takes a **path** rather than a reader: it has to
//! be able to start again. The streamed form spools to a temporary file first,
//! exactly as an inbound [raw transfer](crate::server::transfer) does with a
//! record body.

use super::{Database, TableHandle, assert_no_table_guard_held};
use crate::db::archive::{self, FileEntry, IndexEntry, Manifest, RecordKind, Sink, Source, Summary, Trailer};
use crate::db::error::{DbError, DbResult};
use crate::db::models::{FileAttributes, Record};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The listing file, which is the account's own record of its files. Rebuilt by
/// creating them on restore, so carrying it would restore a listing describing
/// files that may not all have come back.
const LISTING: &str = "DIR";

/// The account holding the registry, the logs and the authorized client
/// thumbprints. Not data, and not in a whole-database export.
const SYSTEM: &str = "SYSTEM";

/// Where an archive being moved over the wire is spooled.
///
/// Hidden, for the reason [`crate::db::transaction::LOG_DIR`] is: an account
/// whose directory *is* the storage directory would otherwise list it as one of
/// its files, and the account listing skips names that start with a dot.
///
/// Inside the storage directory rather than the system temporary directory so a
/// spool is on the same filesystem as the data - a rename never crosses a
/// device, and an operator who has given the database room has given this room
/// too. It is also where an abandoned one is findable, which is the whole
/// argument the raw transfer path makes for staging inside the file's own root.
pub const SPOOL_DIR: &str = ".archive";

/// What a restore should do, and what it may not do without being told.
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    /// Restore into this account rather than the one the archive names.
    ///
    /// Only meaningful for an archive of a single account or a single file; a
    /// whole-database archive names many, and renaming them all onto one would
    /// merge accounts that were never together.
    pub into_account: Option<String>,
    /// Replace a file that is already there. Without it, an import that would
    /// land on an existing file is refused whole.
    pub overwrite: bool,
    /// Read the archive, work out what would happen, and change nothing.
    pub dry_run: bool,
}

/// What an import did, or - for a [dry run](ImportPlan::dry_run) - would do.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// What the archive says about itself, from the verify pass.
    pub summary: Summary,
    /// One entry per file in the archive, in its order.
    pub files: Vec<ImportedFile>,
    /// Accounts the import created, in the order it created them.
    pub accounts_created: Vec<String>,
    /// True when nothing was written.
    pub dry_run: bool,
}

impl ImportReport {
    pub fn records(&self) -> u64 {
        self.files.iter().map(|f| f.records).sum()
    }
}

/// What happens, or happened, to one file of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFile {
    /// The account it lands in, which is not the one the archive names when
    /// [`ImportPlan::into_account`] says otherwise.
    pub account: String,
    pub name: String,
    pub action: FileAction,
    pub records: u64,
    pub dictionary: u64,
    pub bytes: u64,
}

/// Whether a restored file was new or took the place of one already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    /// The account had no file of that name.
    Created,
    /// A file of that name was there and was replaced, which
    /// [`ImportPlan::overwrite`] had to be set to allow.
    Replaced,
}

impl FileAction {
    pub fn as_word(self) -> &'static str {
        match self {
            FileAction::Created => "created",
            FileAction::Replaced => "replaced",
        }
    }
}

impl Database {
    // ------------------------------------------------------------- export ---

    /// Writes an archive of `source` to `sink`.
    ///
    /// The flush and the guards are described in the module documentation; the
    /// short of it is that every file named here is held for the length of the
    /// write, so a caller streaming to a slow destination is holding a database
    /// still while it does.
    pub fn export<W: Write>(&self, source: &Source, sink: W) -> DbResult<Trailer> {
        assert_no_table_guard_held("An export");
        // Acknowledged writes that are still only in memory are part of the
        // state being copied, so they are put on disk first. Nothing here reads
        // the section files - the records come from the tables themselves - but
        // a flush is also what settles a table's own bookkeeping, and an
        // archive taken around one is easier to reason about than one taken
        // through it.
        self.save()?;

        let plan = self.export_plan(source)?;
        // Handles resolved with no guard held, exactly as a transaction stages
        // its files: loading a table takes locks of its own.
        let mut held: Vec<(usize, TableHandle)> = Vec::new();
        for (position, target) in plan.iter().enumerate() {
            if target.attributes.is_directory() {
                continue;
            }
            held.push((position, self.get_table_mut_for_account(&target.account, &target.name)?));
        }

        // Taken in the order `export_plan` put them in, which is (account,
        // file) order. Shared, because an export reads: other readers are
        // unaffected and writers wait.
        let guards: Vec<_> = held.iter().map(|(_, handle)| handle.read()).collect();
        let at: HashMap<usize, usize> = held
            .iter()
            .enumerate()
            .map(|(slot, (position, _))| (*position, slot))
            .collect();

        // Built under the guards, so the shape the manifest describes is the
        // shape the records below are read from.
        let files: Vec<FileEntry> = plan
            .iter()
            .enumerate()
            .map(|(position, target)| {
                let indexes = at
                    .get(&position)
                    .map(|slot| index_entries(&guards[*slot]))
                    .unwrap_or_default();
                FileEntry::of(&target.account, &target.name, &target.attributes, indexes)
            })
            .collect();

        let mut writer = archive::Writer::begin(
            sink,
            Manifest {
                archive_format: archive::CURRENT,
                storage_format: crate::db::format::CURRENT,
                taken_millis: now_millis(),
                source: source.clone(),
                files,
            },
        )?;

        for (position, target) in plan.iter().enumerate() {
            writer.begin_file()?;
            match at.get(&position) {
                Some(slot) => {
                    let table = &*guards[*slot];
                    // Sorted so that two exports of the same data are the same
                    // bytes. A hash map's order is not a property anything
                    // should depend on, and "did this backup change?" is a
                    // question an operator will ask of two files.
                    let mut keys: Vec<&String> = table.dictionary.keys().collect();
                    keys.sort_unstable();
                    for key in keys {
                        writer.record(RecordKind::Dictionary, key, &table.dictionary[key].to_bytes())?;
                    }
                    let mut keys: Vec<&String> = table.records.keys().collect();
                    keys.sort_unstable();
                    for key in keys {
                        writer.record(RecordKind::Data, key, &table.records[key].to_bytes())?;
                    }
                }
                None => self.export_directory_records(&target.account, &target.name, &mut writer)?,
            }
            writer.end_file()?;
        }

        writer.finish()
    }

    /// A directory file's records, streamed one at a time.
    ///
    /// Never held whole: a record here is a host file and may be tens of
    /// megabytes. The length comes from the open handle rather than from the
    /// listing, so a record replaced between the two is written at the size it
    /// actually is.
    fn export_directory_records<W: Write>(
        &self,
        account: &str,
        name: &str,
        writer: &mut archive::Writer<W>,
    ) -> DbResult<()> {
        for entry in self.directory_records(account, name)? {
            // Gone since the listing. Left out rather than written as an empty
            // record, which would restore as a file that exists and is blank -
            // a worse answer than its absence.
            let Some((mut handle, length)) = self.open_directory_record(account, name, &entry.key)? else {
                continue;
            };
            writer.record_from(RecordKind::Data, &entry.key, length, &mut handle)?;
        }
        Ok(())
    }

    /// Writes an archive to a host path, and only puts it at that path once it
    /// is whole.
    ///
    /// Tmp-then-rename, the discipline every other durable write here uses. A
    /// backup that failed part way must not be sitting at the name an operator
    /// will reach for in an emergency, and a half-written one that is *named*
    /// like a good one is worse than no backup at all, because it is trusted.
    pub fn export_to_path(&self, source: &Source, path: &Path) -> DbResult<Trailer> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let staging = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or(archive::EXTENSION)
        ));

        let outcome = (|| {
            let file = File::create(&staging)?;
            let mut buffered = BufWriter::new(file);
            let trailer = self.export(source, &mut buffered)?;
            let file = buffered
                .into_inner()
                .map_err(|e| DbError::Io(std::io::Error::other(e.to_string())))?;
            // The archive is on the platter before the name points at it, so a
            // crash cannot leave the name resolving to bytes that never
            // arrived.
            file.sync_all()?;
            Ok(trailer)
        })();

        match outcome {
            Ok(trailer) => {
                fs::rename(&staging, path)?;
                if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    crate::db::hashfile::sync_dir(parent)?;
                }
                Ok(trailer)
            }
            Err(e) => {
                let _ = fs::remove_file(&staging);
                Err(e)
            }
        }
    }

    /// Which files an export covers, in the order their locks are taken.
    fn export_plan(&self, source: &Source) -> DbResult<Vec<ExportTarget>> {
        let mut targets = Vec::new();
        match source {
            Source::File { account, file } => {
                self.require_account(account)?;
                if !self.list_tables_for_account(account).iter().any(|name| name == file) {
                    return Err(DbError::FileNotFound {
                        account: account.clone(),
                        file: file.clone(),
                    });
                }
                if file == LISTING {
                    return Err(DbError::InvalidRequest(format!(
                        "'{}' is the account's listing of its own files, not data. Export the account instead",
                        LISTING
                    )));
                }
                targets.push(ExportTarget {
                    attributes: self.file_attributes_for_account(account, file),
                    account: account.clone(),
                    name: file.clone(),
                });
            }
            Source::Account { account } => {
                self.require_account(account)?;
                self.collect_account(account, &mut targets);
            }
            Source::All => {
                for account in self.list_accounts() {
                    // Left out deliberately, and named here rather than relied
                    // on falling out of the registry: see the module
                    // documentation for why $CLIENTS in particular has no
                    // business travelling inside a routine backup.
                    if account == SYSTEM {
                        continue;
                    }
                    self.collect_account(&account, &mut targets);
                }
            }
        }
        targets.sort_by(|a, b| (&a.account, &a.name).cmp(&(&b.account, &b.name)));
        Ok(targets)
    }

    fn collect_account(&self, account: &str, targets: &mut Vec<ExportTarget>) {
        for (name, attributes) in self.list_tables_with_attributes_for_account(account) {
            if name == LISTING {
                continue;
            }
            targets.push(ExportTarget {
                account: account.to_string(),
                name,
                attributes,
            });
        }
    }

    fn require_account(&self, account: &str) -> DbResult<()> {
        if self.get_account_dir(account).is_none() {
            return Err(DbError::AccountNotFound(account.to_string()));
        }
        Ok(())
    }

    /// A private path to spool an archive through, for the forms of `EXPORT`
    /// and `IMPORT` that carry one over a connection rather than name a file.
    ///
    /// `label` only makes the name readable while an operator is looking at a
    /// directory listing; uniqueness comes from the process id and the clock,
    /// because two connections may be moving archives at the same moment.
    pub fn archive_spool(&self, label: &str) -> DbResult<PathBuf> {
        let dir = Path::new(&self.storage_dir).join(SPOOL_DIR);
        fs::create_dir_all(&dir)?;
        Ok(dir.join(format!(
            "{}-{}-{}.{}",
            label,
            std::process::id(),
            now_millis(),
            archive::EXTENSION
        )))
    }

    /// Removes every spool file left behind by a connection that died mid
    /// transfer.
    ///
    /// Called on open, beside the other sweeps, which is the one moment when no
    /// transfer is in flight. A spool is never part of the database - nothing
    /// reads one that the transfer that made it is not still holding - so this
    /// is reclaiming space rather than repairing anything.
    ///
    /// It sweeps the whole directory rather than only this process's own names,
    /// which is right because two servers sharing a storage directory is not a
    /// supported configuration - the account registry and the transaction log
    /// assume the same thing.
    pub fn sweep_archive_spool(&self) {
        let dir = Path::new(&self.storage_dir).join(SPOOL_DIR);
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let _ = fs::remove_file(entry.path());
        }
    }

    // ------------------------------------------------------------- import ---

    /// Reads an archive and puts it back: verify, plan, apply.
    ///
    /// Nothing is written until the archive has decoded whole and every file it
    /// names has been resolved against what is already there. See the module
    /// documentation for why that ordering is the point rather than a nicety.
    pub fn import(&self, path: &Path, plan: &ImportPlan) -> DbResult<ImportReport> {
        assert_no_table_guard_held("An import");

        // Pass one: the archive decodes and its checksum holds. Buffered,
        // because this is a straight read of the whole file.
        let summary = archive::verify(BufReader::new(File::open(path)?))?;
        let files = self.import_plan(&summary, plan)?;

        if plan.dry_run {
            return Ok(ImportReport {
                summary,
                files,
                accounts_created: Vec::new(),
                dry_run: true,
            });
        }

        // Pass two, from the start. Every account the archive needs exists
        // before a record is read, so a restore cannot fail half way for a
        // reason that was knowable up front.
        let mut accounts_created = Vec::new();
        for account in distinct_accounts(&files) {
            if self.get_account_dir(&account).is_none() {
                self.create_account(&account, None)?;
                accounts_created.push(account);
            }
        }

        let mut sink = Restore {
            db: self,
            files: &files,
            pending: None,
        };
        archive::read(BufReader::new(File::open(path)?), &mut sink)?;
        // Buffered writes belong to the caller's next crash, not to a restore
        // that reported success.
        self.save()?;

        Ok(ImportReport {
            summary,
            files,
            accounts_created,
            dry_run: false,
        })
    }

    /// Resolves every file in the archive against the database as it is, and
    /// refuses the whole import rather than any part of it.
    fn import_plan(&self, summary: &Summary, plan: &ImportPlan) -> DbResult<Vec<ImportedFile>> {
        let renaming = plan.into_account.as_deref();
        if let Some(into) = renaming {
            let accounts = distinct_source_accounts(summary);
            if accounts.len() > 1 {
                return Err(DbError::InvalidRequest(format!(
                    "This archive holds {} accounts ({}), so it cannot be restored into '{}'. Restore it as it is, \
                     or export one account at a time",
                    accounts.len(),
                    accounts.join(", "),
                    into
                )));
            }
        }

        let mut planned = Vec::with_capacity(summary.manifest.files.len());
        let mut collisions = Vec::new();
        for (entry, counts) in summary.files() {
            let account = renaming.unwrap_or(&entry.account).to_string();
            let exists = self.get_account_dir(&account).is_some()
                && self.list_tables_for_account(&account).contains(&entry.name);
            if exists && !plan.overwrite {
                collisions.push(format!("{}/{}", account, entry.name));
            }
            planned.push(ImportedFile {
                account,
                name: entry.name.clone(),
                action: if exists {
                    FileAction::Replaced
                } else {
                    FileAction::Created
                },
                records: counts.records,
                dictionary: counts.dictionary,
                bytes: counts.bytes,
            });
        }

        if !collisions.is_empty() {
            return Err(DbError::InvalidRequest(format!(
                "{} already there and OVERWRITE was not given, so nothing was imported: {}",
                if collisions.len() == 1 {
                    "This file is"
                } else {
                    "These files are"
                },
                collisions.join(", ")
            )));
        }
        Ok(planned)
    }
}

/// One file an export will read.
struct ExportTarget {
    account: String,
    name: String,
    attributes: FileAttributes,
}

/// The index definitions of a locked table, in field order.
fn index_entries(table: &crate::db::models::Table) -> Vec<IndexEntry> {
    table
        .indexes
        .iter()
        .map(|(field, index)| IndexEntry {
            field: field.clone(),
            excluded: index.excluded().iter().cloned().collect(),
        })
        .collect()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn distinct_accounts(files: &[ImportedFile]) -> Vec<String> {
    let mut names: Vec<String> = files.iter().map(|f| f.account.clone()).collect();
    names.sort();
    names.dedup();
    names
}

fn distinct_source_accounts(summary: &Summary) -> Vec<String> {
    let mut names: Vec<String> = summary.manifest.files.iter().map(|f| f.account.clone()).collect();
    names.sort();
    names.dedup();
    names
}

/// The apply pass: creates each file as the manifest describes it and puts the
/// records back.
struct Restore<'a> {
    db: &'a Database,
    files: &'a [ImportedFile],
    pending: Option<Pending>,
}

/// The file currently being restored.
struct Pending {
    account: String,
    name: String,
    /// Held until the file's records are done, then installed in one act, so a
    /// reader never sees a table half way through a restore. `None` for a
    /// directory file, whose records are host files written as they arrive.
    records: Option<(HashMap<String, Record>, HashMap<String, Record>)>,
    indexes: Vec<IndexEntry>,
}

impl Sink for Restore<'_> {
    fn begin_file(&mut self, index: usize, entry: &FileEntry) -> DbResult<()> {
        let planned = self
            .files
            .get(index)
            .ok_or_else(|| DbError::InvalidRequest("The archive changed while it was being read".to_string()))?;

        // Replacing rather than merging. An import of a file onto an existing
        // one restores *that file*, and a merge would leave records from two
        // different points in time under one name with no way to tell which
        // were which.
        if planned.action == FileAction::Replaced {
            self.db.delete_table_for_account(&planned.account, &planned.name)?;
        }
        self.db
            .create_table_with(&planned.account, &planned.name, entry.attributes())?;

        self.pending = Some(Pending {
            account: planned.account.clone(),
            name: planned.name.clone(),
            records: (!entry.is_directory()).then(|| (HashMap::new(), HashMap::new())),
            indexes: entry.indexes.clone(),
        });
        Ok(())
    }

    fn record(
        &mut self,
        _entry: &FileEntry,
        kind: RecordKind,
        key: &str,
        len: u64,
        body: &mut dyn Read,
    ) -> DbResult<()> {
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| DbError::InvalidRequest("A record outside any file".to_string()))?;

        match &mut pending.records {
            Some((records, dictionary)) => {
                let mut bytes = Vec::with_capacity(len as usize);
                body.read_to_end(&mut bytes)?;
                let record = Record::from_bytes(&bytes);
                match kind {
                    RecordKind::Data => records.insert(key.to_string(), record),
                    RecordKind::Dictionary => dictionary.insert(key.to_string(), record),
                };
                Ok(())
            }
            // A directory record is a host file and may be far too large to
            // hold, so it is streamed into place through the same staging the
            // raw transfer path uses - written to a temporary inside the file's
            // own root and renamed once all of it has arrived.
            None => {
                let account = pending.account.clone();
                let name = pending.name.clone();
                let staged = self.db.stage_directory_record(&account, &name, key, len)?;
                let arrived = stream_to(body, &staged)?;
                self.db
                    .commit_directory_record(&account, &name, key, &staged, arrived, len)
            }
        }
    }

    fn end_file(&mut self, _entry: &FileEntry) -> DbResult<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let Some((records, dictionary)) = pending.records else {
            // A directory file's records are already in place.
            return Ok(());
        };

        let handle = self.db.get_table_mut_for_account(&pending.account, &pending.name)?;
        {
            let mut table = handle.write();
            table.records = records;
            table.dictionary = dictionary;
            table.mark_dict_dirty();
            // Every group is rewritten, because the restored records were never
            // written one at a time and there is no set of dirty keys that
            // describes them. This is also what rehashes them into the target's
            // own modulus - the layout of the machine the archive came from is
            // not carried, and should not be.
            table.touch_all();
        }

        // Definitions restored, postings derived. An index built from the
        // records that actually arrived is the only one that describes them;
        // carrying the postings across would restore an index of the source
        // deployment's data.
        for index in &pending.indexes {
            let excluded = index.excluded.iter().cloned().collect();
            let mut table = handle.write();
            if let Err(e) = table.create_index_excluding(&index.field, excluded) {
                drop(table);
                // A field that can no longer be indexed - because the
                // dictionary entry naming it did not come back - is reported
                // and skipped rather than failing a restore that has otherwise
                // succeeded. The records are the thing being saved; an index is
                // derived and can be made again by hand.
                self.db.log_error(
                    &pending.account,
                    &format!(
                        "IMPORT: {}/{} restored, but its index on '{}' could not be rebuilt: {}",
                        pending.account, pending.name, index.field, e
                    ),
                )?;
                continue;
            }
            table.rebuild_index(&index.field)?;
        }
        Ok(())
    }
}

/// Streams a record body into a staged path, reporting what actually arrived.
fn stream_to(body: &mut dyn Read, staged: &Path) -> DbResult<u64> {
    let mut file = BufWriter::new(File::create(staged)?);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let got = body.read(&mut buffer)?;
        if got == 0 {
            break;
        }
        file.write_all(&buffer[..got])?;
        written += got as u64;
    }
    file.flush()?;
    Ok(written)
}
