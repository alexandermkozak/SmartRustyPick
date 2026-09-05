mod cache;
#[cfg(test)]
mod cache_tests;
pub mod queue;

use crate::db::error::{DbError, DbResult};
use crate::db::hashfile::{self, FsyncPolicy, SectionMeta};
use crate::db::health::{HealthSummary, Verdict};
use crate::db::index::{self, IndexReport, IndexStats, IndexValue};
use crate::db::models::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant, SystemTime};

/// How a table is addressed in the cache: the account it belongs to and its
/// name, because two accounts may each have a file of the same name.
pub type TableKey = (String, String);

/// Takes a shared lock, ignoring poisoning.
///
/// A panic in one request leaves the state it was reading no less readable than
/// it was, so refusing every later request would turn one failed command into a
/// dead server. Every lock in the engine is taken through these three helpers,
/// so that decision is made once.
pub(crate) fn rlock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

/// Takes an exclusive lock, ignoring poisoning. See [`rlock`].
pub(crate) fn wlock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

/// Takes a mutex, ignoring poisoning. See [`rlock`].
pub(crate) fn mlock<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|e| e.into_inner())
}

/// A loaded table together with the lock that guards it.
///
/// Handing this out rather than a borrow of the database is what lets two
/// connections write to two different files at the same time: the caller locks
/// the one table it names, and everybody else is only ever blocked by somebody
/// working on that same table. A handle is cheap to clone and keeps the table
/// alive even if the cache evicts it in the meantime.
///
/// See the module documentation on [`Database`] for the order locks have to be
/// taken in.
#[derive(Clone)]
pub struct TableHandle(Arc<RwLock<Table>>);

impl TableHandle {
    fn new(table: Table) -> Self {
        TableHandle(Arc::new(RwLock::new(table)))
    }

    /// Reads the table. Any number of readers may hold this at once.
    pub fn read(&self) -> TableRead<'_> {
        note_guard_taken();
        TableRead(rlock(&self.0))
    }

    /// Takes the table exclusively, for a write or a flush.
    pub fn write(&self) -> TableWrite<'_> {
        note_guard_taken();
        TableWrite(wlock(&self.0))
    }

    /// How many handles to this table exist, the cache's own included. The
    /// cache uses it to leave a table somebody is working on alone.
    fn refs(&self) -> usize {
        Arc::strong_count(&self.0)
    }
}

/// A shared borrow of a table's contents. Behaves as a `&Table`.
pub struct TableRead<'a>(RwLockReadGuard<'a, Table>);

impl std::ops::Deref for TableRead<'_> {
    type Target = Table;

    fn deref(&self) -> &Table {
        &self.0
    }
}

impl Drop for TableRead<'_> {
    fn drop(&mut self) {
        note_guard_released();
    }
}

/// An exclusive borrow of a table's contents. Behaves as a `&mut Table`.
pub struct TableWrite<'a>(RwLockWriteGuard<'a, Table>);

impl std::ops::Deref for TableWrite<'_> {
    type Target = Table;

    fn deref(&self) -> &Table {
        &self.0
    }
}

impl std::ops::DerefMut for TableWrite<'_> {
    fn deref_mut(&mut self) -> &mut Table {
        &mut self.0
    }
}

impl Drop for TableWrite<'_> {
    fn drop(&mut self) {
        note_guard_released();
    }
}

