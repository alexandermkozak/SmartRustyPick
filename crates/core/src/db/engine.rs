use crate::db::hashfile::{self, FsyncPolicy, SectionMeta};
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
}

#[cfg(debug_assertions)]
fn note_guard_taken() {
    HELD_TABLES.with(|held| held.set(held.get() + 1));
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
        WriteMark { pending: 0, last_flush: Instant::now() }
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
/// 7. `durable_tables`, `clients`, `pending_writes`, `last_flush`.
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
    pub remote_select_lists: HashMap<String, SelectList>,
    pub remote_select_cursors: HashMap<String, usize>,
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
    /// Per-file durability flags read from the DIR file, cached so the write
    /// path does not touch the filesystem on every request.
    durable_tables: RwLock<HashMap<TableKey, bool>>,
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
        if field_name == "ID" { return Some(0); }
        let rec = self.dictionary.get(field_name)?;
        let idx_str = rec.fields.get(DICT_FIELD_IDX)?.values.get(0)?.sub_values.get(0)?;
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
        let code = dict_rec.fields.get(DICT_CONV_IDX)?.values.get(0)?.sub_values.get(0)?;
        if code.is_empty() { None } else { Some(code.as_str()) }
    }

    /// The 0-based index and conversion code of a dictionary field in a single
    /// dictionary lookup, instead of one lookup per property.
    pub fn field_index_and_conversion(&self, field_name: &str) -> Option<(usize, Option<String>)> {
        if field_name == "ID" { return Some((0, None)); }
        let rec = self.dictionary.get(field_name)?;
        let idx_str = rec.fields.get(DICT_FIELD_IDX)?.values.get(0)?.sub_values.get(0)?;
        let idx = match idx_str.parse::<usize>() {
            // Pick attribute 1 is 0-indexed 0 in our internal fields vector
            Ok(idx) if idx > 0 => idx - 1,
            _ => return None,
        };
        Some((idx, Self::conversion_code_from_dict_record(rec).map(str::to_string)))
    }
}