// A debug build counts the table guards each thread is holding, so that
// breaking the locking rule below is a panic naming the rule rather than a
// process that stops and says nothing. Nothing is counted in a release build:
// the two calls compile away and the guards are their inner locks.
#[cfg(debug_assertions)]
thread_local! {
    static HELD_TABLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Every file lock this thread has ever taken. A request on a hot path is
    /// meant to take a fixed, small number of these, and a lookup added to one
    /// of those paths costs a lock on the file every connection is contending
    /// for - which is a few percent of throughput, not a test failure, unless
    /// something counts it. See `table_locks_taken`.
    static TAKEN_TABLES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many file locks this thread has taken since it started.
///
/// Debug builds only, and only meaningful as a difference across a request.
/// The hot-path tests use it to count the work one request does, because that
/// count is a property a test can pin exactly, where the throughput it governs
/// is far too noisy to assert on.
#[cfg(debug_assertions)]
pub fn table_locks_taken() -> u64 {
    TAKEN_TABLES.with(|taken| taken.get())
}

#[cfg(debug_assertions)]
fn note_guard_taken() {
    HELD_TABLES.with(|held| held.set(held.get() + 1));
    TAKEN_TABLES.with(|taken| taken.set(taken.get() + 1));
}

#[cfg(not(debug_assertions))]
fn note_guard_taken() {}

#[cfg(debug_assertions)]
fn note_guard_released() {
    HELD_TABLES.with(|held| held.set(held.get().saturating_sub(1)));
}

#[cfg(not(debug_assertions))]
fn note_guard_released() {}

/// Panics, in a debug build, if the calling thread is holding a table guard.
///
/// A flush locks every dirty file in turn, so starting one while holding a file
/// means waiting for a lock this thread already holds. That is a hang, and a
/// hang is the one failure that arrives with nothing to read - no assertion, no
/// stack, just a test that never finishes. Checking it where the flush starts
/// turns it into a panic that names the caller and the rule it broke.
fn assert_no_table_guard_held(what: &str) {
    #[cfg(debug_assertions)]
    HELD_TABLES.with(|held| {
        let count = held.get();
        assert_eq!(
            count, 0,
            "{what} started while this thread holds {count} table guard(s). \
             A flush locks each dirty file in turn and would wait for a lock it \
             already holds; release the table before flushing. See the locking \
             rules on `Database`.",
        );
    });
    let _ = what;
}

/// What one file has buffered since it was last written out.
///
/// Batching per file rather than per database is what keeps a burst on one file
/// from dragging every other file through a flush - and through that file's
/// lock - with it.
#[derive(Clone, Copy)]
struct WriteMark {
    pending: usize,
    last_flush: Instant,
}

impl WriteMark {
    fn fresh() -> Self {
        WriteMark {
            pending: 0,
            last_flush: Instant::now(),
        }
    }
}

/// The client authorizations, kept together because they are read on every
/// request and rewritten as a unit whenever `SYSTEM/$CLIENTS` changes.
#[derive(Default)]
struct ClientRegistry {
    certs: HashSet<String>,
    clients: HashMap<String, ClientInfo>,
    stamp: Option<TableStamp>,
}

/// The database: a set of accounts, each holding files, cached in memory and
/// flushed to disk in batches.
///
/// # Locking
///
/// A `Database` is shared between every connection, and all of its mutable
/// state sits behind interior locks so that ordinary record work - `READ`,
/// `WRITE`, `DELETE`, `QUERY` - needs nothing more than `&self`. Writers to
/// *different* files therefore never exclude each other, and a flush only
/// excludes the file being flushed.
///
/// Locks must be acquired in this order, and never the other way round:
///
/// 1. the outer `RwLock` a server wraps the database in, if any
///    ([`crate::server::SharedDb`]);
/// 2. `accounts_config` / `registry_stamp` - the account registry;
/// 3. `available_tables` / `available_stamps` - which files each account has;
/// 4. `tables` - the map of loaded tables;
/// 5. `lru_order`;
/// 6. a single table, through its [`TableHandle`];
/// 7. `file_attributes`, `clients`, `pending_writes`, `last_flush`.
///
/// A thread holds **at most one table lock at a time**. Nothing in the engine
/// needs two, and a command that ever does must take them in `(account, name)`
/// order. In particular [`Database::save`] must never be called while a table
/// guard is held: it locks each dirty table in turn and would deadlock on the
/// one already held.
///
/// That last rule is checked rather than merely written down. A debug build
/// counts the table guards each thread holds and panics where a flush starts if
/// any is outstanding, so breaking it fails with a message naming the rule
/// instead of stopping the process and saying nothing. The count and the check
/// compile out of a release build.
pub struct Database {
    pub storage_dir: String,
    session_account: RwLock<String>,
    accounts_config: RwLock<Record>,
    /// Tables in memory, each behind its own lock. The map lock guards
    /// membership only; the tables themselves are locked individually.
    tables: RwLock<HashMap<TableKey, TableHandle>>,
    available_tables: RwLock<HashMap<String, HashSet<String>>>,
    available_stamps: RwLock<HashMap<String, Option<SystemTime>>>,
    lru_order: Mutex<VecDeque<TableKey>>,
    pub max_loaded: usize,
    pub active_select_list: Option<SelectList>,
    /// Select lists built by the remote protocol's `SELECT`, by list name. Each
    /// carries the account it was selected in and its own cursor - see
    /// [`RemoteSelectList`].
    pub remote_select_lists: HashMap<String, RemoteSelectList>,
    clients: RwLock<ClientRegistry>,
    registry_stamp: Mutex<Option<(Option<SystemTime>, u64)>>,
    pub log_detail: String,
    pub max_log_records: usize,
    /// Records each group aims to hold; drives the dynamic modulus.
    pub records_per_group: usize,
    /// When true every write is flushed immediately, trading throughput for
    /// the guarantee that an acknowledged write survives a crash.
    pub durable_writes: bool,
    /// How hard an ordinary buffered flush pushes: handing the bytes to the
    /// page cache is not the same as having them on disk.
    pub fsync: FsyncPolicy,
    /// The policy used for a write that was promised to be durable. Defaults to
    /// [`FsyncPolicy::Always`], because "flushed before the write is
    /// acknowledged" is otherwise a promise about the page cache and not about
    /// the disk. An explicit `fsync` setting overrides it, for the operator who
    /// knowingly trades the guarantee for throughput.
    pub durable_fsync: FsyncPolicy,
    /// Longest a change may sit in memory before the next flush picks it up.
    pub flush_interval: Duration,
    /// Flush once this many writes have accumulated, so a burst is batched but
    /// never grows without bound.
    pub flush_max_pending: usize,
    pending_writes: AtomicUsize,
    last_flush: Mutex<Instant>,
    /// Per-file flush batching, for the writes that name the file they touch.
    write_marks: Mutex<HashMap<TableKey, WriteMark>>,
    /// What each file's DIR entry says about it - durable, queue, and a
    /// queue's claim policy - cached so the write path does not read the DIR
    /// file on every request.
    file_attributes: RwLock<HashMap<TableKey, FileAttributes>>,
}

/// The display width a field without a dictionary width is rendered at.
pub const DEFAULT_FIELD_WIDTH: usize = 10;

/// A table's dictionary resolved to just what serialization emits, so a result
/// set pays the dictionary walk once. Built by
/// [`Database::record_schema`] and consumed by
/// [`Database::serialize_record_with_schema`].
pub struct RecordSchema {
    fields: Vec<RecordSchemaField>,
}

struct RecordSchemaField {
    /// 0-based index into a record's internal `fields` vector.
    field_idx: usize,
    /// camelCase key this field is emitted under.
    camel_key: String,
    /// Pick MDn conversion code, if the field has one.
    conversion: Option<String>,
}

impl Table {
    /// The 0-based index of a dictionary field, or `None` when the field is
    /// unknown. Reading it off the table directly spares the caller a lookup in
    /// `loaded_tables`, which costs two string allocations per call.
    pub fn field_index(&self, field_name: &str) -> Option<usize> {
        if field_name == "ID" {
            return Some(0);
        }
        let rec = self.dictionary.get(field_name)?;
        let idx_str = rec.fields.get(DICT_FIELD_IDX)?.values.first()?.first_text()?;
        match idx_str.parse::<usize>() {
            // Pick attribute 1 is 0-indexed 0 in our internal fields vector
            Ok(idx) if idx > 0 => Some(idx - 1),
            _ => None,
        }
    }

    /// The Pick MDn conversion code of a dictionary field, if it has one.
    pub fn conversion_code(&self, field_name: &str) -> Option<String> {
        let rec = self.dictionary.get(field_name)?;
        Self::conversion_code_from_dict_record(rec).map(str::to_string)
    }

    /// Same as [`conversion_code`], but for a caller that already has the
    /// dictionary record in hand, sparing a second lookup in `dictionary`.
    pub(crate) fn conversion_code_from_dict_record(dict_rec: &Record) -> Option<&str> {
        // Pick MDn conversion is in Field 8
        let code = dict_rec.fields.get(DICT_CONV_IDX)?.values.first()?.sub_values.first()?;
        // A conversion code that is not text is not a conversion code.
        match std::str::from_utf8(code) {
            Ok(code) if !code.is_empty() => Some(code),
            _ => None,
        }
    }

    /// The controlling field this entry names in attribute 5, and the tier it
    /// pairs at, or `None` when it names none.
    ///
    /// An entry naming itself names nothing: it is a typo, and treating it as a
    /// one-field group would be a way to hang the walk below rather than a
    /// useful answer.
    fn controller_of(&self, field_name: &str) -> Option<(String, AssociationDepth)> {
        let rec = self.dictionary.get(field_name)?;
        let controller = rec.get_field_display_string(DICT_ASSOC_IDX).trim().to_string();
        if controller.is_empty() || controller == field_name {
            return None;
        }
        let depth = AssociationDepth::from_attribute(&rec.get_field_display_string(DICT_ASSOC_DEPTH_IDX));
        Some((controller, depth))
    }

    /// The field at the head of this one's chain of controllers, and the tier
    /// it ends up pairing at.
    ///
    /// Following the chain rather than reading one link is what lets a
    /// dictionary nest its tiers the way PICK does - a sub-value field hanging
    /// off a value field that hangs off the controller - and still resolve to
    /// one group. Any `S` on the way down makes the field a sub-value tier
    /// member, because that is the tier it is ultimately paired at.
    ///
    /// A chain that has not ended within [`ASSOC_MAX_DEPTH`] steps is a cycle.
    /// The field is then reported as its own root, so a dictionary that names
    /// itself in a circle resolves to no association rather than to a hang.
    fn association_root(&self, field_name: &str) -> (String, AssociationDepth) {
        let mut name = field_name.to_string();
        let mut depth = AssociationDepth::Value;
        for _ in 0..ASSOC_MAX_DEPTH {
            match self.controller_of(&name) {
                Some((controller, link)) => {
                    if link == AssociationDepth::SubValue {
                        depth = AssociationDepth::SubValue;
                    }
                    name = controller;
                }
                None => return (name, depth),
            }
        }
        (field_name.to_string(), AssociationDepth::Value)
    }

    /// The association group `field_name` belongs to, or `None` when it belongs
    /// to none.
    ///
    /// Resolved by scanning the dictionary, because the relationship is
    /// recorded on the dependents and a controller therefore does not know its
    /// own group. That is the cost of the format that cannot fall out of step
    /// with itself, and it is a scan of a small map held in memory, done once
    /// per query rather than once per record.
    ///
    /// A controller with no dictionary entry of its own is not an error: the
    /// dependents naming it still associate with each other. What is never a
    /// group is a single field, so a lone `BY.EXP` field explodes exactly as it
    /// always has.
    pub fn association(&self, field_name: &str) -> Option<Association> {
        if field_name == "ID" || !self.dictionary.contains_key(field_name) {
            return None;
        }
        let (root, _) = self.association_root(field_name);
        let mut members: Vec<AssociationMember> = Vec::new();
        for name in self.dictionary.keys() {
            let (name_root, depth) = self.association_root(name);
            if name_root != root {
                continue;
            }
            if let Some(index) = self.field_index(name) {
                members.push(AssociationMember {
                    name: name.clone(),
                    index,
                    depth,
                });
            }
        }
        if members.len() < 2 {
            return None;
        }
        members.sort_by(|left, right| left.index.cmp(&right.index).then_with(|| left.name.cmp(&right.name)));
        Some(Association {
            controller: root,
            members,
        })
    }

    /// The 0-based index and conversion code of a dictionary field in a single
    /// dictionary lookup, instead of one lookup per property.
    pub fn field_index_and_conversion(&self, field_name: &str) -> Option<(usize, Option<String>)> {
        if field_name == "ID" {
            return Some((0, None));
        }
        let rec = self.dictionary.get(field_name)?;
        let idx_str = rec.fields.get(DICT_FIELD_IDX)?.values.first()?.first_text()?;
        let idx = match idx_str.parse::<usize>() {
            // Pick attribute 1 is 0-indexed 0 in our internal fields vector
            Ok(idx) if idx > 0 => idx - 1,
            _ => return None,
        };
        Some((idx, Self::conversion_code_from_dict_record(rec).map(str::to_string)))
    }
}

impl Database {
    pub fn new(base_storage_dir: &str, config: Option<crate::config::Config>) -> DbResult<Self> {
        let config = config.unwrap_or_else(crate::config::Config::load);
        let db = Database {
            storage_dir: base_storage_dir.to_string(),
            session_account: RwLock::new(String::new()),
            accounts_config: RwLock::new(Record::new()),
            tables: RwLock::new(HashMap::new()),
            available_tables: RwLock::new(HashMap::new()),
            available_stamps: RwLock::new(HashMap::new()),
            lru_order: Mutex::new(VecDeque::new()),
            max_loaded: config
                .max_loaded_tables
                .filter(|n| *n > 0)
                .unwrap_or(crate::config::DEFAULT_MAX_LOADED_TABLES),
            active_select_list: None,
            remote_select_lists: HashMap::new(),
            clients: RwLock::new(ClientRegistry::default()),
            registry_stamp: Mutex::new(None),
            log_detail: config.log_detail.unwrap_or_else(|| "normal".to_string()),
            max_log_records: config.max_log_records.unwrap_or(100),
            records_per_group: config
                .records_per_group
                .filter(|n| *n > 0)
                .unwrap_or(hashfile::DEFAULT_RECORDS_PER_GROUP),
            durable_writes: config.durable_writes.unwrap_or(false),
            fsync: FsyncPolicy::from_config(config.fsync.as_deref()),
            durable_fsync: match config.fsync.as_deref() {
                Some(value) => FsyncPolicy::from_config(Some(value)),
                None => FsyncPolicy::Always,
            },
            flush_interval: Duration::from_millis(config.flush_interval_ms.unwrap_or(250)),
            flush_max_pending: config.flush_max_pending.unwrap_or(256).max(1),
            pending_writes: AtomicUsize::new(0),
            last_flush: Mutex::new(Instant::now()),
            write_marks: Mutex::new(HashMap::new()),
            file_attributes: RwLock::new(HashMap::new()),
        };

        if !Path::new(&db.storage_dir).exists() {
            fs::create_dir_all(&db.storage_dir)?;
        }

        db.load_account_registry()?;
        db.ensure_system_account()?;

        // Perform all system setup within a single account switch
        db.run_in_system_account(|db| {
            db.ensure_system_files()?;
            db.migrate_legacy_certs()?;
            db.self_heal_system_dictionaries()?;
            db.load_clients_from_table()?;
            Ok(())
        })?;

        Ok(db)
    }

    fn load_account_registry(&self) -> DbResult<()> {
        let registry_path = format!("{}/accounts.reg", self.storage_dir);
        // Stamp before reading, so a write racing with our read is caught next time.
        let stamp = Self::file_stamp(&registry_path);
        if Path::new(&registry_path).exists() {
            let mut map = HashMap::new();
            Self::load_section(&mut map, &registry_path)?;
            if let Some(reg_rec) = map.remove("registry") {
                *wlock(&self.accounts_config) = reg_rec;
            }
        }
        *mlock(&self.registry_stamp) = Some(stamp);
        Ok(())
    }

    /// Re-reads `accounts.reg` when it was modified by another process, so accounts
    /// created or deleted elsewhere are visible without restarting.
    pub fn refresh_account_registry(&self) -> DbResult<()> {
        let registry_path = format!("{}/accounts.reg", self.storage_dir);
        let stamp = Self::file_stamp(&registry_path);
        if *mlock(&self.registry_stamp) == Some(stamp) {
            return Ok(());
        }
        self.load_account_registry()
    }

    /// Reloads the client authorization map when `SYSTEM/$CLIENTS` changed on disk,
    /// so authorizations and revocations made by another process take effect.
    pub fn refresh_clients_if_stale(&self) -> DbResult<()> {
        let _ = self.refresh_account_registry();
        if self.get_account_dir("SYSTEM").is_none() {
            return Ok(());
        }
        let stamp = self.disk_stamp("SYSTEM", "$CLIENTS");
        if rlock(&self.clients).stamp == Some(stamp) {
            return Ok(());
        }
        self.load_clients_from_table()
    }

    fn ensure_system_account(&self) -> DbResult<()> {
        if self.get_account_dir("SYSTEM").is_none() {
            self.create_account("SYSTEM", None)?;
        }
        Ok(())
    }

    fn ensure_system_files(&self) -> DbResult<()> {
        let account = "SYSTEM".to_string();
        self.ensure_available_tables(&account)?;

        // Ensure DIR file exists for SYSTEM account
        if !self.account_has_table(&account, "DIR") {
            self.create_table("DIR")?;
            self.sync_dir_file()?;
        }

        // Ensure mandatory system files exist
        let system_files = vec!["$LOGS", "$ACCOUNTS", "$CLIENTS", "$SAVEDLISTS"];
        for file in system_files {
            if !self.account_has_table(&account, file) {
                self.create_table(file)?;
            }
        }

        // Populate $ACCOUNTS with all non-SYSTEM accounts
        let mut accounts_to_list = Vec::new();
        {
            let config = rlock(&self.accounts_config);
            if let Some(names_field) = config.fields.first()
                && let Some(dirs_field) = config.fields.get(1)
            {
                for (i, v) in names_field.values.iter().enumerate() {
                    if let Some(name) = v.first_text()
                        && name != "SYSTEM"
                        && let Some(dir) = dirs_field.values.get(i).and_then(|v| v.first_text())
                    {
                        accounts_to_list.push((name.to_string(), dir.to_string()));
                    }
                }
            }
        }

        let handle = self.get_table_mut("$ACCOUNTS")?;
        let mut accounts_table = handle.write();
        for (name, dir) in accounts_to_list {
            let mut record = Record::new();
            record.fields.resize_with(SYS_ACCOUNTS_PATH_IDX + 1, Field::default);
            record.fields[SYS_ACCOUNTS_PATH_IDX].values.push(Value::text(dir));
            accounts_table.records.insert(name, record);
        }
        accounts_table.touch_all();
        Ok(())
    }

    fn migrate_legacy_certs(&self) -> DbResult<()> {
        let certs_path = format!("{}/certs.reg", self.storage_dir);
        if !Path::new(&certs_path).exists() {
            return Ok(());
        }

        let mut map = HashMap::new();
        if Self::load_section(&mut map, &certs_path).is_ok()
            && let Some(certs_rec) = map.remove("certs")
            && let Some(f) = certs_rec.fields.first()
        {
            let handle = self.get_table_mut("$CLIENTS")?;
            let mut table = handle.write();
            for v in &f.values {
                for sv in &v.sub_values {
                    if !sv.is_empty() {
                        let thumbprint = text_of(sv);
                        let tp_lower = thumbprint.to_lowercase();
                        // Migrate if not already present
                        let already_exists = table.records.values().any(|r| {
                            r.fields
                                .first()
                                .and_then(|f| f.values.first())
                                .and_then(|v| v.first_text())
                                .as_deref()
                                == Some(tp_lower.as_str())
                        });
                        if !already_exists {
                            let mut rec = Record::new();
                            while rec.fields.len() <= SYS_CLIENTS_ADMIN_IDX {
                                rec.fields.push(Field::default());
                            }
                            rec.fields[SYS_CLIENTS_THUMBPRINT_IDX]
                                .values
                                .push(Value::text(&tp_lower));
                            rec.fields[SYS_CLIENTS_ADMIN_IDX].values.push(Value::text("Y"));
                            table.insert_record(&format!("migrated_{}", &thumbprint[..8]), rec);
                        }
                    }
                }
            }
        }
        let _ = fs::rename(&certs_path, format!("{}.migrated", certs_path));
        Ok(())
    }

    fn self_heal_system_dictionaries(&self) -> DbResult<()> {
        let account = self.current_account();
        if account.is_empty() {
            return Ok(());
        }
        let table_names: Vec<String> = self
            .account_tables(&account)?
            .into_iter()
            .filter(|n| n.starts_with('$') || n == "DIR")
            .collect();

        let mut any_updated = false;
        for table_name in table_names {
            if self.ensure_default_dictionaries(&table_name)? {
                any_updated = true;
            }
        }

        if any_updated {
            self.save()?;
        }
        Ok(())
    }

    fn ensure_default_dictionaries(&self, table_name: &str) -> DbResult<bool> {
        let mut updated = false;
        let handle = self.get_table_mut(table_name)?;
        let mut table = handle.write();
        match table_name {
            "$LOGS" => {
                if !table.dictionary.contains_key("MESSAGE") {
                    table
                        .dictionary
                        .insert("MESSAGE".to_string(), Record::from_display_string("1^MESSAGE^L^60"));
                    updated = true;
                }
                if !table.dictionary.contains_key("DETAIL") {
                    table
                        .dictionary
                        .insert("DETAIL".to_string(), Record::from_display_string("2^DETAIL^L^40"));
                    updated = true;
                }
            }
            "$ACCOUNTS" => {
                if !table.dictionary.contains_key("PATH") {
                    table
                        .dictionary
                        .insert("PATH".to_string(), Record::from_display_string("1^PATH^L^50"));
                    updated = true;
                }
            }
            "$CLIENTS" => {
                if !table.dictionary.contains_key("THUMBPRINT") {
                    table.dictionary.insert(
                        "THUMBPRINT".to_string(),
                        Record::from_display_string("1^THUMBPRINT^L^64"),
                    );
                    updated = true;
                }
                if !table.dictionary.contains_key("ACCOUNTS") {
                    table
                        .dictionary
                        .insert("ACCOUNTS".to_string(), Record::from_display_string("2^ACCOUNTS^L^30"));
                    updated = true;
                }
                if !table.dictionary.contains_key("ADMIN") {
                    table
                        .dictionary
                        .insert("ADMIN".to_string(), Record::from_display_string("3^ADMIN^L^5"));
                    updated = true;
                }
            }
            "$SAVEDLISTS" => {
                if !table.dictionary.contains_key("TABLE") {
                    table
                        .dictionary
                        .insert("TABLE".to_string(), Record::from_display_string("1^TABLE^L^20"));
                    updated = true;
                }
                if !table.dictionary.contains_key("IS_DICT") {
                    table
                        .dictionary
                        .insert("IS_DICT".to_string(), Record::from_display_string("2^IS_DICT^L^1"));
                    updated = true;
                }
            }
            "DIR" => {
                if !table.dictionary.contains_key("TYPE") {
                    table
                        .dictionary
                        .insert("TYPE".to_string(), Record::from_display_string("1^TYPE^L^1"));
                    updated = true;
                }
                if !table.dictionary.contains_key("DURABLE") {
                    table
                        .dictionary
                        .insert("DURABLE".to_string(), Record::from_display_string("2^DURABLE^L^7"));
                    updated = true;
                }
            }
            _ => {}
        }
        if updated {
            table.mark_dict_dirty();
        }
        Ok(updated)
    }

    pub fn load_clients_from_table(&self) -> DbResult<()> {
        // Stamp before reading, so a concurrent write is detected on the next check.
        let stamp = self.disk_stamp("SYSTEM", "$CLIENTS");
        let handle = self.get_table_mut_for_account("SYSTEM", "$CLIENTS")?;
        let table = handle.read();
        let mut clients = Vec::new();
        for (name, record) in table.records.iter() {
            if let Some(tp) = record
                .fields
                .get(SYS_CLIENTS_THUMBPRINT_IDX)
                .and_then(|f| f.values.first())
                .and_then(|v| v.first_text())
            {
                let tp_lower = tp.to_lowercase();
                let mut allowed_accounts = Vec::new();
                if let Some(acc_field) = record.fields.get(SYS_CLIENTS_ACCOUNTS_IDX) {
                    for v in &acc_field.values {
                        if let Some(acc) = v.first_text()
                            && !acc.is_empty()
                        {
                            allowed_accounts.push(acc.to_string());
                        }
                    }
                }
                let is_admin = record
                    .fields
                    .get(SYS_CLIENTS_ADMIN_IDX)
                    .and_then(|f| f.values.first())
                    .and_then(|v| v.first_text())
                    .map(|s| s == "Y")
                    .unwrap_or(false);
                clients.push(ClientInfo {
                    name: name.clone(),
                    thumbprint: tp_lower,
                    allowed_accounts,
                    is_admin,
                });
            }
        }
        // The table lock goes before the registry lock is taken, so a reader of
        // the authorizations never waits on a table read.
        drop(table);
        let mut registry = wlock(&self.clients);
        registry.stamp = Some(stamp);
        registry.clients.clear();
        registry.certs.clear();
        for info in clients {
            let tp = info.thumbprint.clone();
            registry.clients.insert(tp.clone(), info);
            registry.certs.insert(tp);
        }
        Ok(())
    }

    /// The client authorized to present `thumbprint`, if any. Cloned rather
    /// than borrowed so the registry lock is released before the request that
    /// asked runs.
    pub fn client_for_thumbprint(&self, thumbprint: &str) -> Option<ClientInfo> {
        rlock(&self.clients).clients.get(thumbprint).cloned()
    }

    /// Every authorized client, by name.
    pub fn authorized_clients(&self) -> Vec<ClientInfo> {
        let mut clients: Vec<ClientInfo> = rlock(&self.clients).clients.values().cloned().collect();
        clients.sort_by(|a, b| a.name.cmp(&b.name));
        clients
    }

    /// How many clients are authorized, for a management view that only wants
    /// the number.
    pub fn authorized_client_count(&self) -> usize {
        rlock(&self.clients).clients.len()
    }

    /// The thumbprints of every authorized certificate.
    pub fn authorized_certs(&self) -> HashSet<String> {
        rlock(&self.clients).certs.clone()
    }

    pub fn run_in_system_account<F, R>(&self, f: F) -> DbResult<R>
    where
        F: FnOnce(&Database) -> DbResult<R>,
    {
        let original_account = self.current_account();
        let already_system = original_account == "SYSTEM";

        if !already_system {
            self.logto("SYSTEM")?;
        }

        let result = f(self);

        if !already_system {
            if original_account.is_empty() {
                self.logout();
            } else {
                let _ = self.logto(&original_account);
            }
        }
        result
    }

    /// The account this session is logged into.
    ///
    /// A session concept: the server names the account on every request and
    /// never looks at this, so it exists for the CLI and for the `SYSTEM`
    /// excursions [`run_in_system_account`](Self::run_in_system_account) makes.
    pub fn current_account(&self) -> String {
        rlock(&self.session_account).clone()
    }

    /// True when this session is logged into `account`.
    pub fn is_current_account(&self, account: &str) -> bool {
        *rlock(&self.session_account) == account
    }

    /// True when no account is selected.
    pub fn has_no_current_account(&self) -> bool {
        rlock(&self.session_account).is_empty()
    }

    /// Points the session at `account` without flushing anything.
    ///
    /// [`logto`](Self::logto) is the ordinary way in, and saves the previous
    /// account's pending changes first; this is for a caller that wants only
    /// the label changed - clearing it to run as no-one in particular, or the
    /// `SYSTEM` excursions that put it back afterwards.
    pub fn set_current_account(&self, account: &str) {
        *wlock(&self.session_account) = account.to_string();
    }

    pub fn logout(&self) {
        let _ = self.save();
        self.set_current_account("");
    }

    pub fn logto(&self, account_name: &str) -> DbResult<()> {
        if self.get_account_dir(account_name).is_none() {
            let _ = self.refresh_account_registry();
        }
        let _account_dir = self
            .get_account_dir(account_name)
            .ok_or_else(|| DbError::AccountNotFound(account_name.to_string()))?;

        if !self.is_current_account(account_name) {
            self.save()?; // Save current account's dirty tables
            self.set_current_account(account_name);
            self.ensure_available_tables(account_name)?;
        }
        Ok(())
    }

    pub fn create_account(&self, name: &str, directory: Option<&str>) -> DbResult<()> {
        // Pick up accounts registered by another process, otherwise persisting our own
        // snapshot of the registry would erase them.
        let _ = self.refresh_account_registry();
        if self.get_account_dir(name).is_some() {
            return Err(DbError::AccountExists(name.to_string()));
        }

        let dir = directory
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}/{}", self.storage_dir, name));
        if !Path::new(&dir).exists() {
            fs::create_dir_all(&dir)?;
        }

        // Update registry
        let prev_acc = self.current_account();
        self.set_current_account("SYSTEM"); // Temporarily switch to SYSTEM context for registry

        // Add to accounts_config record
        {
            let mut config = wlock(&self.accounts_config);
            while config.fields.len() < 2 {
                config.fields.push(Field::default());
            }
            config.fields[0].values.push(Value::text(name));
            config.fields[1].values.push(Value::text(&dir));
        }

        self.persist_account_registry()?;

        // Update $ACCOUNTS table if it exists
        self.run_in_system_account(|db| {
            if rlock(&db.available_tables)
                .get("SYSTEM")
                .map(|s| s.contains("$ACCOUNTS"))
                .unwrap_or(false)
            {
                {
                    let handle = db.get_table_mut("$ACCOUNTS")?;
                    let mut accounts_table = handle.write();
                    let mut record = Record::new();
                    record.fields.resize_with(SYS_ACCOUNTS_PATH_IDX + 1, Field::default);
                    record.fields[SYS_ACCOUNTS_PATH_IDX].values.push(Value::text(&dir));
                    accounts_table.insert_record(name, record);
                }
                db.save()?;
            }
            Ok(())
        })?;

        // An account describes its files in a DIR file, and everything that
        // reads one - the CLI's LIST, the per-file durability flags - treats a
        // missing DIR as an error rather than as an empty account. Creating it
        // with the account means no caller has to remember to, which is how an
        // account created over the protocol used to end up without one until
        // somebody logged into it from the CLI and answered a prompt.
        self.ensure_dir_file_for_account(name)?;

        if !prev_acc.is_empty() && prev_acc != "SYSTEM" {
            let _ = self.logto(&prev_acc);
        } else if prev_acc.is_empty() {
            self.set_current_account("");
        }
        Ok(())
    }

    fn persist_account_registry(&self) -> DbResult<()> {
        let mut map = HashMap::new();
        map.insert("registry".to_string(), rlock(&self.accounts_config).clone());
        let path = format!("{}/accounts.reg", self.storage_dir);
        Self::save_section(&map, &path)?;
        *mlock(&self.registry_stamp) = Some(Self::file_stamp(&path));
        Ok(())
    }

    pub fn delete_account(&self, name: &str) -> DbResult<()> {
        if name == "SYSTEM" {
            return Err(DbError::AccountProtected("SYSTEM".to_string()));
        }

        let _ = self.refresh_account_registry();
        let dir = self
            .get_account_dir(name)
            .ok_or_else(|| DbError::AccountNotFound(name.to_string()))?;

        // Remove from registry
        {
            let mut config = wlock(&self.accounts_config);
            let position = config
                .fields
                .first()
                .and_then(|names| names.values.iter().position(|v| v.first_bytes() == name.as_bytes()));
            if let Some(pos) = position {
                config.fields[0].values.remove(pos);
                if let Some(dirs_field) = config.fields.get_mut(1) {
                    dirs_field.values.remove(pos);
                }
            }
        }

        // Persist registry
        self.persist_account_registry()?;

        // Remove from $ACCOUNTS table
        self.run_in_system_account(|db| {
            db.get_table_mut("$ACCOUNTS")?.write().remove_record(name);
            db.save()
        })?;

        // Delete physical directory
        let _ = fs::remove_dir_all(dir);

        // Cleanup cache for this account
        let keys_to_remove: Vec<TableKey> = {
            let mut tables = wlock(&self.tables);
            let keys: Vec<TableKey> = tables.keys().filter(|(acc, _)| acc == name).cloned().collect();
            for key in &keys {
                tables.remove(key);
            }
            keys
        };
        for key in keys_to_remove {
            self.forget_lru(&key);
            wlock(&self.file_attributes).remove(&key);
        }
        wlock(&self.available_tables).remove(name);
        wlock(&self.available_stamps).remove(name);

        if self.is_current_account(name) {
            self.set_current_account("");
        }

        Ok(())
    }

    pub fn get_account_dir(&self, account_name: &str) -> Option<String> {
        let config = rlock(&self.accounts_config);
        let names_field = config.fields.first()?;
        let dirs_field = config.fields.get(1)?;
        let pos = names_field
            .values
            .iter()
            .position(|v| v.first_bytes() == account_name.as_bytes())?;
        dirs_field.values.get(pos)?.first_text().map(|dir| dir.to_string())
    }

    pub fn current_storage_dir(&self) -> String {
        self.get_account_dir(&self.current_account())
            .unwrap_or_else(|| self.storage_dir.clone())
    }

    /// Writes the pending changes of one table and clears its dirty state.
    ///
    /// Only the groups holding changed keys are rewritten, unless the table was
    /// edited in bulk, is still in the legacy flat format, or the modulus has
    /// to change - all of which need a full rewrite anyway.
    ///
    /// Takes the handle rather than the name so it can run with the table map
    /// locked - the eviction path needs exactly that - and so a flush of one
    /// file never blocks work on another.
    fn flush_handle(&self, key: &TableKey, handle: &TableHandle) -> io::Result<()> {
        let (account, name) = (&key.0, &key.1);
        let storage = self.account_storage_dir(account);
        let per_group = self.records_per_group;
        let data_path = format!("{}/{}/data", storage, name);
        let dict_path = format!("{}/{}/dict", storage, name);

        // A file the caller marked durable is worth a real fsync: "flushed
        // before the write is acknowledged" has to mean on disk, not merely in
        // the page cache. Read from the cache rather than the DIR file so the
        // flush path stays free of I/O.
        let fsync = if self.durable_writes
            || rlock(&self.file_attributes)
                .get(key)
                .map(|attributes| attributes.durable)
                .unwrap_or(false)
        {
            self.durable_fsync
        } else {
            self.fsync
        };

        let mut table = handle.write();
        if !table.is_dirty() {
            return Ok(());
        }
        let table = &mut *table;
        // An index that has fallen behind is reconciled before anything is
        // written, so what lands on disk is an index that matches the records
        // beside it rather than one that has to be caught up on the next load.
        table.rebuild_stale_indexes();

        if table.records_dirty() {
            let incremental = if table.dirty_all || table.legacy_data {
                None
            } else {
                Some(&table.dirty_keys)
            };
            table.data_meta = hashfile::save_with_fsync(
                &data_path,
                &table.records,
                table.data_meta,
                incremental,
                per_group,
                fsync,
            )?;
            if table.legacy_data {
                let _ = fs::remove_file(&data_path);
                table.legacy_data = false;
            }
        }
        if table.dict_dirty {
            Self::save_section(&table.dictionary, &dict_path)?;
            if fsync == FsyncPolicy::Always {
                File::open(&dict_path)?.sync_all()?;
            }
        }
        // Indexes last, and each one's `state` last of all: `state.data_version`
        // must never name a data version that is not already on disk. A crash
        // anywhere in here leaves an index whose recorded version is behind the
        // data's, which is exactly the mismatch the next load rebuilds on.
        let file_dir = format!("{}/{}", storage, name);
        let data_version = table.data_meta.version;
        for (field, index) in table.indexes.iter_mut() {
            index.save(&index::section_path(&file_dir, field), data_version, per_group, fsync)?;
        }
        // The queue's book last, for the same reason the indexes' `state` files
        // are: it names records, so it must never be written ahead of the
        // records it names. A delivery count for a record that is not there is
        // dropped on the next load; a record with no delivery count merely
        // starts its retries again.
        if let Some(state) = table.queue.as_mut()
            && state.is_dirty()
        {
            queue::persist(&file_dir, state, fsync)?;
        }
        table.clear_dirty();
        // Stamped here, under the guard that did the writing, rather than by
        // the caller afterwards. Two threads flushing the same file take this
        // guard in turn, so the stamp can only ever move forwards; read after
        // the guard was released, the older flush could record its stamp last
        // and leave the table looking stale. It would then be dropped from the
        // cache and read back from disk - which for records is merely wasted
        // work, but for a queue file throws away every claim held in memory and
        // fails the next ACK.
        table.stamp = Some(self.stamp_after_flush(account, name, table));
        Ok(())
    }

    /// Creates an empty hashed record section for a brand new table. Existing
    /// sections are left untouched, so re-creating a table cannot wipe it.
    fn init_data_section(table_dir: &str, per_group: usize) -> io::Result<SectionMeta> {
        let data_path = format!("{}/data", table_dir);
        if let Some(meta) = hashfile::read_meta(&data_path) {
            return Ok(meta);
        }
        if Path::new(&data_path).exists() {
            // A legacy flat file: leave it for the migration on first flush.
            return Ok(SectionMeta::empty());
        }
        hashfile::save(&data_path, &HashMap::new(), SectionMeta::empty(), None, per_group)
    }

    pub fn account_storage_dir(&self, account_name: &str) -> String {
        self.get_account_dir(account_name)
            .unwrap_or_else(|| self.storage_dir.clone())
    }

    /// Writes every pending change to disk immediately.
    ///
    /// The tables are snapshotted first and then locked one at a time, so a
    /// flush of a large file never holds up work on any other file. Because it
    /// locks tables, it must not be called while a table guard is held - see
    /// the locking rules on [`Database`].
    pub fn save(&self) -> DbResult<()> {
        assert_no_table_guard_held("A full flush");
        let snapshot: Vec<(TableKey, TableHandle)> = rlock(&self.tables)
            .iter()
            .map(|(key, handle)| (key.clone(), handle.clone()))
            .collect();
        let mut clients_updated = false;
        for (key, handle) in snapshot {
            let was_dirty = handle.read().is_dirty();
            if key.0 == "SYSTEM" && key.1 == "$CLIENTS" && was_dirty {
                clients_updated = true;
            }
            if !was_dirty {
                // A table we did not write keeps the stamp it has. Refreshing it
                // would mark a snapshot that is already stale on disk as up to
                // date, and the freshness check would stop reloading it - which
                // is why the stamp is now taken inside `flush_handle`, where it
                // can only follow a write this thread actually made.
                continue;
            }
            self.flush_handle(&key, &handle)?;
        }
        self.pending_writes.store(0, Ordering::Relaxed);
        mlock(&self.write_marks).clear();
        *mlock(&self.last_flush) = Instant::now();
        if clients_updated {
            self.load_clients_from_table()?;
        }
        Ok(())
    }

    /// True while changes are held in memory and not yet on disk.
    pub fn has_pending_writes(&self) -> bool {
        let snapshot: Vec<TableHandle> = rlock(&self.tables).values().cloned().collect();
        snapshot.iter().any(|handle| handle.read().is_dirty())
    }

    pub fn pending_write_count(&self) -> usize {
        self.pending_writes.load(Ordering::Relaxed)
            + mlock(&self.write_marks)
                .values()
                .map(|mark| mark.pending)
                .sum::<usize>()
    }

    /// Records a write that does not name the file it touched, and flushes the
    /// whole database when the batch is full or the flush interval has elapsed.
    ///
    /// Saving on every request meant one disk write per record even when a
    /// client streamed thousands of them; batching turns that into one write per
    /// group per interval. The cost is a bounded window (`flush_interval`) in
    /// which an acknowledged write is only in memory - set `durable_writes` to
    /// trade the throughput back for it.
    ///
    /// The server names the file on every write and so goes through
    /// [`note_write_for`](Self::note_write_for) instead; this is for a caller
    /// that has edited something without saying what.
    pub fn note_write(&self) -> DbResult<()> {
        if self.durable_writes {
            return self.save();
        }
        let pending = self.pending_writes.fetch_add(1, Ordering::Relaxed) + 1;
        let elapsed = mlock(&self.last_flush).elapsed();
        if pending >= self.flush_max_pending || elapsed >= self.flush_interval {
            return self.save();
        }
        Ok(())
    }

    /// Records a write to one named file, batching and flushing per file.
    ///
    /// This is the write path the server uses. Two things follow from naming the
    /// file. A file marked durable in its account's DIR entry is flushed before
    /// the write is acknowledged even when the rest of the database is
    /// buffering, which lets mission critical files opt out of the in-memory
    /// window without slowing everything else down. And an ordinary buffered
    /// write is batched against that file's own counter and flushed on its own,
    /// so a burst on one file neither writes out nor waits for any other.
    pub fn note_write_for(&self, account: &str, name: &str) -> DbResult<()> {
        if self.is_table_durable_for_account(account, name) {
            // A file promised to be durable is flushed before the write is
            // acknowledged - and so is everything else buffered at the time,
            // because a promise about the disk is not worth much if it leaves
            // the rest of the database behind in memory.
            return self.save();
        }
        // Everything else batches per file, so two connections writing to two
        // files never flush - or wait on - each other's.
        let key = (account.to_string(), name.to_string());
        let due = {
            let mut marks = mlock(&self.write_marks);
            let mark = marks.entry(key.clone()).or_insert_with(WriteMark::fresh);
            mark.pending += 1;
            mark.pending >= self.flush_max_pending || mark.last_flush.elapsed() >= self.flush_interval
        };
        if due {
            self.flush_table(&key)?;
        }
        Ok(())
    }

    /// Writes out one file and clears its batch, leaving every other file's
    /// buffer, and every other file's lock, untouched.
    fn flush_table(&self, key: &TableKey) -> io::Result<()> {
        assert_no_table_guard_held("A flush of one file");
        if let Some(handle) = self.get_table_read_only_for_account(&key.0, &key.1)
            && handle.read().is_dirty()
        {
            self.flush_handle(key, &handle)?;
            if key.0 == "SYSTEM" && key.1 == "$CLIENTS" {
                self.load_clients_from_table()?;
            }
        }
        mlock(&self.write_marks).insert(key.clone(), WriteMark::fresh());
        Ok(())
    }

    /// Flushes if the interval has elapsed. Intended for a background ticker,
    /// so an idle server still persists the tail of a burst promptly.
    pub fn flush_if_due(&self) -> DbResult<bool> {
        if self.pending_write_count() == 0 && !self.has_pending_writes() {
            return Ok(false);
        }
        if mlock(&self.last_flush).elapsed() < self.flush_interval {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    fn load_section(map: &mut HashMap<String, Record>, path: &str) -> io::Result<()> {
        if !Path::new(path).exists() {
            return Ok(());
        }
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        loop {
            let mut len_bytes = [0u8; 8];
            if let Err(e) = reader.read_exact(&mut len_bytes) {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    break;
                }
                return Err(e);
            }
            let key_len = u64::from_le_bytes(len_bytes) as usize;
            if key_len > 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Key too large: {} bytes in {}", key_len, path),
                ));
            }
            let mut key_bytes = vec![0u8; key_len];
            reader.read_exact(&mut key_bytes)?;
            let key = String::from_utf8_lossy(&key_bytes).to_string();

            let mut data_len_bytes = [0u8; 8];
            reader.read_exact(&mut data_len_bytes)?;
            let data_len = u64::from_le_bytes(data_len_bytes) as usize;
            if data_len > 100 * 1024 * 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Record too large: {} bytes for key '{}' in {}", data_len, key, path),
                ));
            }

            let mut data = vec![0u8; data_len];
            reader.read_exact(&mut data)?;
            map.insert(key, Record::from_bytes(&data));
        }
        Ok(())
    }

    fn save_section(map: &HashMap<String, Record>, path: &str) -> io::Result<()> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        let mut keys: Vec<_> = map.keys().cloned().collect();
        keys.sort();

        for key in keys {
            let record = map.get(&key).unwrap();
            let key_bytes = key.as_bytes();
            writer.write_all(&(key_bytes.len() as u64).to_le_bytes())?;
            writer.write_all(key_bytes)?;

            let data = record.to_bytes();
            writer.write_all(&(data.len() as u64).to_le_bytes())?;
            writer.write_all(&data)?;
        }
        writer.flush()?;
        Ok(())
    }

    pub fn list_tables(&self) -> Vec<String> {
        self.list_tables_for_account(&self.current_account())
    }

    pub fn list_tables_for_account(&self, account: &str) -> Vec<String> {
        let mut tables = self.account_tables(account).unwrap_or_default();
        tables.sort();
        tables
    }

    /// Every file in the account with what its `DIR` entry says about it,
    /// sorted by name.
    ///
    /// The attributes are otherwise only readable by reading the account's
    /// `DIR` file, which is an obscure interface for something a client may
    /// reasonably want beside the name - and "which of these is a queue" is a
    /// question a listing should answer without opening every file.
    pub fn list_tables_with_attributes_for_account(&self, account: &str) -> Vec<(String, FileAttributes)> {
        self.list_tables_for_account(account)
            .into_iter()
            .map(|name| {
                let mut attributes = self.file_attributes_for_account(account, &name);
                // A database running wholly in durable mode makes every file
                // durable whatever its entry says, and a listing that reported
                // otherwise would be describing the entry rather than the file.
                attributes.durable |= self.durable_writes;
                (name, attributes)
            })
            .collect()
    }

    /// Every account in the registry, sorted, picking up accounts created by
    /// another process since the last look.
    pub fn list_accounts(&self) -> Vec<String> {
        let _ = self.refresh_account_registry();
        let mut names: Vec<String> = rlock(&self.accounts_config)
            .fields
            .first()
            .map(|f| {
                f.values
                    .iter()
                    .filter_map(|v| v.first_text().map(|name| name.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names.dedup();
        names
    }

    /// Summarises every account for a management view.
    ///
    /// Record counts come from each file's section metadata, so a large account
    /// is described without any of it being read into memory.
    pub fn account_statistics(&self) -> Vec<AccountStats> {
        self.list_accounts()
            .into_iter()
            .map(|name| {
                let directory = self.account_storage_dir(&name);
                let files = self.list_tables_for_account(&name);
                let record_count = files
                    .iter()
                    .map(|file| self.file_record_count(&directory, &name, file))
                    .sum();
                let (disk_bytes, _) = Self::tree_stats(Path::new(&directory));
                // The roll-up is what makes a problem findable without opening
                // every file in the account in turn, so it is the cheap check -
                // metadata and index `state` files only - repeated per file.
                let mut health = HealthSummary::good();
                let (mut index_count, mut stale_indexes, mut unhealthy_files) = (0, 0, 0);
                for file in &files {
                    let file_dir = format!("{}/{}", directory, file);
                    let data_version = hashfile::read_meta(&format!("{}/data", file_dir))
                        .map(|meta| meta.version)
                        .unwrap_or(0);
                    for field in index::indexed_fields(&file_dir) {
                        index_count += 1;
                        if index::read_state(&index::section_path(&file_dir, &field))
                            .is_none_or(|state| state.data_version != data_version)
                        {
                            stale_indexes += 1;
                        }
                    }
                    let summary = self.file_health_summary(&name, file);
                    if summary.verdict != Verdict::Good {
                        unhealthy_files += 1;
                    }
                    health.absorb(&summary);
                }
                if unhealthy_files > 0 {
                    health.reasons = vec![format!("{} of {} files need attention", unhealthy_files, files.len())];
                }
                AccountStats {
                    name,
                    directory,
                    file_count: files.len(),
                    record_count,
                    disk_bytes,
                    index_count,
                    stale_indexes,
                    unhealthy_files,
                    health,
                }
            })
            .collect()
    }

    /// Records in one file, preferring the in-memory table when it is loaded so
    /// that writes not yet flushed are still counted.
    fn file_record_count(&self, directory: &str, account: &str, file: &str) -> u64 {
        if let Some(handle) = self.get_table_read_only_for_account(account, file) {
            return handle.read().records.len() as u64;
        }
        let data_path = format!("{}/{}/data", directory, file);
        match hashfile::read_meta(&data_path) {
            Some(meta) => meta.records,
            // Pre-hashfile flat file: the count is only in the file itself.
            None => {
                let mut map = HashMap::new();
                let _ = Self::load_section(&mut map, &data_path);
                map.len() as u64
            }
        }
    }

    /// Total bytes under `path` and the most recent modification time found
    /// there. Returns zeroes for a path that does not exist.
    fn tree_stats(path: &Path) -> (u64, Option<SystemTime>) {
        let mut bytes = 0;
        let mut newest = None;
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let metadata = match entry.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                if metadata.is_dir() {
                    stack.push(entry.path());
                    continue;
                }
                bytes += metadata.len();
                if let Ok(modified) = metadata.modified() {
                    newest = Some(match newest {
                        Some(current) if current > modified => current,
                        _ => modified,
                    });
                }
            }
        }
        (bytes, newest)
    }

    /// Describes one file: how many records it holds, how they are spread over
    /// the hash groups and what it costs on disk. No record is returned, and
    /// none is loaded to answer this unless the file is in the legacy format.
    pub fn file_statistics(&self, account: &str, name: &str) -> DbResult<FileStats> {
        if !self.account_has_table(account, name) {
            self.scan_available_tables(account)?;
        }
        if !self.account_has_table(account, name) {
            return Err(DbError::FileNotFound {
                account: account.to_string(),
                file: name.to_string(),
            });
        }

        let durable = self.is_table_durable_for_account(account, name);
        let directory = self.account_storage_dir(account);
        let file_dir = format!("{}/{}", directory, name);
        let data_path = format!("{}/data", file_dir);
        let meta = hashfile::read_meta(&data_path);
        // The group *trailers*, not just the file lengths: the record count of
        // each group is in its 20-byte trailer, so the true distribution costs
        // one seek per group and loads nothing. Bytes per group were all this
        // ever reported, and bytes are not the thing that hurts.
        let groups = hashfile::group_profile(&data_path);
        let group_sizes: Vec<u64> = {
            let mut sizes: Vec<u64> = groups.iter().map(|group| group.bytes).collect();
            sizes.sort();
            sizes
        };
        let (disk_bytes, modified) = Self::tree_stats(Path::new(&file_dir));

        // Before the cache is looked at, because answering it loads the file: a
        // queue's depth and in-flight count live in the order held in memory,
        // which is the one figure here that cannot be read off the disk. Swept
        // on the way past, so `FILE.STATS` sees what a consumer arriving now
        // would - a lapsed claim is not still in flight.
        let queue = self
            .is_table_queue_for_account(account, name)
            .then(|| self.queue_statistics(account, name).ok())
            .flatten();

        let loaded_table = self.get_table_read_only_for_account(account, name);
        let loaded = loaded_table.is_some();
        let record_count = match &loaded_table {
            Some(handle) => handle.read().records.len() as u64,
            None => self.file_record_count(&directory, account, name),
        };
        let dict_count = match &loaded_table {
            Some(handle) => handle.read().dictionary.len(),
            None => {
                let mut dictionary = HashMap::new();
                let _ = Self::load_section(&mut dictionary, &format!("{}/dict", file_dir));
                dictionary.len()
            }
        };

        let indexes = self.index_statistics(account, name).unwrap_or_default();
        let index_bytes = indexes.iter().map(|index| index.disk_bytes).sum();
        let modulus = meta.map(|m| m.modulus).unwrap_or(0);
        let per_group = self.records_per_group;
        let distribution = GroupDistribution::of(&groups, modulus);

        let capacity = modulus.saturating_mul(per_group as u64);
        let mut stats = FileStats {
            indexes,
            queue,
            account: account.to_string(),
            name: name.to_string(),
            record_count,
            dict_count,
            modulus,
            version: meta.map(|m| m.version).unwrap_or(0),
            group_count: group_sizes.len(),
            smallest_group_bytes: group_sizes.first().copied().unwrap_or(0),
            largest_group_bytes: group_sizes.last().copied().unwrap_or(0),
            disk_bytes,
            checksums: meta.map(|m| m.checksums).unwrap_or(false),
            legacy: meta.is_none(),
            durable,
            loaded,
            modified_seconds_ago: modified
                .and_then(|time| SystemTime::now().duration_since(time).ok())
                .map(|elapsed| elapsed.as_secs()),
            group_bytes: groups.iter().map(|group| group.bytes).sum(),
            index_bytes,
            records_per_group_target: per_group as u64,
            load_factor: if capacity == 0 {
                0.0
            } else {
                record_count as f64 / capacity as f64
            },
            records_until_growth: hashfile::records_until_growth(modulus, record_count, per_group),
            records_until_shrink: hashfile::records_until_shrink(modulus, record_count, per_group),
            largest_group_share: if record_count == 0 {
                0.0
            } else {
                distribution.max as f64 / record_count as f64
            },
            skew: if distribution.mean > 0.0 {
                distribution.max as f64 / distribution.mean
            } else {
                0.0
            },
            group_records: distribution,
            health: crate::db::health::Health::default(),
        };
        // Judged last, from the numbers above, so the verdicts and the values
        // they are about cannot be built apart and get out of step.
        stats.health = crate::db::health::file_health(&stats);
        Ok(stats)
    }

    /// A file's verdict without the measures behind it, from section metadata
    /// and index `state` files alone.
    ///
    /// What a *listing* can afford. `LIST.FILES` answers "which of these is
    /// worth opening", and answering it must not cost what opening one costs -
    /// so this reads no group trailer and no index section, and says only what
    /// `meta` and `state` already know: the format, the checksums, and whether
    /// an index has fallen behind the records. The full measures arrive with
    /// `FILE.STATS`.
    pub fn file_health_summary(&self, account: &str, name: &str) -> HealthSummary {
        let file_dir = self.file_dir(account, name);
        let meta = hashfile::read_meta(&format!("{}/data", file_dir));
        let mut summary = HealthSummary::good();
        match meta {
            None => summary.note(Verdict::Act, "legacy flat file"),
            Some(meta) if !meta.checksums => summary.note(Verdict::Act, "no per-group checksums"),
            Some(_) => {}
        }
        let data_version = meta.map(|meta| meta.version).unwrap_or(0);
        let fields = index::indexed_fields(&file_dir);
        let stale = fields
            .iter()
            .filter(|field| {
                index::read_state(&index::section_path(&file_dir, field))
                    .is_none_or(|state| state.data_version != data_version)
            })
            .count();
        if stale > 0 {
            summary.note(Verdict::Act, format!("{} of {} indexes stale", stale, fields.len()));
        }
        summary
    }

    /// Every file of an account with its cheap verdict, for the listing.
    pub fn file_health_for_account(&self, account: &str) -> Vec<(String, HealthSummary)> {
        self.list_tables_for_account(account)
            .into_iter()
            .map(|file| {
                let summary = self.file_health_summary(account, &file);
                (file, summary)
            })
            .collect()
    }

    /// Creates an index on a dictionary field of one file and writes it out.
    ///
    /// Building it is a single pass over the records, which is the one O(file)
    /// cost an index has; everything afterwards rides the ordinary write path.
    pub fn create_index_for_account(&self, account: &str, file: &str, field: &str) -> DbResult<IndexStats> {
        self.create_index_excluding(account, file, field, &[])
    }

    /// [`Database::create_index_for_account`], with values the index will not
    /// hold. See [`Database::set_index_exclusions`] for what that is for.
    pub fn create_index_excluding(
        &self,
        account: &str,
        file: &str,
        field: &str,
        exclude: &[String],
    ) -> DbResult<IndexStats> {
        let handle = self.get_table_mut_for_account(account, file)?;
        {
            let mut table = handle.write();
            if table.has_index(field.trim()) {
                return Err(DbError::IndexExists {
                    file: file.to_string(),
                    field: field.trim().to_string(),
                });
            }
            table.create_index_excluding(field, exclude.iter().cloned().collect())?;
        }
        // The file's lock is released first: a flush locks each dirty file in
        // turn, and would wait on the one this thread is holding.
        self.flush_table(&(account.to_string(), file.to_string()))?;
        self.index_statistics_for_field(account, file, field.trim())
    }

    /// Drops an index and removes its section from disk.
    pub fn drop_index_for_account(&self, account: &str, file: &str, field: &str) -> DbResult<()> {
        let field = field.trim();
        let handle = self.get_table_mut_for_account(account, file)?;
        let dropped = handle.write().drop_index(field);
        if !dropped
            && !index::indexed_fields(&self.file_dir(account, file))
                .iter()
                .any(|f| f == field)
        {
            return Err(DbError::IndexNotFound {
                file: file.to_string(),
                field: field.to_string(),
            });
        }
        index::remove_section(&index::section_path(&self.file_dir(account, file), field))?;
        Ok(())
    }

    /// Derives an index from the records again and writes it out.
    ///
    /// The repair for an index that is stale, and the way to bring one back
    /// after its section has been damaged or removed underneath the server.
    pub fn rebuild_index_for_account(&self, account: &str, file: &str, field: &str) -> DbResult<IndexStats> {
        let field = field.trim();
        let handle = self.get_table_mut_for_account(account, file)?;
        {
            let mut table = handle.write();
            if !table.has_index(field) {
                return Err(DbError::IndexNotFound {
                    file: file.to_string(),
                    field: field.to_string(),
                });
            }
            table.rebuild_index(field)?;
        }
        self.flush_table(&(account.to_string(), file.to_string()))?;
        self.index_statistics_for_field(account, file, field)
    }

    /// The file's directory on disk.
    fn file_dir(&self, account: &str, name: &str) -> String {
        format!("{}/{}", self.account_storage_dir(account), name)
    }

    /// Every index of one file, with the counts a management view reports.
    ///
    /// Read from the loaded table when the file is in memory, so an index that
    /// has been changed but not yet flushed is described as it now is. For a
    /// file that is not loaded the index sections are read instead - they hold
    /// values and keys rather than record bodies, so this is affordable where
    /// reading the file would not be.
    pub fn index_statistics(&self, account: &str, file: &str) -> DbResult<Vec<IndexStats>> {
        if !self.account_has_table(account, file) {
            self.scan_available_tables(account)?;
        }
        if !self.account_has_table(account, file) {
            return Err(DbError::FileNotFound {
                account: account.to_string(),
                file: file.to_string(),
            });
        }
        let file_dir = self.file_dir(account, file);
        let directory = self.account_storage_dir(account);
        // The file's record count is what every index verdict is read against,
        // so it is fetched once here rather than by each `stats` call.
        let records = self.file_record_count(&directory, account, file);
        if let Some(handle) = self.get_table_read_only_for_account(account, file) {
            let table = handle.read();
            return Ok(table
                .indexes
                .values()
                .map(|index| index.stats(&file_dir, true, records).in_file(file))
                .collect());
        }
        let data_version = hashfile::read_meta(&format!("{}/data", file_dir))
            .map(|meta| meta.version)
            .unwrap_or(0);
        Ok(index::indexed_fields(&file_dir)
            .into_iter()
            .map(|field| index::stats_from_disk(&file_dir, &field, data_version, records).in_file(file))
            .collect())
    }

    /// Every index in an account, with the file each belongs to.
    ///
    /// The view that comes to you. A database with forty files had no way to
    /// ask "which three indexes are worth my attention" short of opening forty
    /// pages, which is a question nobody asks and so a problem nobody finds.
    /// Sorted by file then field, so the same account always lists the same way.
    pub fn index_statistics_for_account(&self, account: &str) -> DbResult<Vec<(String, IndexStats)>> {
        if !self.list_accounts().iter().any(|name| name == account) {
            return Err(DbError::AccountNotFound(account.to_string()));
        }
        let mut out = Vec::new();
        for file in self.list_tables_for_account(account) {
            let Ok(indexes) = self.index_statistics(account, &file) else {
                continue;
            };
            for stats in indexes {
                out.push((file.clone(), stats));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.field.cmp(&b.1.field)));
        Ok(out)
    }

    /// One index in full: its statistics, its verdicts and the values that
    /// dominate it.
    ///
    /// The histogram is what turns "this index is skewed" into "`STATUS =
    /// ACTIVE` is 91% of it", which is the difference between a number and
    /// something to do about it.
    pub fn index_report(&self, account: &str, file: &str, field: &str, limit: usize) -> DbResult<IndexReport> {
        let field = field.trim();
        let limit = limit.clamp(1, crate::db::health::thresholds::HISTOGRAM_MAX);
        if !self.account_has_table(account, file) {
            self.scan_available_tables(account)?;
        }
        if !self.account_has_table(account, file) {
            return Err(DbError::FileNotFound {
                account: account.to_string(),
                file: file.to_string(),
            });
        }
        let file_dir = self.file_dir(account, file);
        let directory = self.account_storage_dir(account);
        let record_count = self.file_record_count(&directory, account, file);

        let (stats, top_values, values_available) =
            if let Some(handle) = self.get_table_read_only_for_account(account, file) {
                let table = handle.read();
                let index = table.indexes.get(field).ok_or_else(|| DbError::IndexNotFound {
                    file: file.to_string(),
                    field: field.to_string(),
                })?;
                // A stale index's postings do not describe the records, so
                // reporting its values would be reporting fiction.
                let available = !index.needs_rebuild;
                let values = if available { index.histogram(limit) } else { Vec::new() };
                (
                    index.stats(&file_dir, true, record_count).in_file(file),
                    values,
                    available,
                )
            } else {
                if !index::indexed_fields(&file_dir).iter().any(|f| f == field) {
                    return Err(DbError::IndexNotFound {
                        file: file.to_string(),
                        field: field.to_string(),
                    });
                }
                let data_version = hashfile::read_meta(&format!("{}/data", file_dir))
                    .map(|meta| meta.version)
                    .unwrap_or(0);
                let (stats, values, available) =
                    index::stats_and_values_from_disk(&file_dir, field, data_version, record_count, limit);
                (stats.in_file(file), values, available)
            };

        Ok(IndexReport {
            record_count,
            index: stats,
            top_values,
            values_available,
        })
    }

    /// Replaces the values one index deliberately does not hold.
    ///
    /// The remedy between "leave it" and "drop it". A field where 90% of
    /// records carry one value is excellent to index - for the other 10%;
    /// excluding the dominant value keeps everything the index is good at and
    /// stops paying for the entry that saves nothing.
    ///
    /// Changing the set rebuilds the index, exactly as moving its field to
    /// another attribute does: the index no longer holds what it says it holds.
    pub fn set_index_exclusions(
        &self,
        account: &str,
        file: &str,
        field: &str,
        values: &[String],
    ) -> DbResult<IndexStats> {
        let field = field.trim();
        let handle = self.get_table_mut_for_account(account, file)?;
        {
            let mut table = handle.write();
            let attr = table.field_index(field);
            let index = table.indexes.get_mut(field).ok_or_else(|| DbError::IndexNotFound {
                file: file.to_string(),
                field: field.to_string(),
            })?;
            let excluded: std::collections::BTreeSet<String> = values.iter().cloned().collect();
            if index.set_excluded(excluded) {
                // Rebuilt here rather than left for the load path, so the
                // command's own reply already describes the new index.
                let attr = attr.unwrap_or(index.attr);
                let records = std::mem::take(&mut table.records);
                if let Some(index) = table.indexes.get_mut(field) {
                    index.rebuild(&records, attr);
                }
                table.records = records;
            }
        }
        // The file's lock is released first: a flush locks each dirty file in
        // turn, and would wait on the one this thread is holding.
        self.flush_table(&(account.to_string(), file.to_string()))?;
        self.index_statistics_for_field(account, file, field)
    }

    /// The values one index holds, largest first - the histogram alone.
    pub fn index_values(&self, account: &str, file: &str, field: &str, limit: usize) -> DbResult<Vec<IndexValue>> {
        Ok(self.index_report(account, file, field, limit)?.top_values)
    }

    /// One index's statistics, by field name.
    fn index_statistics_for_field(&self, account: &str, file: &str, field: &str) -> DbResult<IndexStats> {
        self.index_statistics(account, file)?
            .into_iter()
            .find(|stats| stats.field == field)
            .ok_or_else(|| DbError::IndexNotFound {
                file: file.to_string(),
                field: field.to_string(),
            })
    }

    pub fn is_table_available(&self, name: &str) -> bool {
        self.account_has_table(&self.current_account(), name)
    }

    pub fn is_table_loaded(&self, name: &str) -> bool {
        rlock(&self.tables).contains_key(&(self.current_account(), name.to_string()))
    }

    pub fn create_table(&self, name: &str) -> DbResult<()> {
        self.create_table_for_account(&self.current_account(), name)
    }

    /// Creates a file and marks it durable, so every write to it is flushed
    /// before being acknowledged regardless of the global buffering settings.
    pub fn create_table_durable(&self, name: &str, durable: bool) -> DbResult<()> {
        self.create_table_for_account_durable(&self.current_account(), name, durable)
    }

    pub fn create_table_for_account_durable(&self, account: &str, name: &str, durable: bool) -> DbResult<()> {
        self.create_table_with(account, name, FileAttributes { durable, queue: None })
    }

    /// Creates a file with the `DIR` attributes it is to carry.
    ///
    /// The attributes are written after the file exists but before anything can
    /// be put in it, so a queue is a queue from its first record rather than
    /// from whenever its flag caught up. Attributes that say nothing - an
    /// ordinary buffered file - are not written at all, which leaves the entry
    /// `sync_dir_file_for_account` already makes.
    pub fn create_table_with(&self, account: &str, name: &str, attributes: FileAttributes) -> DbResult<()> {
        self.create_table_for_account(account, name)?;
        if attributes != FileAttributes::default() {
            self.set_file_attributes_for_account(account, name, attributes, "attributes")?;
        }
        Ok(())
    }

    pub fn create_table_for_account(&self, account: &str, name: &str) -> DbResult<()> {
        if account.is_empty() {
            return Err(DbError::NoAccount);
        }
        self.ensure_available_tables(account)?;

        let storage = self.account_storage_dir(account);
        let table_dir = format!("{}/{}", storage, name);
        if !Path::new(&table_dir).exists() {
            fs::create_dir_all(&table_dir)?;
        }
        Self::init_data_section(&table_dir, self.records_per_group)?;
        File::create(format!("{}/dict", table_dir))?;

        let has_dir = {
            let mut listings = wlock(&self.available_tables);
            let available = listings.entry(account.to_string()).or_default();
            if available.contains(name) {
                return Err(DbError::FileExists {
                    account: account.to_string(),
                    file: name.to_string(),
                });
            }
            available.insert(name.to_string());
            available.contains("DIR")
        };

        // DIR is the account's own listing of its files, so a file created in
        // an account that has not got one brings it into being rather than
        // going unlisted - which is what a client creating an account and then
        // files over the protocol used to end up with, since nothing outside
        // the CLI's login prompt ever made one. Creating DIR itself must not
        // come back through here, which the name check takes care of, and
        // `ensure_dir_file_for_account` writes the listing when it creates one.
        if name != "DIR" {
            let _ = if has_dir {
                self.sync_dir_file_for_account(account)
            } else {
                self.ensure_dir_file_for_account(account)
            };
        }

        // Set default dictionary for SYSTEM files
        if account == "SYSTEM" && (name.starts_with('$') || name == "DIR") {
            let _ = self.ensure_default_dictionaries(name);
        } else if name == "DIR" {
            // Every account's DIR describes the same two attributes, so name them
            // here too rather than only for SYSTEM.
            if let Ok(dir_table) = self.get_table_mut_for_account(account, "DIR") {
                Self::ensure_dir_dictionary(&mut dir_table.write());
            }
        }

        Ok(())
    }

    pub fn delete_table(&self, name: &str) -> DbResult<()> {
        self.delete_table_for_account(&self.current_account(), name)
    }

    pub fn delete_table_for_account(&self, account: &str, name: &str) -> DbResult<()> {
        if account.is_empty() {
            return Err(DbError::NoAccount);
        }
        if !self.account_has_table(account, name) {
            return Err(DbError::FileNotFound {
                account: account.to_string(),
                file: name.to_string(),
            });
        }

        let key = (account.to_string(), name.to_string());
        wlock(&self.tables).remove(&key);
        wlock(&self.file_attributes).remove(&key);
        if let Some(available) = wlock(&self.available_tables).get_mut(account) {
            available.remove(name);
        }
        self.forget_lru(&key);

        let storage = self.account_storage_dir(account);
        let table_dir = format!("{}/{}", storage, name);
        let _ = fs::remove_dir_all(table_dir);

        Ok(())
    }

    pub fn sync_dir_file(&self) -> DbResult<()> {
        self.sync_dir_file_for_account(&self.current_account())
    }

    pub fn sync_dir_file_for_account(&self, account: &str) -> DbResult<()> {
        let tables = self.list_tables_for_account(account);
        let handle = self.get_table_mut_for_account(account, "DIR")?;
        let mut dir_table = handle.write();
        // The listing is rebuilt from the filesystem, but nothing on the
        // filesystem says whether a file is durable or a queue, so every
        // attribute of the old entry is carried over to the new one. A rebuild
        // that dropped them would silently demote a queue to an ordinary file
        // holding some oddly named records.
        let attributes: HashMap<String, FileAttributes> = dir_table
            .records
            .iter()
            .map(|(k, r)| (k.clone(), FileAttributes::of(r)))
            .collect();
        dir_table.records.clear();
        for t in tables {
            if t != "DIR" {
                let carried = attributes.get(&t).copied().unwrap_or_default();
                dir_table.records.insert(t, carried.to_record());
            }
        }
        // The listing is rebuilt from scratch, so every group has to be rewritten.
        dir_table.touch_all();
        Ok(())
    }

    fn ensure_dir_dictionary(dir_table: &mut Table) {
        // Attribute number, heading, justification and width, in the order
        // `DIR`'s attributes are defined in `models.rs`.
        const ENTRIES: [(&str, &str); 5] = [
            ("TYPE", "1^TYPE^L^1"),
            ("DURABLE", "2^DURABLE^L^7"),
            ("QUEUE", "3^QUEUE^L^5"),
            ("QUEUE.TIMEOUT", "4^TIMEOUT^R^7"),
            ("QUEUE.RETRIES", "5^RETRIES^R^7"),
        ];
        for (name, definition) in ENTRIES {
            if !dir_table.dictionary.contains_key(name) {
                dir_table
                    .dictionary
                    .insert(name.to_string(), Record::from_display_string(definition));
                dir_table.mark_dict_dirty();
            }
        }
    }

    pub fn set_table_durable(&self, name: &str, durable: bool) -> DbResult<()> {
        self.set_table_durable_for_account(&self.current_account(), name, durable)
    }

    /// Records the per-file durability flag in the account's DIR file, which is
    /// how an existing file is promoted to durable or demoted back without
    /// being recreated.
    ///
    /// The flag is written before the flush rather than after it, so the flush
    /// this ends with is already the durable one: anything the file had
    /// buffered reaches the disk under the durability being turned on, and the
    /// flag never gets ahead of the data it promises to protect. Demoting is
    /// safe either way - it only ever relaxes what a later write has to do.
    pub fn set_table_durable_for_account(&self, account: &str, name: &str, durable: bool) -> DbResult<()> {
        let mut attributes = self.file_attributes_for_account(account, name);
        attributes.durable = durable;
        self.set_file_attributes_for_account(account, name, attributes, "durability")
    }

    /// Replaces one file's `DIR` attributes wholesale: durability, whether it
    /// is a queue, and that queue's claim policy.
    ///
    /// The caller reads the current attributes with
    /// [`file_attributes_for_account`](Self::file_attributes_for_account) and
    /// hands back the whole set, so nothing is changed by omission.
    pub fn set_file_attributes(&self, account: &str, name: &str, attributes: FileAttributes) -> DbResult<()> {
        self.set_file_attributes_for_account(account, name, attributes, "attributes")
    }

    /// Writes one file's `DIR` entry and refreshes the cache the write path
    /// reads it from.
    ///
    /// `what` names the thing being changed, for the refusal on `DIR` itself.
    /// The entry is written before the flush this ends with, so the flush is
    /// already the one the new attributes ask for: anything the file had
    /// buffered reaches the disk under the durability being turned on, and the
    /// flag never gets ahead of the data it promises to protect. Relaxing an
    /// attribute is safe either way - it only ever loosens what a later write
    /// has to do.
    pub(crate) fn set_file_attributes_for_account(
        &self,
        account: &str,
        name: &str,
        attributes: FileAttributes,
        what: &str,
    ) -> DbResult<()> {
        if account.is_empty() {
            return Err(DbError::NoAccount);
        }
        if !self.account_has_table(account, name) {
            return Err(DbError::FileNotFound {
                account: account.to_string(),
                file: name.to_string(),
            });
        }
        if name == "DIR" {
            // DIR holds the attributes; it is not one of the files they
            // describe, and an entry for itself would be dropped the next time
            // the listing is rebuilt. Recording a promise nothing honours is
            // worse than saying no.
            return Err(DbError::InvalidRequest(format!(
                "The DIR file's {} is not settable: it carries the other files' attributes rather than one of its own",
                what
            )));
        }
        // The attributes live in DIR, so an account without one gets it now
        // rather than silently losing what was asked for.
        self.ensure_dir_file_for_account(account)?;
        {
            let handle = self.get_table_mut_for_account(account, "DIR")?;
            let mut dir_table = handle.write();
            dir_table.insert_record(name, attributes.to_record());
            Self::ensure_dir_dictionary(&mut dir_table);
        }
        wlock(&self.file_attributes).insert((account.to_string(), name.to_string()), attributes);
        // A file that has just become - or stopped being - a queue has to pick
        // up or drop its in-memory ordering before the next command reaches it.
        self.reattach_queue(account, name, attributes.queue.is_some())?;
        self.save()
    }

    pub fn is_table_durable(&self, name: &str) -> bool {
        self.is_table_durable_for_account(&self.current_account(), name)
    }

    /// True when this file must be flushed on every write, either because the
    /// whole database runs in durable mode or because its DIR entry says so.
    pub fn is_table_durable_for_account(&self, account: &str, name: &str) -> bool {
        self.durable_writes || self.file_attributes_for_account(account, name).durable
    }

    /// What the account's `DIR` says about one of its files, from the cache
    /// when it can be and from the `DIR` file itself the first time.
    ///
    /// `DIR` describes the other files rather than itself, so asking about
    /// `DIR` answers with the defaults instead of reading its own entry - which
    /// is also what stops loading `DIR` from having to load `DIR`.
    pub fn file_attributes_for_account(&self, account: &str, name: &str) -> FileAttributes {
        let key = (account.to_string(), name.to_string());
        if let Some(attributes) = rlock(&self.file_attributes).get(&key) {
            return *attributes;
        }
        let has_dir = name != "DIR" && self.account_has_table(account, "DIR");
        let attributes = if has_dir {
            match self.get_table_mut_for_account(account, "DIR") {
                Ok(dir) => dir.read().records.get(name).map(FileAttributes::of).unwrap_or_default(),
                Err(_) => FileAttributes::default(),
            }
        } else {
            FileAttributes::default()
        };
        wlock(&self.file_attributes).insert(key, attributes);
        attributes
    }

    /// Creates the account's DIR file if it does not have one yet.
    pub fn ensure_dir_file_for_account(&self, account: &str) -> DbResult<()> {
        if self.account_has_table(account, "DIR") {
            return Ok(());
        }
        self.create_table_for_account(account, "DIR")?;
        self.sync_dir_file_for_account(account)
    }

    pub fn ensure_dir_file(&self) -> DbResult<bool> {
        Ok(self.account_has_table(&self.current_account(), "DIR"))
    }

    pub fn create_dir_file(&self) -> DbResult<()> {
        self.create_table("DIR")?;
        self.sync_dir_file()
    }

    pub fn get_account_for_dir(&self, dir: &str) -> Option<String> {
        let config = rlock(&self.accounts_config);
        let names_field = config.fields.first()?;
        let dirs_field = config.fields.get(1)?;
        for (i, v) in dirs_field.values.iter().enumerate() {
            if v.first_bytes() == dir.as_bytes() {
                return names_field.values.get(i)?.first_text().map(|name| name.to_string());
            }
        }
        None
    }

    pub fn get_conversion_code_read_only(&self, table_name: &str, field_name: &str) -> Option<String> {
        self.get_conversion_code_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_conversion_code_read_only_for_account(
        &self,
        account: &str,
        table_name: &str,
        field_name: &str,
    ) -> Option<String> {
        self.get_table_read_only_for_account(account, table_name)?
            .read()
            .conversion_code(field_name)
    }

    pub fn get_conversion_code(&self, table_name: &str, field_name: &str) -> Option<String> {
        self.get_conversion_code_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_header_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_header_in(&handle.read(), field_name),
            None => field_name.to_string(),
        }
    }

    /// The column heading of a field, read from a table the caller has already
    /// resolved.
    ///
    /// The `_in` variants exist for a caller that is holding the table's lock
    /// already - a report renderer walking every column of every row. Going
    /// back through the database for each one would take that same lock again,
    /// which is both wasteful and, with a writer waiting in between, a way to
    /// deadlock against ourselves.
    pub fn field_header_in(table: &Table, field_name: &str) -> String {
        if field_name == "ID" {
            return "ID".to_string();
        }
        if let Some(rec) = table.dictionary.get(field_name)
            && let Some(f2) = rec.fields.get(DICT_NAME_IDX)
            && let Some(v1) = f2.values.first()
            && let Some(header) = v1.first_text()
            && !header.is_empty()
        {
            return header.to_string();
        }
        field_name.to_string()
    }

    pub fn get_field_width_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> usize {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_width_in(&handle.read(), field_name),
            None => DEFAULT_FIELD_WIDTH,
        }
    }

    /// The display width of a field. See [`field_header_in`](Self::field_header_in).
    pub fn field_width_in(table: &Table, field_name: &str) -> usize {
        if field_name == "ID" {
            return DEFAULT_FIELD_WIDTH;
        }
        if let Some(rec) = table.dictionary.get(field_name)
            && let Some(f4) = rec.fields.get(DICT_WIDTH_IDX)
            && let Some(v1) = f4.values.first()
            && let Some(width_str) = v1.first_text()
            && let Ok(width) = width_str.parse::<usize>()
        {
            return width;
        }
        DEFAULT_FIELD_WIDTH
    }

    pub fn get_field_justification_read_only_for_account(
        &self,
        account: &str,
        table_name: &str,
        field_name: &str,
    ) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_justification_in(&handle.read(), field_name),
            None => "L".to_string(),
        }
    }

    /// The justification of a field. See [`field_header_in`](Self::field_header_in).
    pub fn field_justification_in(table: &Table, field_name: &str) -> String {
        if field_name == "ID" {
            return "L".to_string();
        }
        if let Some(rec) = table.dictionary.get(field_name)
            && let Some(f3) = rec.fields.get(DICT_JUSTIFY_IDX)
            && let Some(v1) = f3.values.first()
            && let Some(just) = v1.first_text()
            && !just.is_empty()
        {
            return just.to_string();
        }
        "L".to_string()
    }

    pub fn get_all_dict_fields_read_only_for_account(&self, account: &str, table_name: &str) -> Vec<String> {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::all_dict_fields_in(&handle.read()),
            None => Vec::new(),
        }
    }

    /// Every dictionary field, in attribute order. See
    /// [`field_header_in`](Self::field_header_in).
    pub fn all_dict_fields_in(table: &Table) -> Vec<String> {
        let mut fields_map: HashMap<usize, String> = HashMap::new();
        let mut keys: Vec<_> = table.dictionary.keys().cloned().collect();
        keys.sort(); // Consistent order for "picking the first"

        for key in keys {
            if let Some(record) = table.dictionary.get(&key)
                && let Some(field_idx_str) = record
                    .fields
                    .get(DICT_FIELD_IDX)
                    .and_then(|f| f.values.first())
                    .and_then(|v| v.first_text())
                && let Ok(idx) = field_idx_str.parse::<usize>()
                && idx > 0
                && !fields_map.contains_key(&idx)
            {
                fields_map.insert(idx, key);
            }
        }

        let mut sorted_indices: Vec<_> = fields_map.keys().cloned().collect();
        sorted_indices.sort();

        sorted_indices
            .into_iter()
            .map(|idx| fields_map.get(&idx).unwrap().clone())
            .collect()
    }

    pub fn apply_conversion(val: &str, code: &str) -> String {
        if code.starts_with("MD")
            && code.len() > 2
            && let Ok(decimals) = code[2..].parse::<usize>()
        {
            let divisor = 10f64.powi(decimals as i32);
            if let Ok(num) = val.parse::<i64>() {
                let mut s = format!("{:.width$}", num as f64 / divisor, width = decimals);
                if decimals == 0 {
                    s = format!("{}", num);
                }
                return s;
            } else if let Ok(f) = val.parse::<f64>() {
                // Robustness: handle cases where data might already be stored with a decimal point
                let mut s = format!("{:.width$}", f / divisor, width = decimals);
                if decimals == 0 {
                    s = format!("{}", f.round() as i64);
                }
                return s;
            }
        }
        val.to_string()
    }

    pub fn apply_iconv(val: &str, code: &str) -> String {
        if code.starts_with("MD")
            && code.len() > 2
            && let Ok(decimals) = code[2..].parse::<usize>()
            && let Ok(f) = val.parse::<f64>()
        {
            let multiplier = 10f64.powi(decimals as i32);
            return format!("{:.0}", (f * multiplier).round());
        }
        val.to_string()
    }

    /// Applies an output conversion to each value and sub-value of a field
    /// rather than to the whole field.
    ///
    /// The field's display string joins its values with `]` and sub-values with
    /// `\\`, so handing that to [`apply_conversion`](Self::apply_conversion)
    /// gives it something like `"200]300"`, which parses as no number at all
    /// and comes back unconverted. Splitting on the marks first means an `MD2`
    /// column of a multivalued field converts the way a single-valued one does.
    fn convert_display_string(raw: &str, code: &str) -> String {
        if !raw.contains([']', '\\']) {
            return Self::apply_conversion(raw, code);
        }
        raw.split(']')
            .map(|value| {
                value
                    .split('\\')
                    .map(|sub| Self::apply_conversion(sub, code))
                    .collect::<Vec<_>>()
                    .join("\\")
            })
            .collect::<Vec<_>>()
            .join("]")
    }

    pub fn format_record_field(&self, table_name: &str, record: &Record, field_name: &str) -> String {
        self.format_record_field_for_account(&self.current_account(), table_name, record, field_name)
    }

    pub fn format_record_field_for_account(
        &self,
        account: &str,
        table_name: &str,
        record: &Record,
        field_name: &str,
    ) -> String {
        self.format_record_field_at_for_account(account, table_name, record, field_name, None)
    }

    /// Renders one column of one output row. `position` is the row's exploded
    /// position, so an exploded column shows only the value (or sub-value) that
    /// put the row there; `None` renders the whole field, which is what every
    /// unexploded row does.
    pub fn format_record_field_at(
        &self,
        table_name: &str,
        record: &Record,
        field_name: &str,
        position: Option<ValuePosition>,
    ) -> String {
        self.format_record_field_at_for_account(&self.current_account(), table_name, record, field_name, position)
    }

    pub fn format_record_field_at_for_account(
        &self,
        account: &str,
        table_name: &str,
        record: &Record,
        field_name: &str,
        position: Option<ValuePosition>,
    ) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::format_record_field_at_in(&handle.read(), record, field_name, position),
            None => String::new(),
        }
    }

    /// Renders one column of one row from a table the caller has already
    /// resolved, in a single dictionary lookup. See
    /// [`field_header_in`](Self::field_header_in).
    pub fn format_record_field_at_in(
        table: &Table,
        record: &Record,
        field_name: &str,
        position: Option<ValuePosition>,
    ) -> String {
        let (field_idx, conv) = match table.field_index_and_conversion(field_name) {
            Some(resolved) => resolved,
            None => return String::new(),
        };

        let raw_val = record.get_value_display_string(field_idx, position);
        match conv {
            Some(code) => Self::convert_display_string(&raw_val, &code),
            None => raw_val,
        }
    }

    pub fn get_field_index_read_only(&self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_index_read_only_for_account(
        &self,
        account: &str,
        table_name: &str,
        field_name: &str,
    ) -> Option<usize> {
        if field_name == "ID" {
            return Some(0);
        }
        self.get_table_read_only_for_account(account, table_name)?
            .read()
            .field_index(field_name)
    }

    pub fn get_field_index(&self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_index_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" {
            return Some(0);
        }
        let _ = self.get_table_mut_for_account(account, table_name).ok();
        self.get_field_index_read_only_for_account(account, table_name, field_name)
    }

    pub fn serialize_record(&self, table_name: &str, record: &Record) -> serde_json::Value {
        self.serialize_record_for_account(&self.current_account(), table_name, record)
    }

    pub fn serialize_record_for_account(&self, account: &str, table_name: &str, record: &Record) -> serde_json::Value {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => self.serialize_record_in(&handle.read(), record),
            None => serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Resolves a table's dictionary into the fields that serialization actually
    /// emits: the attribute index, the camelCase output key, and the MDn
    /// conversion code. Built once per result set and reused for every record,
    /// this turns the O(records * dict) index parsing and camel-casing in
    /// [`serialize_record_in`](Self::serialize_record_in) into O(dict).
    pub fn record_schema(&self, table: &Table) -> RecordSchema {
        let mut fields = Vec::with_capacity(table.dictionary.len());
        for (dict_key, dict_rec) in &table.dictionary {
            let idx = dict_rec
                .fields
                .get(DICT_FIELD_IDX)
                .and_then(|f| f.values.first())
                .and_then(|v| v.first_text())
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|idx| *idx > 0);
            let Some(idx) = idx else { continue };
            fields.push(RecordSchemaField {
                // Pick attribute 1 is 0-indexed 0 in our internal fields vector
                field_idx: idx - 1,
                camel_key: self.to_camel_case(dict_key),
                conversion: Table::conversion_code_from_dict_record(dict_rec).map(str::to_string),
            });
        }
        RecordSchema { fields }
    }

    /// Serializes `record` against a schema resolved earlier by
    /// [`record_schema`](Self::record_schema), so a whole result set shares one
    /// pass over the dictionary.
    pub fn serialize_record_with_schema(&self, schema: &RecordSchema, record: &Record) -> serde_json::Value {
        let mut map = serde_json::Map::with_capacity(schema.fields.len());
        for field in &schema.fields {
            map.insert(
                field.camel_key.clone(),
                Self::serialize_field(record.fields.get(field.field_idx), field.conversion.as_deref()),
            );
        }
        serde_json::Value::Object(map)
    }

    /// One field as JSON.
    ///
    /// A field holding a single value with a single sub-value - by far the
    /// common case - is a string, so existing clients see no change. Anything
    /// with real multivalue structure becomes an array of values, and a value
    /// with sub-values becomes a nested array. Emitting `"TEST]PAYROLL"` for
    /// those instead would be ambiguous with a value that genuinely contains a
    /// `]`, which is what made a read/modify/write round trip lossy.
    fn serialize_field(field: Option<&Field>, conversion: Option<&str>) -> serde_json::Value {
        let convert = |s: &str| match conversion {
            Some(code) => Self::apply_conversion(s, code),
            None => s.to_string(),
        };
        let Some(field) = field else {
            return serde_json::Value::String(String::new());
        };

        // A sub-value that is text is a JSON string, exactly as it always was.
        // One that is not cannot be: JSON strings are UTF-8, and forcing it
        // through would corrupt it silently, which is the bug this exists to
        // stop. It travels tagged instead, and a conversion is not applied -
        // there is no conversion of a byte string.
        let emit = |sub: &[u8]| match std::str::from_utf8(sub) {
            Ok(text) => serde_json::Value::String(convert(text)),
            Err(_) => Self::binary_json(sub),
        };

        match field.values.as_slice() {
            [] => return serde_json::Value::String(String::new()),
            [only] if only.sub_values.len() <= 1 => {
                return emit(only.first_bytes());
            }
            _ => {}
        }

        serde_json::Value::Array(
            field
                .values
                .iter()
                .map(|value| match value.sub_values.as_slice() {
                    [] => serde_json::Value::String(String::new()),
                    [only] => emit(only),
                    subs => serde_json::Value::Array(subs.iter().map(|sub| emit(sub)).collect()),
                })
                .collect(),
        )
    }

    /// One JSON scalar as the text a record stores for it.
    fn scalar_text(val: &serde_json::Value, conversion: Option<&str>) -> String {
        let text = match val {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => {
                if *b {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            }
            other => other.to_string(),
        };
        match conversion {
            Some(code) => Self::apply_iconv(&text, code),
            None => text,
        }
    }

    /// The envelope a sub-value that is not valid UTF-8 travels in.
    fn binary_json(sub: &[u8]) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        object.insert(
            BINARY_JSON_KEY.to_string(),
            serde_json::Value::String(crate::db::base64::encode(sub)),
        );
        serde_json::Value::Object(object)
    }

    /// Whether the caller meant this to be bytes.
    ///
    /// Separate from decoding it, so that a malformed payload is told apart
    /// from a value that was never binary in the first place.
    fn is_binary_json(val: &serde_json::Value) -> bool {
        val.as_object()
            .is_some_and(|object| object.contains_key(BINARY_JSON_KEY))
    }

    /// The bytes inside a `{"$base64": "..."}` envelope, or `None` when the
    /// payload is absent, not a string, or not valid base64.
    fn binary_from_json(val: &serde_json::Value) -> Option<Vec<u8>> {
        let encoded = val.as_object()?.get(BINARY_JSON_KEY)?.as_str()?;
        crate::db::base64::decode(encoded)
    }

    /// Serializes `record` using `table`, which the caller has already
    /// resolved. Spares the caller a second table lookup per record. Callers
    /// serializing an entire result set should instead resolve a
    /// [`RecordSchema`] once and call
    /// [`serialize_record_with_schema`](Self::serialize_record_with_schema).
    pub fn serialize_record_in(&self, table: &Table, record: &Record) -> serde_json::Value {
        self.serialize_record_with_schema(&self.record_schema(table), record)
    }

    /// The mirror of [`serialize_field`](Self::serialize_field): an array
    /// becomes values, a nested array becomes sub-values, and a scalar stays a
    /// single value.
    ///
    /// A plain string is deliberately *not* re-split on `]`, so a client that
    /// genuinely means to store that character still can.
    ///
    /// `None` when the caller sent a binary envelope that does not decode. That
    /// is a refusal rather than a fallback on purpose: the alternative is
    /// storing the envelope's *own JSON text* as the value, which is a write
    /// that succeeds and reads back as something the client never sent - the
    /// exact failure this codec exists to end.
    fn deserialize_field(val: &serde_json::Value, conversion: Option<&str>) -> Option<Vec<Value>> {
        let scalar = |v: &serde_json::Value| -> Option<SubValue> {
            // The tagged envelope first: it is the only shape that can carry
            // bytes, and a client that sent one meant exactly those bytes.
            if Self::is_binary_json(v) {
                return Self::binary_from_json(v);
            }
            Some(Self::scalar_text(v, conversion).into_bytes())
        };
        let values = match val {
            serde_json::Value::Array(values) => values
                .iter()
                .map(|value| match value {
                    serde_json::Value::Array(subs) => Some(Value {
                        sub_values: subs.iter().map(&scalar).collect::<Option<Vec<_>>>()?,
                    }),
                    other => Some(Value {
                        sub_values: vec![scalar(other)?],
                    }),
                })
                .collect::<Option<Vec<_>>>()?,
            other => vec![Value {
                sub_values: vec![scalar(other)?],
            }],
        };
        Some(values)
    }

    pub fn deserialize_record(&self, table_name: &str, data: &serde_json::Value) -> Option<Record> {
        self.deserialize_record_for_account(&self.current_account(), table_name, data)
    }

    pub fn deserialize_record_for_account(
        &self,
        account: &str,
        table_name: &str,
        data: &serde_json::Value,
    ) -> Option<Record> {
        let handle = self.get_table_read_only_for_account(account, table_name)?;
        let table = handle.read();
        self.deserialize_record_in(&table, data)
    }

    /// Same, from a file the caller has already resolved.
    ///
    /// The write path holds the handle already, and on a file several
    /// connections are writing at once, looking it up again is not free: every
    /// resolution takes that contended lock once more, on top of the `stat`
    /// calls of the freshness check.
    pub fn deserialize_record_in(&self, table: &Table, data: &serde_json::Value) -> Option<Record> {
        let obj = data.as_object()?;
        let mut record = Record::new();

        // Inverse mapping of camelCase or original dictionary keys to attribute indices and conversion codes
        let mut attr_map = HashMap::new();
        let mut conv_map = HashMap::new();
        for (dict_key, dict_rec) in &table.dictionary {
            if let Some(f1) = dict_rec.fields.get(DICT_FIELD_IDX)
                && let Some(v1) = f1.values.first()
                && let Some(idx_str) = v1.first_text()
                && let Ok(idx) = idx_str.parse::<usize>()
                && idx > 0
            {
                let attr_idx = idx - 1;
                let camel_key = self.to_camel_case(dict_key);
                attr_map.insert(camel_key.clone(), attr_idx);
                attr_map.insert(dict_key.clone(), attr_idx);

                if let Some(code) = table.conversion_code(dict_key) {
                    conv_map.insert(camel_key, code.clone());
                    conv_map.insert(dict_key.clone(), code);
                }
            }
        }

        for (key, val) in obj {
            if let Some(&idx) = attr_map.get(key) {
                while record.fields.len() <= idx {
                    record.fields.push(Field::default());
                }
                record.fields[idx].values = Self::deserialize_field(val, conv_map.get(key).map(String::as_str))?;
            }
        }

        Some(record)
    }

    fn to_camel_case(&self, s: &str) -> String {
        let mut res = String::new();
        let mut capitalize_next = false;
        for c in s.chars() {
            if c == '.' {
                capitalize_next = true;
            } else if capitalize_next {
                res.push(c.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                res.push(c.to_ascii_lowercase());
            }
        }
        res
    }

    pub fn log_error(&self, account: &str, message: &str) -> DbResult<()> {
        self.run_in_system_account(|db| {
            let now = time::OffsetDateTime::now_utc();
            let date_str = format!("{:04}{:02}{:02}", now.year(), now.month() as u8, now.day());
            let time_str = format!("{:02}{:02}{:02}", now.hour(), now.minute(), now.second());
            // Add a microsecond component to ensure key uniqueness during fast tests
            let key = format!("{}*{}*{}*{}", date_str, time_str, now.microsecond(), account);

            let mut record = Record::new();
            while record.fields.len() <= SYS_LOGS_DETAIL_IDX {
                record.fields.push(Field::default());
            }

            // Field 1: Message
            record.fields[SYS_LOGS_MESSAGE_IDX].values.push(Value::text(message));

            // Field 2: Detail
            if db.log_detail == "detailed" {
                record.fields[SYS_LOGS_DETAIL_IDX]
                    .values
                    .push(Value::text(format!("UTC: {}", now)));
            }

            let max_records = db.max_log_records;
            {
                let handle = db.get_table_mut("$LOGS")?;
                let mut table = handle.write();
                table.records.insert(key, record);
                // Trimming below removes arbitrary keys, so this is a bulk change.
                table.touch_all();

                if table.records.len() > max_records {
                    let mut keys: Vec<_> = table.records.keys().cloned().collect();
                    keys.sort();
                    while keys.len() > max_records {
                        let oldest = keys.remove(0);
                        table.records.remove(&oldest);
                    }
                }
            }
            db.save()
        })
    }

    pub fn add_authorized_client(
        &self,
        name: &str,
        thumbprint: &str,
        allowed_accounts: Vec<String>,
        is_admin: bool,
    ) -> DbResult<()> {
        self.run_in_system_account(|db| {
            let thumbprint_lower = thumbprint.to_lowercase();

            // Update $CLIENTS table
            {
                let handle = db.get_table_mut("$CLIENTS")?;
                let mut table = handle.write();
                let mut record = Record::new();
                while record.fields.len() <= SYS_CLIENTS_ADMIN_IDX {
                    record.fields.push(Field::default());
                }
                // Field 0: Thumbprint
                record.fields[SYS_CLIENTS_THUMBPRINT_IDX]
                    .values
                    .push(Value::text(&thumbprint_lower));
                // Field 1: Allowed Accounts
                for acc in &allowed_accounts {
                    record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.push(Value::text(acc));
                }
                // Field 2: Admin flag
                record.fields[SYS_CLIENTS_ADMIN_IDX]
                    .values
                    .push(Value::text(if is_admin { "Y" } else { "" }));

                table.insert_record(name, record);
            }
            db.save()?;

            // Update in-memory structures
            db.load_clients_from_table()?;

            // Sync with certs.reg for backward compatibility (optional but safe)
            db.save_certs()
        })
    }

    pub fn add_client_account(&self, name: &str, account: &str) -> DbResult<bool> {
        self.run_in_system_account(|db| {
            let mut success = false;
            {
                let handle = db.get_table_mut("$CLIENTS")?;
                let mut table = handle.write();
                if let Some(record) = table.records.get_mut(name) {
                    // Ensure mandatory fields exist
                    while record.fields.len() <= SYS_CLIENTS_ACCOUNTS_IDX {
                        record.fields.push(Field::default());
                    }

                    // Check if account already exists in Field 1
                    let already_exists = record.fields[SYS_CLIENTS_ACCOUNTS_IDX]
                        .values
                        .iter()
                        .any(|v| v.first_bytes() == account.as_bytes());

                    if !already_exists {
                        record.fields[SYS_CLIENTS_ACCOUNTS_IDX]
                            .values
                            .push(Value::text(account));
                        table.mark_dirty(name);
                        success = true;
                    }
                }
            }

            if success {
                db.save()?;
                db.load_clients_from_table()?;
            }

            Ok(success)
        })
    }

    pub fn remove_client_account(&self, name: &str, account: &str) -> DbResult<bool> {
        self.run_in_system_account(|db| {
            let mut success = false;
            {
                let handle = db.get_table_mut("$CLIENTS")?;
                let mut table = handle.write();
                if let Some(record) = table.records.get_mut(name)
                    && record.fields.len() > SYS_CLIENTS_ACCOUNTS_IDX
                {
                    let original_len = record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.len();
                    record.fields[SYS_CLIENTS_ACCOUNTS_IDX]
                        .values
                        .retain(|v| v.first_bytes() != account.as_bytes());

                    if record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.len() < original_len {
                        table.mark_dirty(name);
                        success = true;
                    }
                }
            }

            if success {
                db.save()?;
                db.load_clients_from_table()?;
            }
            Ok(success)
        })
    }

    pub fn remove_authorized_client(&self, name: &str) -> DbResult<bool> {
        self.run_in_system_account(|db| {
            let found = db.get_table_mut("$CLIENTS")?.write().remove_record(name).is_some();

            if found {
                db.save()?;
                db.load_clients_from_table()?;
                let _ = db.save_certs();
            }
            Ok(found)
        })
    }

    pub fn save_certs(&self) -> DbResult<()> {
        let mut certs_rec = Record::new();
        certs_rec.fields.push(Field::default());
        for tp in rlock(&self.clients).certs.iter() {
            certs_rec.fields[0].values.push(Value::text(tp));
        }
        let mut map = HashMap::new();
        map.insert("certs".to_string(), certs_rec);
        Self::save_section(&map, &format!("{}/certs.reg", self.storage_dir))?;
        Ok(())
    }

    pub fn create_test_account(&self, name: &str) -> DbResult<()> {
        let original_account = self.current_account();
        self.create_account(name, None)?;
        self.logto(name)?;
        // DIR comes with the account now; creating it again would be refused.
        self.create_table("USERS")?;
        self.create_table("PRODUCTS")?;
        self.sync_dir_file()?;
        {
            let handle = self.get_table_mut("USERS")?;
            let mut table = handle.write();
            table
                .dictionary
                .insert("NAME".to_string(), Record::from_display_string("1^NAME^L^15"));
            table
                .dictionary
                .insert("EMAIL".to_string(), Record::from_display_string("2^EMAIL^L^20"));
            // ROLES is multivalued, and Jane's second role is sub-valued, so the
            // fixture exercises every level of the hierarchy rather than only
            // the flat one.
            table
                .dictionary
                .insert("ROLES".to_string(), Record::from_display_string("3^ROLES^L^20"));
            table.records.insert(
                "1".to_string(),
                Record::from_display_string("John Doe^john@example.com^ADMIN]DEV]TEST"),
            );
            table.records.insert(
                "2".to_string(),
                Record::from_display_string("Jane Smith^jane@example.com^DEV]TEST\\LAB"),
            );
            table.touch_all();
            table.mark_dict_dirty();
        }
        {
            let handle = self.get_table_mut("PRODUCTS")?;
            let mut table = handle.write();
            table
                .dictionary
                .insert("DESC".to_string(), Record::from_display_string("1^DESCRIPTION^L^20"));
            table
                .dictionary
                .insert("PRICE".to_string(), Record::from_display_string("2^PRICE^R^10^^^^MD2"));
            // An association group, so the fixture reaches correlated
            // multivalues as well as plain ones: SUPPLIERS controls, SUP.CODES
            // pairs with it value for value, and SUP.CONTACTS pairs at the
            // second tier, sub-value for sub-value inside a supplier.
            table
                .dictionary
                .insert("SUPPLIERS".to_string(), Record::from_display_string("3^SUPPLIER^L^12"));
            table.dictionary.insert(
                "SUP.CODES".to_string(),
                Record::from_display_string("4^CODE^L^8^SUPPLIERS^V"),
            );
            table.dictionary.insert(
                "SUP.CONTACTS".to_string(),
                Record::from_display_string("5^CONTACT^L^10^SUPPLIERS^S"),
            );
            // P1's group is deliberately ragged - three suppliers, two codes -
            // and its second supplier has two contacts, so exploding it shows
            // both tiers and the empty cell a short member leaves behind.
            table.records.insert(
                "P1".to_string(),
                Record::from_display_string("Laptop^120000^ACME]GLOBEX]INITECH^A-1]G-7^ann]bob\\cara"),
            );
            table
                .records
                .insert("P2".to_string(), Record::from_display_string("Mouse^2500^ACME^A-2^dan"));
            table.touch_all();
            table.mark_dict_dirty();
        }
        // A queue file, so the fixture reaches the ordering primitive as well
        // as the record ones. Its policy is deliberately not the default -
        // ninety seconds and three deliveries - so anywhere the policy is shown
        // is somewhere it can be seen to be read rather than assumed.
        self.create_table_with(
            name,
            "JOBS",
            FileAttributes {
                durable: true,
                queue: Some(crate::db::queue::QueuePolicy {
                    visibility: Duration::from_secs(90),
                    max_deliveries: 3,
                }),
            },
        )?;
        {
            let handle = self.get_table_mut("JOBS")?;
            let mut table = handle.write();
            table
                .dictionary
                .insert("KIND".to_string(), Record::from_display_string("1^KIND^L^12"));
            table
                .dictionary
                .insert("REF".to_string(), Record::from_display_string("2^REFERENCE^L^12"));
            table.mark_dict_dirty();
        }
        // Enqueued through the queue itself rather than inserted, so the keys
        // are minted the way a client's would be and the order is real.
        for (kind, reference) in [("invoice", "P1"), ("invoice", "P2"), ("statement", "1")] {
            self.enqueue(
                name,
                "JOBS",
                Record::from_display_string(&format!("{}^{}", kind, reference)),
            )?;
        }
        self.save()?;
        if !original_account.is_empty() {
            let _ = self.logto(&original_account);
        } else {
            self.logout();
        }
        Ok(())
    }
}