impl Database {
    pub fn new(base_storage_dir: &str, config: Option<crate::config::Config>) -> io::Result<Self> {
        let config = config.unwrap_or_else(crate::config::Config::load);
        let db = Database {
            storage_dir: base_storage_dir.to_string(),
            session_account: RwLock::new(String::new()),
            accounts_config: RwLock::new(Record::new()),
            tables: RwLock::new(HashMap::new()),
            available_tables: RwLock::new(HashMap::new()),
            available_stamps: RwLock::new(HashMap::new()),
            lru_order: Mutex::new(VecDeque::new()),
            max_loaded: config.max_loaded_tables
                .filter(|n| *n > 0)
                .unwrap_or(crate::config::DEFAULT_MAX_LOADED_TABLES),
            active_select_list: None,
            remote_select_lists: HashMap::new(),
            remote_select_cursors: HashMap::new(),
            clients: RwLock::new(ClientRegistry::default()),
            registry_stamp: Mutex::new(None),
            log_detail: config.log_detail.unwrap_or_else(|| "normal".to_string()),
            max_log_records: config.max_log_records.unwrap_or(100),
            records_per_group: config.records_per_group
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
            durable_tables: RwLock::new(HashMap::new()),
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

    fn load_account_registry(&self) -> io::Result<()> {
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
    pub fn refresh_account_registry(&self) -> io::Result<()> {
        let registry_path = format!("{}/accounts.reg", self.storage_dir);
        let stamp = Self::file_stamp(&registry_path);
        if *mlock(&self.registry_stamp) == Some(stamp) {
            return Ok(());
        }
        self.load_account_registry()
    }

    /// Reloads the client authorization map when `SYSTEM/$CLIENTS` changed on disk,
    /// so authorizations and revocations made by another process take effect.
    pub fn refresh_clients_if_stale(&self) -> io::Result<()> {
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

    fn ensure_system_account(&self) -> io::Result<()> {
        if self.get_account_dir("SYSTEM").is_none() {
            self.create_account("SYSTEM", None)?;
        }
        Ok(())
    }

    fn ensure_system_files(&self) -> io::Result<()> {
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
            if let Some(names_field) = config.fields.get(0) {
                if let Some(dirs_field) = config.fields.get(1) {
                    for (i, v) in names_field.values.iter().enumerate() {
                        if let Some(name) = v.sub_values.get(0) {
                            if name != "SYSTEM" {
                                if let Some(dir) = dirs_field.values.get(i).and_then(|v| v.sub_values.get(0)) {
                                    accounts_to_list.push((name.clone(), dir.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }

        let handle = self.get_table_mut("$ACCOUNTS")?;
        let mut accounts_table = handle.write();
        for (name, dir) in accounts_to_list {
            let mut record = Record::new();
            while record.fields.len() <= SYS_ACCOUNTS_PATH_IDX {
                record.fields.push(Field::default());
            }
            record.fields[SYS_ACCOUNTS_PATH_IDX].values.push(Value { sub_values: vec![dir] });
            accounts_table.records.insert(name, record);
        }
        accounts_table.touch_all();
        Ok(())
    }

    fn migrate_legacy_certs(&self) -> io::Result<()> {
        let certs_path = format!("{}/certs.reg", self.storage_dir);
        if !Path::new(&certs_path).exists() {
            return Ok(());
        }

        let mut map = HashMap::new();
        if Self::load_section(&mut map, &certs_path).is_ok() {
            if let Some(certs_rec) = map.remove("certs") {
                if let Some(f) = certs_rec.fields.get(0) {
                    let handle = self.get_table_mut("$CLIENTS")?;
                    let mut table = handle.write();
                    for v in &f.values {
                        for sv in &v.sub_values {
                            if !sv.is_empty() {
                                let tp_lower = sv.to_lowercase();
                                // Migrate if not already present
                                let already_exists = table.records.values().any(|r| {
                                    r.fields.get(0).and_then(|f| f.values.get(0)).and_then(|v| v.sub_values.get(0)) == Some(&tp_lower)
                                });
                                if !already_exists {
                                    let mut rec = Record::new();
                                    while rec.fields.len() <= SYS_CLIENTS_ADMIN_IDX {
                                        rec.fields.push(Field::default());
                                    }
                                    rec.fields[SYS_CLIENTS_THUMBPRINT_IDX].values.push(Value { sub_values: vec![tp_lower] });
                                    rec.fields[SYS_CLIENTS_ADMIN_IDX].values.push(Value { sub_values: vec!["Y".to_string()] });
                                    table.insert_record(&format!("migrated_{}", &sv[..8]), rec);
                                }
                            }
                        }
                    }
                }
            }
        }
        let _ = fs::rename(&certs_path, format!("{}.migrated", certs_path));
        Ok(())
    }

    fn self_heal_system_dictionaries(&self) -> io::Result<()> {
        let account = self.current_account();
        if account.is_empty() { return Ok(()); }
        let table_names: Vec<String> = self.account_tables(&account)?.into_iter()
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

    fn ensure_default_dictionaries(&self, table_name: &str) -> io::Result<bool> {
        let mut updated = false;
        let handle = self.get_table_mut(table_name)?;
        let mut table = handle.write();
        match table_name {
            "$LOGS" => {
                if !table.dictionary.contains_key("MESSAGE") {
                    table.dictionary.insert("MESSAGE".to_string(), Record::from_display_string("1^MESSAGE^L^60"));
                    updated = true;
                }
                if !table.dictionary.contains_key("DETAIL") {
                    table.dictionary.insert("DETAIL".to_string(), Record::from_display_string("2^DETAIL^L^40"));
                    updated = true;
                }
            }
            "$ACCOUNTS" => {
                if !table.dictionary.contains_key("PATH") {
                    table.dictionary.insert("PATH".to_string(), Record::from_display_string("1^PATH^L^50"));
                    updated = true;
                }
            }
            "$CLIENTS" => {
                if !table.dictionary.contains_key("THUMBPRINT") {
                    table.dictionary.insert("THUMBPRINT".to_string(), Record::from_display_string("1^THUMBPRINT^L^64"));
                    updated = true;
                }
                if !table.dictionary.contains_key("ACCOUNTS") {
                    table.dictionary.insert("ACCOUNTS".to_string(), Record::from_display_string("2^ACCOUNTS^L^30"));
                    updated = true;
                }
                if !table.dictionary.contains_key("ADMIN") {
                    table.dictionary.insert("ADMIN".to_string(), Record::from_display_string("3^ADMIN^L^5"));
                    updated = true;
                }
            }
            "$SAVEDLISTS" => {
                if !table.dictionary.contains_key("TABLE") {
                    table.dictionary.insert("TABLE".to_string(), Record::from_display_string("1^TABLE^L^20"));
                    updated = true;
                }
                if !table.dictionary.contains_key("IS_DICT") {
                    table.dictionary.insert("IS_DICT".to_string(), Record::from_display_string("2^IS_DICT^L^1"));
                    updated = true;
                }
            }
            "DIR" => {
                if !table.dictionary.contains_key("TYPE") {
                    table.dictionary.insert("TYPE".to_string(), Record::from_display_string("1^TYPE^L^1"));
                    updated = true;
                }
                if !table.dictionary.contains_key("DURABLE") {
                    table.dictionary.insert("DURABLE".to_string(), Record::from_display_string("2^DURABLE^L^7"));
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

    pub fn load_clients_from_table(&self) -> io::Result<()> {
        // Stamp before reading, so a concurrent write is detected on the next check.
        let stamp = self.disk_stamp("SYSTEM", "$CLIENTS");
        let handle = self.get_table_mut_for_account("SYSTEM", "$CLIENTS")?;
        let table = handle.read();
        let mut clients = Vec::new();
        for (name, record) in table.records.iter() {
            if let Some(tp) = record.fields.get(SYS_CLIENTS_THUMBPRINT_IDX).and_then(|f| f.values.get(0)).and_then(|v| v.sub_values.get(0)) {
                let tp_lower = tp.to_lowercase();
                let mut allowed_accounts = Vec::new();
                if let Some(acc_field) = record.fields.get(SYS_CLIENTS_ACCOUNTS_IDX) {
                    for v in &acc_field.values {
                        if let Some(acc) = v.sub_values.get(0) {
                            if !acc.is_empty() {
                                allowed_accounts.push(acc.clone());
                            }
                        }
                    }
                }
                let is_admin = record.fields.get(SYS_CLIENTS_ADMIN_IDX)
                    .and_then(|f| f.values.get(0))
                    .and_then(|v| v.sub_values.get(0))
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

    pub fn run_in_system_account<F, R>(&self, f: F) -> io::Result<R>
    where
        F: FnOnce(&Database) -> io::Result<R>,
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

    pub fn logto(&self, account_name: &str) -> io::Result<()> {
        if self.get_account_dir(account_name).is_none() {
            let _ = self.refresh_account_registry();
        }
        let _account_dir = self.get_account_dir(account_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)))?;

        if !self.is_current_account(account_name) {
            self.save()?; // Save current account's dirty tables
            self.set_current_account(account_name);
            self.ensure_available_tables(account_name)?;
        }
        Ok(())
    }

    fn ensure_available_tables(&self, account_name: &str) -> io::Result<()> {
        if self.get_account_dir(account_name).is_none() {
            let _ = self.refresh_account_registry();
        }
        let account_dir = self.get_account_dir(account_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)))?;

        // Re-scan whenever the account directory changed on disk, so tables created
        // by another process (e.g. the server while a local CLI is attached) are visible.
        let dir_stamp = Self::dir_modified(&account_dir);
        if rlock(&self.available_tables).contains_key(account_name)
            && rlock(&self.available_stamps).get(account_name) == Some(&dir_stamp) {
            return Ok(());
        }

        self.scan_available_tables(account_name)
    }

    /// True when the account is known to hold a file of this name, refreshing
    /// the listing first. Reading through the lock rather than handing out a
    /// borrow keeps the listing lock out of the caller's hands.
    fn account_has_table(&self, account: &str, name: &str) -> bool {
        self.ensure_available_tables(account).is_ok()
            && rlock(&self.available_tables).get(account).map(|s| s.contains(name)).unwrap_or(false)
    }

    /// Every file the account holds, unsorted.
    fn account_tables(&self, account: &str) -> io::Result<Vec<String>> {
        self.ensure_available_tables(account)?;
        Ok(rlock(&self.available_tables).get(account)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default())
    }

    /// Unconditionally re-reads the account directory. Used when the cached listing
    /// does not contain a requested table, because directory mtime resolution is
    /// coarse on some filesystems and may hide a freshly created table.
    fn scan_available_tables(&self, account_name: &str) -> io::Result<()> {
        let account_dir = self.get_account_dir(account_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)))?;
        let dir_stamp = Self::dir_modified(&account_dir);

        let mut tables = HashSet::new();
        if let Ok(entries) = fs::read_dir(&account_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        tables.insert(name.to_string());
                    }
                }
            }
        }
        wlock(&self.available_tables).insert(account_name.to_string(), tables);
        wlock(&self.available_stamps).insert(account_name.to_string(), dir_stamp);
        Ok(())
    }

    fn dir_modified(path: &str) -> Option<SystemTime> {
        fs::metadata(path).ok().and_then(|m| m.modified().ok())
    }

    fn file_stamp(path: &str) -> (Option<SystemTime>, u64) {
        match fs::metadata(path) {
            Ok(m) => (m.modified().ok(), m.len()),
            Err(_) => (None, 0),
        }
    }

    fn disk_stamp(&self, account: &str, name: &str) -> TableStamp {
        let storage = self.account_storage_dir(account);
        let data_path = format!("{}/{}/data", storage, name);
        // A hashed section spreads its records over many files, so its identity
        // is the meta file: its flush counter changes on every write, which is
        // a stronger signal than a timestamp whose resolution may be coarse.
        let (data_modified, data_len) = match hashfile::read_meta(&data_path) {
            Some(meta) => (
                Self::file_stamp(hashfile::section_dir(&data_path).join("meta").to_str().unwrap_or_default()).0,
                meta.version,
            ),
            None => Self::file_stamp(&data_path),
        };
        let (dict_modified, dict_len) = Self::file_stamp(&format!("{}/{}/dict", storage, name));
        TableStamp { data_modified, data_len, dict_modified, dict_len }
    }

    /// The table, when it can be read through a shared reference alone: it is
    /// already in memory and either holds unflushed changes of ours or still
    /// matches the files on disk. Callers that only hold `&self` cannot load or
    /// invalidate a table, so they use this to decide whether they have to fall
    /// back to an exclusive borrow. Returning the table itself saves them a
    /// second lookup.
    pub fn table_ready_for_read(&self, account: &str, name: &str) -> Option<TableHandle> {
        let handle = self.get_table_read_only_for_account(account, name)?;
        let ready = {
            let table = handle.read();
            table.is_dirty() || table.stamp == Some(self.disk_stamp(account, name))
        };
        if ready {
            // A table served through this path is still in use, so it must not
            // look like the coldest one to the next eviction.
            self.touch_lru(&(account.to_string(), name.to_string()));
            Some(handle)
        } else {
            None
        }
    }

    /// Drops a cached table whose backing files were modified by another process,
    /// forcing a fresh read on the next access. Locally modified (dirty) tables are
    /// kept untouched so that pending changes are never silently discarded.
    fn invalidate_if_stale(&self, account: &str, name: &str) {
        let key = (account.to_string(), name.to_string());
        let handle = match rlock(&self.tables).get(&key) {
            Some(handle) => handle.clone(),
            None => return,
        };
        let stale = {
            let table = handle.read();
            !table.is_dirty() && table.stamp != Some(self.disk_stamp(account, name))
        };
        if !stale {
            return;
        }
        // Re-check under the map lock: another thread may have replaced the
        // entry, or made it dirty, since the check above.
        let mut tables = wlock(&self.tables);
        let still_stale = match tables.get(&key) {
            Some(current) => {
                let table = current.read();
                !table.is_dirty() && table.stamp != Some(self.disk_stamp(account, name))
            }
            None => false,
        };
        if still_stale {
            tables.remove(&key);
            drop(tables);
            self.forget_lru(&key);
        }
    }

    /// Moves a table to the young end of the eviction order.
    fn touch_lru(&self, key: &TableKey) {
        let mut lru = mlock(&self.lru_order);
        if let Some(pos) = lru.iter().position(|x| x == key) {
            let entry = lru.remove(pos).unwrap();
            lru.push_back(entry);
        } else {
            lru.push_back(key.clone());
        }
    }

    /// Removes a table from the eviction order entirely.
    fn forget_lru(&self, key: &TableKey) {
        let mut lru = mlock(&self.lru_order);
        if let Some(pos) = lru.iter().position(|x| x == key) {
            lru.remove(pos);
        }
    }

    pub fn create_account(&self, name: &str, directory: Option<&str>) -> io::Result<()> {
        // Pick up accounts registered by another process, otherwise persisting our own
        // snapshot of the registry would erase them.
        let _ = self.refresh_account_registry();
        if self.get_account_dir(name).is_some() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("Account '{}' already exists", name)));
        }

        let dir = directory.map(|s| s.to_string()).unwrap_or_else(|| format!("{}/{}", self.storage_dir, name));
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
            config.fields[0].values.push(Value { sub_values: vec![name.to_string()] });
            config.fields[1].values.push(Value { sub_values: vec![dir.clone()] });
        }

        self.persist_account_registry()?;

        // Update $ACCOUNTS table if it exists
        self.run_in_system_account(|db| {
            if rlock(&db.available_tables).get("SYSTEM").map(|s| s.contains("$ACCOUNTS")).unwrap_or(false) {
                {
                    let handle = db.get_table_mut("$ACCOUNTS")?;
                    let mut accounts_table = handle.write();
                    let mut record = Record::new();
                    while record.fields.len() <= SYS_ACCOUNTS_PATH_IDX {
                        record.fields.push(Field::default());
                    }
                    record.fields[SYS_ACCOUNTS_PATH_IDX].values.push(Value { sub_values: vec![dir] });
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

    fn persist_account_registry(&self) -> io::Result<()> {
        let mut map = HashMap::new();
        map.insert("registry".to_string(), rlock(&self.accounts_config).clone());
        let path = format!("{}/accounts.reg", self.storage_dir);
        Self::save_section(&map, &path)?;
        *mlock(&self.registry_stamp) = Some(Self::file_stamp(&path));
        Ok(())
    }

    pub fn delete_account(&self, name: &str) -> io::Result<()> {
        if name == "SYSTEM" {
            return Err(io::Error::new(io::ErrorKind::Other, "Cannot delete SYSTEM account"));
        }

        let _ = self.refresh_account_registry();
        let dir = self.get_account_dir(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", name)))?;

        // Remove from registry
        {
            let mut config = wlock(&self.accounts_config);
            let position = config.fields.get(0).and_then(|names| {
                names.values.iter().position(|v| v.sub_values.get(0) == Some(&name.to_string()))
            });
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
            wlock(&self.durable_tables).remove(&key);
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
        let names_field = config.fields.get(0)?;
        let dirs_field = config.fields.get(1)?;
        let pos = names_field.values.iter().position(|v| v.sub_values.get(0) == Some(&account_name.to_string()))?;
        dirs_field.values.get(pos)?.sub_values.get(0).cloned()
    }

    pub fn current_storage_dir(&self) -> String {
        self.get_account_dir(&self.current_account()).unwrap_or_else(|| self.storage_dir.clone())
    }

    pub fn get_table_read_only(&self, name: &str) -> Option<TableHandle> {
        self.get_table_read_only_for_account(&self.current_account(), name)
    }

    /// The table if it is already in memory, without loading or refreshing it.
    pub fn get_table_read_only_for_account(&self, account: &str, name: &str) -> Option<TableHandle> {
        rlock(&self.tables).get(&(account.to_string(), name.to_string())).cloned()
    }

    pub fn get_table(&self, name: &str) -> Option<TableHandle> {
        self.get_table_for_account(&self.current_account(), name)
    }

    pub fn get_table_for_account(&self, account: &str, name: &str) -> Option<TableHandle> {
        self.resolve_table(account, name, false).ok()
    }

    pub fn get_table_mut(&self, name: &str) -> io::Result<TableHandle> {
        self.get_table_mut_for_account(&self.current_account(), name)
    }

    /// The table, loaded if it is not in memory yet.
    ///
    /// Named `_mut` for the callers that go on to write to it; the database
    /// itself is only borrowed shared, because the table is locked separately
    /// through the handle this returns.
    pub fn get_table_mut_for_account(&self, account: &str, name: &str) -> io::Result<TableHandle> {
        self.resolve_table(account, name, true)
    }

    /// Finds a table in the cache, loading it when it is not there.
    ///
    /// `create_missing` distinguishes the two callers: a reader wants `None`
    /// for a file whose records cannot be read, while a writer wants an empty
    /// table created for a directory that exists but holds no section yet.
    ///
    /// Two connections may load the same cold table at the same time. The
    /// second one to finish finds the first one's entry already in the map and
    /// discards its own copy, which costs a duplicate read of a file nobody had
    /// written to yet, and is what keeps the map lock off the disk.
    fn resolve_table(&self, account: &str, name: &str, create_missing: bool) -> io::Result<TableHandle> {
        let not_found = || io::Error::new(
            io::ErrorKind::NotFound,
            format!("Table '{}' not found in account '{}'", name, account),
        );

        self.ensure_available_tables(account)?;
        if !self.account_has_table(account, name) {
            // Might have been created by another process since the last scan.
            self.scan_available_tables(account)?;
        }
        // Strict validation: the name must be one the account listing knows,
        // and the listing's spelling of it is the one used from here on.
        let name_str = match rlock(&self.available_tables).get(account).and_then(|s| s.get(name)) {
            Some(validated) => validated.clone(),
            None => return Err(not_found()),
        };

        self.invalidate_if_stale(account, &name_str);

        let key = (account.to_string(), name_str.clone());
        if let Some(handle) = rlock(&self.tables).get(&key).cloned() {
            self.touch_lru(&key);
            return Ok(handle);
        }

        // Loading touches the disk, so it happens without the map locked.
        let table = match self.load_table_for_account(account, &name_str) {
            Ok(table) => table,
            Err(e) if e.kind() == io::ErrorKind::NotFound && create_missing => {
                let storage = self.account_storage_dir(account);
                let table_dir = format!("{}/{}", storage, name_str);
                if !Path::new(&table_dir).exists() {
                    fs::create_dir_all(&table_dir)?;
                }
                let mut table = Table::new();
                table.data_meta = Self::init_data_section(&table_dir, self.records_per_group)?;
                File::create(format!("{}/dict", table_dir))?;
                table
            }
            Err(e) => return Err(e),
        };

        let handle = {
            let mut tables = wlock(&self.tables);
            tables.entry(key.clone()).or_insert_with(|| TableHandle::new(table)).clone()
        };
        self.touch_lru(&key);
        self.evict_if_over_budget();
        Ok(handle)
    }

    /// Writes out and drops the coldest tables once the cache is over budget.
    ///
    /// A table another thread still holds a handle to is skipped rather than
    /// evicted: dropping it from the map would let a third thread load a second
    /// copy from disk and the two would overwrite each other. The flush happens
    /// with the map still locked, for the same reason - nothing may reload the
    /// table between it leaving the cache and its changes reaching the disk.
    fn evict_if_over_budget(&self) {
        if rlock(&self.tables).len() <= self.max_loaded {
            return;
        }
        let mut tables = wlock(&self.tables);
        let mut lru = mlock(&self.lru_order);
        let mut skipped = Vec::new();
        while tables.len() > self.max_loaded {
            let key = match lru.pop_front() {
                Some(key) => key,
                None => break,
            };
            let handle = match tables.get(&key) {
                Some(handle) => handle.clone(),
                // Already gone; the order was just stale.
                None => continue,
            };
            // The map's own handle, our clone, and nothing else.
            if handle.refs() > 2 {
                skipped.push(key);
                continue;
            }
            let _ = self.flush_handle(&key, &handle);
            tables.remove(&key);
        }
        for key in skipped {
            lru.push_back(key);
        }
    }

    /// Files currently held in memory.
    pub fn loaded_table_count(&self) -> usize {
        rlock(&self.tables).len()
    }

    /// Drops every cached table without writing it out. Test support: a caller
    /// that wants to observe a load from disk needs the cache empty first.
    pub fn clear_loaded_tables(&self) {
        wlock(&self.tables).clear();
        mlock(&self.lru_order).clear();
    }

    fn load_table_for_account(&self, account: &str, name: &str) -> io::Result<Table> {
        let storage = self.account_storage_dir(account);
        let data_path = format!("{}/{}/data", storage, name);
        let mut table = Table::new();
        // Take the stamp before reading: if the files change while we read them, the
        // stamp will no longer match and the table is reloaded on the next access.
        table.stamp = Some(self.disk_stamp(account, name));
        if hashfile::is_hashfile(&data_path) {
            table.data_meta = hashfile::load(&data_path, &mut table.records)?;
        } else {
            // Pre-hashfile database: read the flat file and remember to convert
            // it on the first flush, so upgrading needs no migration step.
            Self::load_section(&mut table.records, &data_path)?;
            table.legacy_data = !table.records.is_empty() || Path::new(&data_path).exists();
        }
        Self::load_section(&mut table.dictionary, &format!("{}/{}/dict", storage, name))?;
        Ok(table)
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
        let fsync = if self.durable_writes || rlock(&self.durable_tables).get(key).copied().unwrap_or(false) {
            self.durable_fsync
        } else {
            self.fsync
        };

        let mut table = handle.write();
        if !table.is_dirty() {
            return Ok(());
        }
        let table = &mut *table;

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
        table.clear_dirty();
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
        self.get_account_dir(account_name).unwrap_or_else(|| self.storage_dir.clone())
    }

    /// Writes every pending change to disk immediately.
    ///
    /// The tables are snapshotted first and then locked one at a time, so a
    /// flush of a large file never holds up work on any other file. Because it
    /// locks tables, it must not be called while a table guard is held - see
    /// the locking rules on [`Database`].
    pub fn save(&self) -> io::Result<()> {
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
                // Never refresh the stamp of a table we did not write: doing so would
                // mark a snapshot that is already stale on disk as up to date and the
                // freshness check would stop reloading it.
                continue;
            }
            self.flush_handle(&key, &handle)?;
            let stamp = self.disk_stamp(&key.0, &key.1);
            handle.write().stamp = Some(stamp);
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
            + mlock(&self.write_marks).values().map(|mark| mark.pending).sum::<usize>()
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
    pub fn note_write(&self) -> io::Result<()> {
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
    pub fn note_write_for(&self, account: &str, name: &str) -> io::Result<()> {
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
        if let Some(handle) = self.get_table_read_only_for_account(&key.0, &key.1) {
            if handle.read().is_dirty() {
                self.flush_handle(key, &handle)?;
                let stamp = self.disk_stamp(&key.0, &key.1);
                handle.write().stamp = Some(stamp);
                if key.0 == "SYSTEM" && key.1 == "$CLIENTS" {
                    self.load_clients_from_table()?;
                }
            }
        }
        mlock(&self.write_marks).insert(key.clone(), WriteMark::fresh());
        Ok(())
    }

    /// Flushes if the interval has elapsed. Intended for a background ticker,
    /// so an idle server still persists the tail of a burst promptly.
    pub fn flush_if_due(&self) -> io::Result<bool> {
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
        if !Path::new(path).exists() { return Ok(()); }
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        loop {
            let mut len_bytes = [0u8; 8];
            if let Err(e) = reader.read_exact(&mut len_bytes) {
                if e.kind() == io::ErrorKind::UnexpectedEof { break; }
                return Err(e);
            }
            let key_len = u64::from_le_bytes(len_bytes) as usize;
            if key_len > 1024 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Key too large: {} bytes in {}", key_len, path)));
            }
            let mut key_bytes = vec![0u8; key_len];
            reader.read_exact(&mut key_bytes)?;
            let key = String::from_utf8_lossy(&key_bytes).to_string();

            let mut data_len_bytes = [0u8; 8];
            reader.read_exact(&mut data_len_bytes)?;
            let data_len = u64::from_le_bytes(data_len_bytes) as usize;
            if data_len > 100 * 1024 * 1024 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Record too large: {} bytes for key '{}' in {}", data_len, key, path)));
            }

            let mut data = vec![0u8; data_len];
            reader.read_exact(&mut data)?;
            map.insert(key, Record::from_bytes(&data));
        }
        Ok(())
    }

    fn save_section(map: &HashMap<String, Record>, path: &str) -> io::Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
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

    /// Every file in the account with its durability flag, sorted by name.
    ///
    /// The flag is otherwise only readable by reading the account's `DIR` file,
    /// which is an obscure interface for something a client may reasonably want
    /// beside the name.
    pub fn list_tables_with_durability_for_account(&self, account: &str) -> Vec<(String, bool)> {
        self.list_tables_for_account(account)
            .into_iter()
            .map(|name| {
                let durable = self.is_table_durable_for_account(account, &name);
                (name, durable)
            })
            .collect()
    }

    /// Every account in the registry, sorted, picking up accounts created by
    /// another process since the last look.
    pub fn list_accounts(&self) -> Vec<String> {
        let _ = self.refresh_account_registry();
        let mut names: Vec<String> = rlock(&self.accounts_config).fields.get(0)
            .map(|f| f.values.iter().filter_map(|v| v.sub_values.get(0).cloned()).collect())
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
                let record_count = files.iter()
                    .map(|file| self.file_record_count(&directory, &name, file))
                    .sum();
                let (disk_bytes, _) = Self::tree_stats(Path::new(&directory));
                AccountStats { name, directory, file_count: files.len(), record_count, disk_bytes }
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
    pub fn file_statistics(&self, account: &str, name: &str) -> io::Result<FileStats> {
        if !self.account_has_table(account, name) {
            self.scan_available_tables(account)?;
        }
        if !self.account_has_table(account, name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Table '{}' not found in account '{}'", name, account),
            ));
        }

        let durable = self.is_table_durable_for_account(account, name);
        let directory = self.account_storage_dir(account);
        let file_dir = format!("{}/{}", directory, name);
        let data_path = format!("{}/data", file_dir);
        let meta = hashfile::read_meta(&data_path);
        let group_sizes = hashfile::group_sizes(&data_path);
        let (disk_bytes, modified) = Self::tree_stats(Path::new(&file_dir));

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

        Ok(FileStats {
            account: account.to_string(),
            name: name.to_string(),
            record_count,
            dict_count,
            modulus: meta.map(|m| m.modulus).unwrap_or(0),
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
        })
    }

    pub fn is_table_available(&self, name: &str) -> bool {
        self.account_has_table(&self.current_account(), name)
    }

    pub fn is_table_loaded(&self, name: &str) -> bool {
        rlock(&self.tables).contains_key(&(self.current_account(), name.to_string()))
    }

    pub fn create_table(&self, name: &str) -> io::Result<()> {
        self.create_table_for_account(&self.current_account(), name)
    }

    /// Creates a file and marks it durable, so every write to it is flushed
    /// before being acknowledged regardless of the global buffering settings.
    pub fn create_table_durable(&self, name: &str, durable: bool) -> io::Result<()> {
        self.create_table_for_account_durable(&self.current_account(), name, durable)
    }

    pub fn create_table_for_account_durable(&self, account: &str, name: &str, durable: bool) -> io::Result<()> {
        self.create_table_for_account(account, name)?;
        if durable {
            self.set_table_durable_for_account(account, name, true)?;
        }
        Ok(())
    }

    pub fn create_table_for_account(&self, account: &str, name: &str) -> io::Result<()> {
        if account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
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
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("Table '{}' already exists", name)));
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

    pub fn delete_table(&self, name: &str) -> io::Result<()> {
        self.delete_table_for_account(&self.current_account(), name)
    }

    pub fn delete_table_for_account(&self, account: &str, name: &str) -> io::Result<()> {
        if account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        if !self.account_has_table(account, name) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Table '{}' not found", name)));
        }

        let key = (account.to_string(), name.to_string());
        wlock(&self.tables).remove(&key);
        wlock(&self.durable_tables).remove(&key);
        if let Some(available) = wlock(&self.available_tables).get_mut(account) {
            available.remove(name);
        }
        self.forget_lru(&key);

        let storage = self.account_storage_dir(account);
        let table_dir = format!("{}/{}", storage, name);
        let _ = fs::remove_dir_all(table_dir);

        Ok(())
    }

    pub fn sync_dir_file(&self) -> io::Result<()> {
        self.sync_dir_file_for_account(&self.current_account())
    }

    pub fn sync_dir_file_for_account(&self, account: &str) -> io::Result<()> {
        let tables = self.list_tables_for_account(account);
        let handle = self.get_table_mut_for_account(account, "DIR")?;
        let mut dir_table = handle.write();
        // The listing is rebuilt from scratch, but the durability flag is not
        // derived from the filesystem, so it has to be carried over.
        let durable: HashMap<String, bool> = dir_table.records.iter()
            .map(|(k, r)| (k.clone(), Self::record_is_durable(r)))
            .collect();
        dir_table.records.clear();
        for t in tables {
            if t != "DIR" {
                let flag = durable.get(&t).copied().unwrap_or(false);
                dir_table.records.insert(t, Self::dir_entry(flag));
            }
        }
        // The listing is rebuilt from scratch, so every group has to be rewritten.
        dir_table.touch_all();
        Ok(())
    }

    fn dir_entry(durable: bool) -> Record {
        let mut rec = Record::new();
        while rec.fields.len() <= DIR_DURABLE_IDX {
            rec.fields.push(Field::default());
        }
        rec.fields[DIR_TYPE_IDX].values = vec![Value { sub_values: vec!["F".to_string()] }];
        rec.fields[DIR_DURABLE_IDX].values = vec![Value {
            sub_values: vec![if durable { "Y".to_string() } else { String::new() }],
        }];
        rec
    }

    fn ensure_dir_dictionary(dir_table: &mut Table) {
        if !dir_table.dictionary.contains_key("TYPE") {
            dir_table.dictionary.insert("TYPE".to_string(), Record::from_display_string("1^TYPE^L^1"));
            dir_table.mark_dict_dirty();
        }
        if !dir_table.dictionary.contains_key("DURABLE") {
            dir_table.dictionary.insert("DURABLE".to_string(), Record::from_display_string("2^DURABLE^L^7"));
            dir_table.mark_dict_dirty();
        }
    }

    fn record_is_durable(record: &Record) -> bool {
        record.fields.get(DIR_DURABLE_IDX)
            .and_then(|f| f.values.get(0))
            .and_then(|v| v.sub_values.get(0))
            .map(|s| matches!(s.trim().to_uppercase().as_str(), "Y" | "YES" | "1" | "TRUE" | "DURABLE"))
            .unwrap_or(false)
    }

    pub fn set_table_durable(&self, name: &str, durable: bool) -> io::Result<()> {
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
    pub fn set_table_durable_for_account(&self, account: &str, name: &str, durable: bool) -> io::Result<()> {
        if account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        if !self.account_has_table(account, name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Table '{}' not found in account '{}'", name, account),
            ));
        }
        if name == "DIR" {
            // DIR holds the flags; it is not one of the files they describe, and
            // an entry for itself would be dropped the next time the listing is
            // rebuilt. Recording a promise nothing honours is worse than saying no.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The DIR file's durability is not settable: its own writes are always flushed at once",
            ));
        }
        // The flag lives in DIR, so an account without one gets it now rather
        // than silently losing the requested durability.
        self.ensure_dir_file_for_account(account)?;
        {
            let handle = self.get_table_mut_for_account(account, "DIR")?;
            let mut dir_table = handle.write();
            let mut rec = dir_table.records.get(name).cloned().unwrap_or_else(|| Self::dir_entry(false));
            while rec.fields.len() <= DIR_DURABLE_IDX {
                rec.fields.push(Field::default());
            }
            if rec.fields[DIR_TYPE_IDX].values.is_empty() {
                rec.fields[DIR_TYPE_IDX].values = vec![Value { sub_values: vec!["F".to_string()] }];
            }
            rec.fields[DIR_DURABLE_IDX].values = vec![Value {
                sub_values: vec![if durable { "Y".to_string() } else { String::new() }],
            }];
            dir_table.insert_record(name, rec);
            Self::ensure_dir_dictionary(&mut dir_table);
        }
        wlock(&self.durable_tables).insert((account.to_string(), name.to_string()), durable);
        self.save()
    }

    pub fn is_table_durable(&self, name: &str) -> bool {
        self.is_table_durable_for_account(&self.current_account(), name)
    }

    /// True when this file must be flushed on every write, either because the
    /// whole database runs in durable mode or because its DIR entry says so.
    pub fn is_table_durable_for_account(&self, account: &str, name: &str) -> bool {
        if self.durable_writes {
            return true;
        }
        let key = (account.to_string(), name.to_string());
        if let Some(flag) = rlock(&self.durable_tables).get(&key) {
            return *flag;
        }
        let has_dir = name != "DIR" && self.account_has_table(account, "DIR");
        let flag = if has_dir {
            match self.get_table_mut_for_account(account, "DIR") {
                Ok(dir) => dir.read().records.get(name).map(Self::record_is_durable).unwrap_or(false),
                Err(_) => false,
            }
        } else {
            false
        };
        wlock(&self.durable_tables).insert(key, flag);
        flag
    }

    /// Creates the account's DIR file if it does not have one yet.
    pub fn ensure_dir_file_for_account(&self, account: &str) -> io::Result<()> {
        if self.account_has_table(account, "DIR") {
            return Ok(());
        }
        self.create_table_for_account(account, "DIR")?;
        self.sync_dir_file_for_account(account)
    }

    pub fn ensure_dir_file(&self) -> io::Result<bool> {
        Ok(self.account_has_table(&self.current_account(), "DIR"))
    }

    pub fn create_dir_file(&self) -> io::Result<()> {
        self.create_table("DIR")?;
        self.sync_dir_file()
    }

    pub fn get_account_for_dir(&self, dir: &str) -> Option<String> {
        let config = rlock(&self.accounts_config);
        let names_field = config.fields.get(0)?;
        let dirs_field = config.fields.get(1)?;
        for (i, v) in dirs_field.values.iter().enumerate() {
            if let Some(d) = v.sub_values.get(0) {
                if d == dir {
                    return names_field.values.get(i)?.sub_values.get(0).cloned();
                }
            }
        }
        None
    }

    pub fn get_conversion_code_read_only(&self, table_name: &str, field_name: &str) -> Option<String> {
        self.get_conversion_code_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_conversion_code_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<String> {
        self.get_table_read_only_for_account(account, table_name)?.read().conversion_code(field_name)
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
        if field_name == "ID" { return "ID".to_string(); }
        if let Some(rec) = table.dictionary.get(field_name) {
            if let Some(f2) = rec.fields.get(DICT_NAME_IDX) {
                if let Some(v1) = f2.values.get(0) {
                    if let Some(header) = v1.sub_values.get(0) {
                        if !header.is_empty() {
                            return header.clone();
                        }
                    }
                }
            }
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
        if field_name == "ID" { return DEFAULT_FIELD_WIDTH; }
        if let Some(rec) = table.dictionary.get(field_name) {
            if let Some(f4) = rec.fields.get(DICT_WIDTH_IDX) {
                if let Some(v1) = f4.values.get(0) {
                    if let Some(width_str) = v1.sub_values.get(0) {
                        if let Ok(width) = width_str.parse::<usize>() {
                            return width;
                        }
                    }
                }
            }
        }
        DEFAULT_FIELD_WIDTH
    }

    pub fn get_field_justification_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_justification_in(&handle.read(), field_name),
            None => "L".to_string(),
        }
    }

    /// The justification of a field. See [`field_header_in`](Self::field_header_in).
    pub fn field_justification_in(table: &Table, field_name: &str) -> String {
        if field_name == "ID" { return "L".to_string(); }
        if let Some(rec) = table.dictionary.get(field_name) {
            if let Some(f3) = rec.fields.get(DICT_JUSTIFY_IDX) {
                if let Some(v1) = f3.values.get(0) {
                    if let Some(just) = v1.sub_values.get(0) {
                        if !just.is_empty() {
                            return just.clone();
                        }
                    }
                }
            }
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
            if let Some(record) = table.dictionary.get(&key) {
                if let Some(field_idx_str) = record.fields.get(DICT_FIELD_IDX).and_then(|f| f.values.get(0)).and_then(|v| v.sub_values.get(0)) {
                    if let Ok(idx) = field_idx_str.parse::<usize>() {
                        if idx > 0 && !fields_map.contains_key(&idx) {
                            fields_map.insert(idx, key);
                        }
                    }
                }
            }
        }

        let mut sorted_indices: Vec<_> = fields_map.keys().cloned().collect();
        sorted_indices.sort();

        sorted_indices.into_iter().map(|idx| fields_map.get(&idx).unwrap().clone()).collect()
    }

    pub fn apply_conversion(val: &str, code: &str) -> String {
        if code.starts_with("MD") && code.len() > 2 {
            if let Ok(decimals) = code[2..].parse::<usize>() {
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
        }
        val.to_string()
    }

    pub fn apply_iconv(val: &str, code: &str) -> String {
        if code.starts_with("MD") && code.len() > 2 {
            if let Ok(decimals) = code[2..].parse::<usize>() {
                if let Ok(f) = val.parse::<f64>() {
                    let multiplier = 10f64.powi(decimals as i32);
                    return format!("{:.0}", (f * multiplier).round());
                }
            }
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

    pub fn format_record_field_for_account(&self, account: &str, table_name: &str, record: &Record, field_name: &str) -> String {
        self.format_record_field_at_for_account(account, table_name, record, field_name, None)
    }

    /// Renders one column of one output row. `position` is the row's exploded
    /// position, so an exploded column shows only the value (or sub-value) that
    /// put the row there; `None` renders the whole field, which is what every
    /// unexploded row does.
    pub fn format_record_field_at(&self, table_name: &str, record: &Record, field_name: &str, position: Option<ValuePosition>) -> String {
        self.format_record_field_at_for_account(&self.current_account(), table_name, record, field_name, position)
    }

    pub fn format_record_field_at_for_account(&self, account: &str, table_name: &str, record: &Record, field_name: &str, position: Option<ValuePosition>) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::format_record_field_at_in(&handle.read(), record, field_name, position),
            None => String::new(),
        }
    }

    /// Renders one column of one row from a table the caller has already
    /// resolved, in a single dictionary lookup. See
    /// [`field_header_in`](Self::field_header_in).
    pub fn format_record_field_at_in(table: &Table, record: &Record, field_name: &str, position: Option<ValuePosition>) -> String {
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

    pub fn get_field_index_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" { return Some(0); }
        self.get_table_read_only_for_account(account, table_name)?.read().field_index(field_name)
    }

    pub fn get_field_index(&self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_index_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" { return Some(0); }
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
            let idx = dict_rec.fields.get(DICT_FIELD_IDX)
                .and_then(|f| f.values.get(0))
                .and_then(|v| v.sub_values.get(0))
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
        let Some(field) = field else { return serde_json::Value::String(String::new()) };

        match field.values.as_slice() {
            [] => return serde_json::Value::String(String::new()),
            [only] if only.sub_values.len() <= 1 => {
                let text = only.sub_values.first().map(String::as_str).unwrap_or("");
                return serde_json::Value::String(convert(text));
            }
            _ => {}
        }

        serde_json::Value::Array(
            field.values.iter().map(|value| match value.sub_values.as_slice() {
                [] => serde_json::Value::String(String::new()),
                [only] => serde_json::Value::String(convert(only)),
                subs => serde_json::Value::Array(
                    subs.iter().map(|sub| serde_json::Value::String(convert(sub))).collect(),
                ),
            }).collect(),
        )
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
    fn deserialize_field(val: &serde_json::Value, conversion: Option<&str>) -> Vec<Value> {
        let scalar = |v: &serde_json::Value| -> String {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
                other => other.to_string(),
            };
            match conversion {
                Some(code) => Self::apply_iconv(&text, code),
                None => text,
            }
        };

        match val {
            serde_json::Value::Array(values) => values
                .iter()
                .map(|value| match value {
                    serde_json::Value::Array(subs) => Value {
                        sub_values: subs.iter().map(&scalar).collect(),
                    },
                    other => Value { sub_values: vec![scalar(other)] },
                })
                .collect(),
            other => vec![Value { sub_values: vec![scalar(other)] }],
        }
    }

    pub fn deserialize_record(&self, table_name: &str, data: &serde_json::Value) -> Option<Record> {
        self.deserialize_record_for_account(&self.current_account(), table_name, data)
    }

    pub fn deserialize_record_for_account(&self, account: &str, table_name: &str, data: &serde_json::Value) -> Option<Record> {
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
            if let Some(f1) = dict_rec.fields.get(DICT_FIELD_IDX) {
                if let Some(v1) = f1.values.get(0) {
                    if let Some(idx_str) = v1.sub_values.get(0) {
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            if idx > 0 {
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
                    }
                }
            }
        }

        for (key, val) in obj {
            if let Some(&idx) = attr_map.get(key) {
                while record.fields.len() <= idx {
                    record.fields.push(Field::default());
                }
                record.fields[idx].values = Self::deserialize_field(val, conv_map.get(key).map(String::as_str));
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

    pub fn log_error(&self, account: &str, message: &str) -> io::Result<()> {
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
            record.fields[SYS_LOGS_MESSAGE_IDX].values.push(Value { sub_values: vec![message.to_string()] });

            // Field 2: Detail
            if db.log_detail == "detailed" {
                record.fields[SYS_LOGS_DETAIL_IDX].values.push(Value { sub_values: vec![format!("UTC: {}", now)] });
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

    pub fn add_authorized_client(&self, name: &str, thumbprint: &str, allowed_accounts: Vec<String>, is_admin: bool) -> io::Result<()> {
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
                record.fields[SYS_CLIENTS_THUMBPRINT_IDX].values.push(Value { sub_values: vec![thumbprint_lower] });
                // Field 1: Allowed Accounts
                for acc in &allowed_accounts {
                    record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.push(Value { sub_values: vec![acc.clone()] });
                }
                // Field 2: Admin flag
                record.fields[SYS_CLIENTS_ADMIN_IDX].values.push(Value { sub_values: vec![if is_admin { "Y".to_string() } else { "".to_string() }] });

                table.insert_record(name, record);
            }
            db.save()?;

            // Update in-memory structures
            db.load_clients_from_table()?;

            // Sync with certs.reg for backward compatibility (optional but safe)
            db.save_certs()
        })
    }

    pub fn add_client_account(&self, name: &str, account: &str) -> io::Result<bool> {
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
                    let already_exists = record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.iter().any(|v| v.sub_values.get(0) == Some(&account.to_string()));

                    if !already_exists {
                        record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.push(Value { sub_values: vec![account.to_string()] });
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

    pub fn remove_client_account(&self, name: &str, account: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let mut success = false;
            {
                let handle = db.get_table_mut("$CLIENTS")?;
                let mut table = handle.write();
                if let Some(record) = table.records.get_mut(name) {
                    if record.fields.len() > SYS_CLIENTS_ACCOUNTS_IDX {
                        let original_len = record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.len();
                        record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.retain(|v| v.sub_values.get(0).map(|s| s != account).unwrap_or(true));

                        if record.fields[SYS_CLIENTS_ACCOUNTS_IDX].values.len() < original_len {
                            table.mark_dirty(name);
                            success = true;
                        }
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

    pub fn remove_authorized_client(&self, name: &str) -> io::Result<bool> {
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

    pub fn save_certs(&self) -> io::Result<()> {
        let mut certs_rec = Record::new();
        certs_rec.fields.push(Field::default());
        for tp in rlock(&self.clients).certs.iter() {
            certs_rec.fields[0].values.push(Value { sub_values: vec![tp.clone()] });
        }
        let mut map = HashMap::new();
        map.insert("certs".to_string(), certs_rec);
        Self::save_section(&map, &format!("{}/certs.reg", self.storage_dir))
    }

    pub fn create_test_account(&self, name: &str) -> io::Result<()> {
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
            table.dictionary.insert("NAME".to_string(), Record::from_display_string("1^NAME^L^15"));
            table.dictionary.insert("EMAIL".to_string(), Record::from_display_string("2^EMAIL^L^20"));
            // ROLES is multivalued, and Jane's second role is sub-valued, so the
            // fixture exercises every level of the hierarchy rather than only
            // the flat one.
            table.dictionary.insert("ROLES".to_string(), Record::from_display_string("3^ROLES^L^20"));
            table.records.insert("1".to_string(), Record::from_display_string("John Doe^john@example.com^ADMIN]DEV]TEST"));
            table.records.insert("2".to_string(), Record::from_display_string("Jane Smith^jane@example.com^DEV]TEST\\LAB"));
            table.touch_all();
            table.mark_dict_dirty();
        }
        {
            let handle = self.get_table_mut("PRODUCTS")?;
            let mut table = handle.write();
            table.dictionary.insert("DESC".to_string(), Record::from_display_string("1^DESCRIPTION^L^20"));
            table.dictionary.insert("PRICE".to_string(), Record::from_display_string("2^PRICE^R^10^^^^MD2"));
            table.records.insert("P1".to_string(), Record::from_display_string("Laptop^120000"));
            table.records.insert("P2".to_string(), Record::from_display_string("Mouse^2500"));
            table.touch_all();
            table.mark_dict_dirty();
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
