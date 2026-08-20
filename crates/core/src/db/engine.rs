use crate::db::hashfile::{self, FsyncPolicy, SectionMeta};
use crate::db::models::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

pub struct Database {
    pub storage_dir: String,
    pub current_account: String,
    pub accounts_config: Record,
    pub loaded_tables: HashMap<(String, String), Table>,
    pub available_tables: HashMap<String, HashSet<String>>,
    pub available_stamps: HashMap<String, Option<SystemTime>>,
    pub lru_order: VecDeque<(String, String)>,
    pub max_loaded: usize,
    pub active_select_list: Option<SelectList>,
    pub remote_select_lists: HashMap<String, SelectList>,
    pub remote_select_cursors: HashMap<String, usize>,
    pub authorized_certs: HashSet<String>,
    pub authorized_clients: HashMap<String, ClientInfo>,
    pub registry_stamp: Option<(Option<SystemTime>, u64)>,
    pub clients_stamp: Option<TableStamp>,
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
    pending_writes: usize,
    last_flush: Instant,
    /// Per-file durability flags read from the DIR file, cached so the write
    /// path does not touch the filesystem on every request.
    durable_tables: HashMap<(String, String), bool>,
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
        Self::conversion_code_from_dict_record(rec)
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
        Some((idx, Self::conversion_code_from_dict_record(rec)))
    }
}

impl Database {
    pub fn new(base_storage_dir: &str, config: Option<crate::config::Config>) -> io::Result<Self> {
        let config = config.unwrap_or_else(crate::config::Config::load);
        let mut db = Database {
            storage_dir: base_storage_dir.to_string(),
            current_account: String::new(),
            accounts_config: Record::new(),
            loaded_tables: HashMap::new(),
            available_tables: HashMap::new(),
            available_stamps: HashMap::new(),
            lru_order: VecDeque::new(),
            max_loaded: 10,
            active_select_list: None,
            remote_select_lists: HashMap::new(),
            remote_select_cursors: HashMap::new(),
            authorized_certs: HashSet::new(),
            authorized_clients: HashMap::new(),
            registry_stamp: None,
            clients_stamp: None,
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
            pending_writes: 0,
            last_flush: Instant::now(),
            durable_tables: HashMap::new(),
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

    fn load_account_registry(&mut self) -> io::Result<()> {
        let registry_path = format!("{}/accounts.reg", self.storage_dir);
        // Stamp before reading, so a write racing with our read is caught next time.
        let stamp = Self::file_stamp(&registry_path);
        if Path::new(&registry_path).exists() {
            let mut map = HashMap::new();
            Self::load_section(&mut map, &registry_path)?;
            if let Some(reg_rec) = map.remove("registry") {
                self.accounts_config = reg_rec;
            }
        }
        self.registry_stamp = Some(stamp);
        Ok(())
    }

    /// Re-reads `accounts.reg` when it was modified by another process, so accounts
    /// created or deleted elsewhere are visible without restarting.
    pub fn refresh_account_registry(&mut self) -> io::Result<()> {
        let registry_path = format!("{}/accounts.reg", self.storage_dir);
        let stamp = Self::file_stamp(&registry_path);
        if self.registry_stamp == Some(stamp) {
            return Ok(());
        }
        self.load_account_registry()
    }

    /// Reloads the client authorization map when `SYSTEM/$CLIENTS` changed on disk,
    /// so authorizations and revocations made by another process take effect.
    pub fn refresh_clients_if_stale(&mut self) -> io::Result<()> {
        let _ = self.refresh_account_registry();
        if self.get_account_dir("SYSTEM").is_none() {
            return Ok(());
        }
        let stamp = self.disk_stamp("SYSTEM", "$CLIENTS");
        if self.clients_stamp == Some(stamp) {
            return Ok(());
        }
        self.run_in_system_account(|db| db.load_clients_from_table())
    }

    fn ensure_system_account(&mut self) -> io::Result<()> {
        if self.get_account_dir("SYSTEM").is_none() {
            self.create_account("SYSTEM", None)?;
        }
        Ok(())
    }

    fn ensure_system_files(&mut self) -> io::Result<()> {
        let account = "SYSTEM".to_string();
        self.ensure_available_tables(&account)?;
        let available = self.available_tables.get(&account).unwrap();

        // Ensure DIR file exists for SYSTEM account
        if !available.contains("DIR") {
            self.create_table("DIR")?;
            self.sync_dir_file()?;
        }

        // Ensure mandatory system files exist
        let system_files = vec!["$LOGS", "$ACCOUNTS", "$CLIENTS", "$SAVEDLISTS"];
        for file in system_files {
            if !self.available_tables.get(&account).unwrap().contains(file) {
                self.create_table(file)?;
            }
        }

        // Populate $ACCOUNTS with all non-SYSTEM accounts
        let mut accounts_to_list = Vec::new();
        if let Some(names_field) = self.accounts_config.fields.get(0) {
            if let Some(dirs_field) = self.accounts_config.fields.get(1) {
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

        let accounts_table = self.get_table_mut("$ACCOUNTS")?;
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

    fn migrate_legacy_certs(&mut self) -> io::Result<()> {
        let certs_path = format!("{}/certs.reg", self.storage_dir);
        if !Path::new(&certs_path).exists() {
            return Ok(());
        }

        let mut map = HashMap::new();
        if Self::load_section(&mut map, &certs_path).is_ok() {
            if let Some(certs_rec) = map.remove("certs") {
                if let Some(f) = certs_rec.fields.get(0) {
                    let table = self.get_table_mut("$CLIENTS")?;
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

    fn self_heal_system_dictionaries(&mut self) -> io::Result<()> {
        let account = self.current_account.clone();
        if account.is_empty() { return Ok(()); }
        self.ensure_available_tables(&account)?;
        let table_names: Vec<String> = self.available_tables.get(&account).unwrap().iter()
            .filter(|n| n.starts_with('$') || *n == "DIR")
            .cloned()
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

    fn ensure_default_dictionaries(&mut self, table_name: &str) -> io::Result<bool> {
        let mut updated = false;
        let table = self.get_table_mut(table_name)?;
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

    pub fn load_clients_from_table(&mut self) -> io::Result<()> {
        // Stamp before reading, so a concurrent write is detected on the next check.
        let stamp = self.disk_stamp("SYSTEM", "$CLIENTS");
        let table = self.get_table_mut("$CLIENTS")?;
        let mut clients = Vec::new();
        for record in table.records.values() {
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
                    thumbprint: tp_lower,
                    allowed_accounts,
                    is_admin,
                });
            }
        }
        self.clients_stamp = Some(stamp);
        self.authorized_clients.clear();
        self.authorized_certs.clear();
        for info in clients {
            let tp = info.thumbprint.clone();
            self.authorized_clients.insert(tp.clone(), info);
            self.authorized_certs.insert(tp);
        }
        Ok(())
    }

    pub fn run_in_system_account<F, R>(&mut self, f: F) -> io::Result<R>
    where
        F: FnOnce(&mut Database) -> io::Result<R>,
    {
        let original_account = self.current_account.clone();
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

    pub fn logout(&mut self) {
        let _ = self.save();
        self.current_account = String::new();
    }

    pub fn logto(&mut self, account_name: &str) -> io::Result<()> {
        if self.get_account_dir(account_name).is_none() {
            let _ = self.refresh_account_registry();
        }
        let _account_dir = self.get_account_dir(account_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)))?;

        if self.current_account != account_name {
            self.save()?; // Save current account's dirty tables
            self.current_account = account_name.to_string();
            self.ensure_available_tables(account_name)?;
        }
        Ok(())
    }

    fn ensure_available_tables(&mut self, account_name: &str) -> io::Result<()> {
        if self.get_account_dir(account_name).is_none() {
            let _ = self.refresh_account_registry();
        }
        let account_dir = self.get_account_dir(account_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)))?;

        // Re-scan whenever the account directory changed on disk, so tables created
        // by another process (e.g. the server while a local CLI is attached) are visible.
        let dir_stamp = Self::dir_modified(&account_dir);
        if self.available_tables.contains_key(account_name)
            && self.available_stamps.get(account_name) == Some(&dir_stamp) {
            return Ok(());
        }

        self.scan_available_tables(account_name)
    }

    /// Unconditionally re-reads the account directory. Used when the cached listing
    /// does not contain a requested table, because directory mtime resolution is
    /// coarse on some filesystems and may hide a freshly created table.
    fn scan_available_tables(&mut self, account_name: &str) -> io::Result<()> {
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
        self.available_tables.insert(account_name.to_string(), tables);
        self.available_stamps.insert(account_name.to_string(), dir_stamp);
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
    pub fn table_ready_for_read(&self, account: &str, name: &str) -> Option<&Table> {
        let table = self.get_table_read_only_for_account(account, name)?;
        if table.is_dirty() || table.stamp == Some(self.disk_stamp(account, name)) {
            Some(table)
        } else {
            None
        }
    }

    /// Drops a cached table whose backing files were modified by another process,
    /// forcing a fresh read on the next access. Locally modified (dirty) tables are
    /// kept untouched so that pending changes are never silently discarded.
    fn invalidate_if_stale(&mut self, account: &str, name: &str) {
        let key = (account.to_string(), name.to_string());
        let stale = match self.loaded_tables.get(&key) {
            Some(table) if !table.is_dirty() => table.stamp != Some(self.disk_stamp(account, name)),
            _ => false,
        };
        if stale {
            self.loaded_tables.remove(&key);
            if let Some(pos) = self.lru_order.iter().position(|x| x == &key) {
                self.lru_order.remove(pos);
            }
        }
    }

    pub fn create_account(&mut self, name: &str, directory: Option<&str>) -> io::Result<()> {
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
        let prev_acc = self.current_account.clone();
        self.current_account = "SYSTEM".to_string(); // Temporarily switch to SYSTEM context for registry

        // Add to accounts_config record
        while self.accounts_config.fields.len() < 2 {
            self.accounts_config.fields.push(Field::default());
        }
        self.accounts_config.fields[0].values.push(Value { sub_values: vec![name.to_string()] });
        self.accounts_config.fields[1].values.push(Value { sub_values: vec![dir.clone()] });

        self.persist_account_registry()?;

        // Update $ACCOUNTS table if it exists
        self.run_in_system_account(|db| {
            if db.available_tables.get("SYSTEM").map(|s| s.contains("$ACCOUNTS")).unwrap_or(false) {
                let accounts_table = db.get_table_mut("$ACCOUNTS")?;
                let mut record = Record::new();
                while record.fields.len() <= SYS_ACCOUNTS_PATH_IDX {
                    record.fields.push(Field::default());
                }
                record.fields[SYS_ACCOUNTS_PATH_IDX].values.push(Value { sub_values: vec![dir] });
                accounts_table.insert_record(name, record);
                db.save()?;
            }
            Ok(())
        })?;

        if !prev_acc.is_empty() && prev_acc != "SYSTEM" {
            let _ = self.logto(&prev_acc);
        } else if prev_acc.is_empty() {
            self.current_account = String::new();
        }
        Ok(())
    }

    fn persist_account_registry(&mut self) -> io::Result<()> {
        let mut map = HashMap::new();
        map.insert("registry".to_string(), self.accounts_config.clone());
        let path = format!("{}/accounts.reg", self.storage_dir);
        Self::save_section(&map, &path)?;
        self.registry_stamp = Some(Self::file_stamp(&path));
        Ok(())
    }

    pub fn delete_account(&mut self, name: &str) -> io::Result<()> {
        if name == "SYSTEM" {
            return Err(io::Error::new(io::ErrorKind::Other, "Cannot delete SYSTEM account"));
        }

        let _ = self.refresh_account_registry();
        let dir = self.get_account_dir(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", name)))?;

        // Remove from registry
        if let Some(names_field) = self.accounts_config.fields.get_mut(0) {
            if let Some(pos) = names_field.values.iter().position(|v| v.sub_values.get(0) == Some(&name.to_string())) {
                names_field.values.remove(pos);
                if let Some(dirs_field) = self.accounts_config.fields.get_mut(1) {
                    dirs_field.values.remove(pos);
                }
            }
        }

        // Persist registry
        self.persist_account_registry()?;

        // Remove from $ACCOUNTS table
        self.run_in_system_account(|db| {
            let table = db.get_table_mut("$ACCOUNTS")?;
            table.remove_record(name);
            db.save()
        })?;

        // Delete physical directory
        let _ = fs::remove_dir_all(dir);

        // Cleanup cache for this account
        let keys_to_remove: Vec<(String, String)> = self.loaded_tables.keys()
            .filter(|(acc, _)| acc == name)
            .cloned()
            .collect();
        for key in keys_to_remove {
            self.loaded_tables.remove(&key);
            if let Some(pos) = self.lru_order.iter().position(|x| x == &key) {
                self.lru_order.remove(pos);
            }
        }
        self.available_tables.remove(name);
        self.available_stamps.remove(name);

        if self.current_account == name {
            self.current_account = String::new();
        }

        Ok(())
    }

    pub fn get_account_dir(&self, account_name: &str) -> Option<String> {
        let names_field = self.accounts_config.fields.get(0)?;
        let dirs_field = self.accounts_config.fields.get(1)?;
        let pos = names_field.values.iter().position(|v| v.sub_values.get(0) == Some(&account_name.to_string()))?;
        dirs_field.values.get(pos)?.sub_values.get(0).cloned()
    }

    pub fn current_storage_dir(&self) -> String {
        self.get_account_dir(&self.current_account).unwrap_or_else(|| self.storage_dir.clone())
    }

    pub fn get_table_read_only(&self, name: &str) -> Option<&Table> {
        self.get_table_read_only_for_account(&self.current_account, name)
    }

    pub fn get_table_read_only_for_account(&self, account: &str, name: &str) -> Option<&Table> {
        self.loaded_tables.get(&(account.to_string(), name.to_string()))
    }

    pub fn get_table(&mut self, name: &str) -> Option<&Table> {
        let account = self.current_account.clone();
        self.get_table_for_account(&account, name)
    }

    pub fn get_table_for_account(&mut self, account: &str, name: &str) -> Option<&Table> {
        self.ensure_available_tables(account).ok()?;
        if !self.available_tables.get(account).map(|s| s.contains(name)).unwrap_or(false) {
            // Might have been created by another process since the last scan.
            self.scan_available_tables(account).ok()?;
        }

        // Strict validation: name must be in available_tables for this account
        let available = self.available_tables.get(account)?;
        if !available.contains(name) {
            return None;
        }

        // Use the validated name from available_tables
        let validated_name = available.get(name)?.clone();
        let name_str = validated_name;

        self.invalidate_if_stale(account, &name_str);

        let key = (account.to_string(), name_str.clone());
        if !self.loaded_tables.contains_key(&key) {
            if let Ok(table) = self.load_table_for_account(account, &name_str) {
                if self.loaded_tables.len() >= self.max_loaded {
                    if let Some(oldest_key) = self.lru_order.pop_front() {
                        let _ = self.save_table_for_account(&oldest_key.0, &oldest_key.1);
                        self.loaded_tables.remove(&oldest_key);
                    }
                }
                self.loaded_tables.insert(key.clone(), table);
                self.lru_order.push_back(key.clone());
            } else {
                return None;
            }
        } else {
            // Update LRU
            if let Some(pos) = self.lru_order.iter().position(|x| x == &key) {
                let n = self.lru_order.remove(pos).unwrap();
                self.lru_order.push_back(n);
            }
        }

        self.loaded_tables.get(&key)
    }

    pub fn get_table_mut(&mut self, name: &str) -> io::Result<&mut Table> {
        let account = self.current_account.clone();
        self.get_table_mut_for_account(&account, name)
    }

    pub fn get_table_mut_for_account(&mut self, account: &str, name: &str) -> io::Result<&mut Table> {
        self.ensure_available_tables(account)?;
        if !self.available_tables.get(account).map(|s| s.contains(name)).unwrap_or(false) {
            // Might have been created by another process since the last scan.
            self.scan_available_tables(account)?;
        }
        let available = self.available_tables.get_mut(account).unwrap();

        // Strict validation: name must be in available_tables
        if !available.contains(name) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Table '{}' not found in account '{}'", name, account)));
        }

        // Use the validated name from available_tables
        let validated_name = available.get(name).unwrap().clone();
        let name_str = validated_name;

        self.invalidate_if_stale(account, &name_str);

        let key = (account.to_string(), name_str.clone());
        if !self.loaded_tables.contains_key(&key) {
            let table = match self.load_table_for_account(account, &name_str) {
                Ok(table) => table,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let storage = self.get_account_dir(account).unwrap_or_else(|| self.storage_dir.clone());
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

            if self.loaded_tables.len() >= self.max_loaded {
                if let Some(oldest_key) = self.lru_order.pop_front() {
                    let _ = self.save_table_for_account(&oldest_key.0, &oldest_key.1);
                    self.loaded_tables.remove(&oldest_key);
                }
            }
            self.loaded_tables.insert(key.clone(), table);
            self.lru_order.push_back(key.clone());
        } else {
            // Update LRU
            if let Some(pos) = self.lru_order.iter().position(|x| x == &key) {
                let n = self.lru_order.remove(pos).unwrap();
                self.lru_order.push_back(n);
            }
        }
        Ok(self.loaded_tables.get_mut(&key).unwrap())
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
    fn save_table_for_account(&mut self, account: &str, name: &str) -> io::Result<()> {
        let key = (account.to_string(), name.to_string());
        let storage = self.account_storage_dir(account);
        let per_group = self.records_per_group;
        let data_path = format!("{}/{}/data", storage, name);
        let dict_path = format!("{}/{}/dict", storage, name);

        // A file the caller marked durable is worth a real fsync: "flushed
        // before the write is acknowledged" has to mean on disk, not merely in
        // the page cache. Read from the cache rather than the DIR file so the
        // flush path stays free of I/O.
        let fsync = if self.durable_writes || self.durable_tables.get(&key).copied().unwrap_or(false) {
            self.durable_fsync
        } else {
            self.fsync
        };

        let table = match self.loaded_tables.get_mut(&key) {
            Some(table) if table.is_dirty() => table,
            _ => return Ok(()),
        };

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
    pub fn save(&mut self) -> io::Result<()> {
        let keys: Vec<(String, String)> = self.loaded_tables.keys().cloned().collect();
        let mut clients_updated = false;
        for (account, name) in keys {
            let was_dirty = self.loaded_tables.get(&(account.clone(), name.clone()))
                .map(|t| t.is_dirty())
                .unwrap_or(false);
            if account == "SYSTEM" && name == "$CLIENTS" && was_dirty {
                clients_updated = true;
            }
            if !was_dirty {
                // Never refresh the stamp of a table we did not write: doing so would
                // mark a snapshot that is already stale on disk as up to date and the
                // freshness check would stop reloading it.
                continue;
            }
            self.save_table_for_account(&account, &name)?;
            let stamp = self.disk_stamp(&account, &name);
            if let Some(t) = self.loaded_tables.get_mut(&(account, name)) {
                t.stamp = Some(stamp);
            }
        }
        self.pending_writes = 0;
        self.last_flush = Instant::now();
        if clients_updated {
            self.load_clients_from_table()?;
        }
        Ok(())
    }

    /// True while changes are held in memory and not yet on disk.
    pub fn has_pending_writes(&self) -> bool {
        self.loaded_tables.values().any(|t| t.is_dirty())
    }

    pub fn pending_write_count(&self) -> usize {
        self.pending_writes
    }

    /// Records that a write happened and flushes only when the batch is full
    /// or the flush interval has elapsed.
    ///
    /// This is the write path used by the server. Saving on every request meant
    /// one disk write per record even when a client streamed thousands of them;
    /// batching turns that into one write per group per interval. The cost is a
    /// bounded window (`flush_interval`) in which an acknowledged write is only
    /// in memory - set `durable_writes` to trade the throughput back for it.
    pub fn note_write(&mut self) -> io::Result<()> {
        if self.durable_writes {
            return self.save();
        }
        self.pending_writes += 1;
        if self.pending_writes >= self.flush_max_pending
            || self.last_flush.elapsed() >= self.flush_interval
        {
            return self.save();
        }
        Ok(())
    }

    /// Same as [`Database::note_write`], but honours the durability flag of the
    /// file that was written: a file marked durable in its account's DIR entry
    /// is flushed before the write is acknowledged, even when the rest of the
    /// database is buffering. This lets mission critical files opt out of the
    /// in-memory window without slowing everything else down.
    pub fn note_write_for(&mut self, account: &str, name: &str) -> io::Result<()> {
        if self.is_table_durable_for_account(account, name) {
            return self.save();
        }
        self.note_write()
    }

    /// Flushes if the interval has elapsed. Intended for a background ticker,
    /// so an idle server still persists the tail of a burst promptly.
    pub fn flush_if_due(&mut self) -> io::Result<bool> {
        if self.pending_writes == 0 && !self.has_pending_writes() {
            return Ok(false);
        }
        if self.last_flush.elapsed() < self.flush_interval {
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

    pub fn list_tables(&mut self) -> Vec<String> {
        let account = self.current_account.clone();
        self.list_tables_for_account(&account)
    }

    pub fn list_tables_for_account(&mut self, account: &str) -> Vec<String> {
        let _ = self.ensure_available_tables(account);
        let mut tables: Vec<_> = self.available_tables.get(account)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        tables.sort();
        tables
    }

    pub fn is_table_available(&mut self, name: &str) -> bool {
        let account = self.current_account.clone();
        let _ = self.ensure_available_tables(&account);
        self.available_tables.get(&account)
            .map(|s| s.contains(name))
            .unwrap_or(false)
    }

    pub fn is_table_loaded(&self, name: &str) -> bool {
        self.loaded_tables.contains_key(&(self.current_account.clone(), name.to_string()))
    }

    pub fn create_table(&mut self, name: &str) -> io::Result<()> {
        let account = self.current_account.clone();
        self.create_table_for_account(&account, name)
    }

    /// Creates a file and marks it durable, so every write to it is flushed
    /// before being acknowledged regardless of the global buffering settings.
    pub fn create_table_durable(&mut self, name: &str, durable: bool) -> io::Result<()> {
        let account = self.current_account.clone();
        self.create_table_for_account_durable(&account, name, durable)
    }

    pub fn create_table_for_account_durable(&mut self, account: &str, name: &str, durable: bool) -> io::Result<()> {
        self.create_table_for_account(account, name)?;
        if durable {
            // The flag lives in DIR, so an account without one gets it now
            // rather than silently losing the requested durability.
            self.ensure_dir_file_for_account(account)?;
            self.set_table_durable_for_account(account, name, true)?;
        }
        Ok(())
    }

    pub fn create_table_for_account(&mut self, account: &str, name: &str) -> io::Result<()> {
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

        let available = self.available_tables.get_mut(account).unwrap();
        if available.contains(name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("Table '{}' already exists", name)));
        }
        available.insert(name.to_string());

        // Update DIR file if it exists and this is not the DIR file itself
        if name != "DIR" && available.contains("DIR") {
            let _ = self.sync_dir_file_for_account(account);
        }

        // Set default dictionary for SYSTEM files
        if account == "SYSTEM" && (name.starts_with('$') || name == "DIR") {
            let _ = self.ensure_default_dictionaries(name);
        } else if name == "DIR" {
            // Every account's DIR describes the same two attributes, so name them
            // here too rather than only for SYSTEM.
            if let Ok(dir_table) = self.get_table_mut_for_account(account, "DIR") {
                Self::ensure_dir_dictionary(dir_table);
            }
        }

        Ok(())
    }

    pub fn delete_table(&mut self, name: &str) -> io::Result<()> {
        let account = self.current_account.clone();
        self.delete_table_for_account(&account, name)
    }

    pub fn delete_table_for_account(&mut self, account: &str, name: &str) -> io::Result<()> {
        if account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        self.ensure_available_tables(account)?;
        if !self.available_tables.get(account).unwrap().contains(name) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Table '{}' not found", name)));
        }

        let key = (account.to_string(), name.to_string());
        self.loaded_tables.remove(&key);
        self.durable_tables.remove(&key);
        self.available_tables.get_mut(account).unwrap().remove(name);
        if let Some(pos) = self.lru_order.iter().position(|x| x == &key) {
            self.lru_order.remove(pos);
        }

        let storage = self.account_storage_dir(account);
        let table_dir = format!("{}/{}", storage, name);
        let _ = fs::remove_dir_all(table_dir);

        Ok(())
    }

    pub fn sync_dir_file(&mut self) -> io::Result<()> {
        let account = self.current_account.clone();
        self.sync_dir_file_for_account(&account)
    }

    pub fn sync_dir_file_for_account(&mut self, account: &str) -> io::Result<()> {
        let tables = self.list_tables_for_account(account);
        let dir_table = self.get_table_mut_for_account(account, "DIR")?;
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

    pub fn set_table_durable(&mut self, name: &str, durable: bool) -> io::Result<()> {
        let account = self.current_account.clone();
        self.set_table_durable_for_account(&account, name, durable)
    }

    /// Records the per-file durability flag in the account's DIR file. The flag
    /// itself is metadata for mission critical files, so it is flushed at once.
    pub fn set_table_durable_for_account(&mut self, account: &str, name: &str, durable: bool) -> io::Result<()> {
        if account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        self.ensure_available_tables(account)?;
        if !self.available_tables.get(account).map(|s| s.contains("DIR")).unwrap_or(false) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "DIR file not found for account"));
        }
        let dir_table = self.get_table_mut_for_account(account, "DIR")?;
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
        Self::ensure_dir_dictionary(dir_table);
        self.durable_tables.insert((account.to_string(), name.to_string()), durable);
        self.save()
    }

    pub fn is_table_durable(&mut self, name: &str) -> bool {
        let account = self.current_account.clone();
        self.is_table_durable_for_account(&account, name)
    }

    /// True when this file must be flushed on every write, either because the
    /// whole database runs in durable mode or because its DIR entry says so.
    pub fn is_table_durable_for_account(&mut self, account: &str, name: &str) -> bool {
        if self.durable_writes {
            return true;
        }
        let key = (account.to_string(), name.to_string());
        if let Some(flag) = self.durable_tables.get(&key) {
            return *flag;
        }
        let has_dir = name != "DIR"
            && self.ensure_available_tables(account).is_ok()
            && self.available_tables.get(account).map(|s| s.contains("DIR")).unwrap_or(false);
        let flag = if has_dir {
            match self.get_table_mut_for_account(account, "DIR") {
                Ok(dir) => dir.records.get(name).map(Self::record_is_durable).unwrap_or(false),
                Err(_) => false,
            }
        } else {
            false
        };
        self.durable_tables.insert(key, flag);
        flag
    }

    /// Creates the account's DIR file if it does not have one yet.
    pub fn ensure_dir_file_for_account(&mut self, account: &str) -> io::Result<()> {
        self.ensure_available_tables(account)?;
        if self.available_tables.get(account).map(|s| s.contains("DIR")).unwrap_or(false) {
            return Ok(());
        }
        self.create_table_for_account(account, "DIR")?;
        self.sync_dir_file_for_account(account)
    }

    pub fn ensure_dir_file(&mut self) -> io::Result<bool> {
        let account = self.current_account.clone();
        self.ensure_available_tables(&account)?;
        if self.available_tables.get(&account).unwrap().contains("DIR") {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn create_dir_file(&mut self) -> io::Result<()> {
        self.create_table("DIR")?;
        self.sync_dir_file()
    }

    pub fn get_account_for_dir(&self, dir: &str) -> Option<String> {
        let names_field = self.accounts_config.fields.get(0)?;
        let dirs_field = self.accounts_config.fields.get(1)?;
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
        self.get_conversion_code_read_only_for_account(&self.current_account, table_name, field_name)
    }

    pub fn get_conversion_code_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<String> {
        self.get_table_read_only_for_account(account, table_name)?.conversion_code(field_name)
    }

    pub fn get_conversion_code(&mut self, table_name: &str, field_name: &str) -> Option<String> {
        let account = self.current_account.clone();
        self.get_conversion_code_read_only_for_account(&account, table_name, field_name)
    }

    pub fn get_field_header_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> String {
        let table = match self.get_table_read_only_for_account(account, table_name) {
            Some(t) => t,
            None => return field_name.to_string(),
        };
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
        let table = match self.get_table_read_only_for_account(account, table_name) {
            Some(t) => t,
            None => return 10,
        };
        if field_name == "ID" { return 10; }
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
        10
    }

    pub fn get_field_justification_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> String {
        let table = match self.get_table_read_only_for_account(account, table_name) {
            Some(t) => t,
            None => return "L".to_string(),
        };
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
        let table = match self.get_table_read_only_for_account(account, table_name) {
            Some(t) => t,
            None => return Vec::new(),
        };

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

    pub fn format_record_field(&self, table_name: &str, record: &Record, field_name: &str) -> String {
        let account = self.current_account.clone();
        self.format_record_field_for_account(&account, table_name, record, field_name)
    }

    pub fn format_record_field_for_account(&self, account: &str, table_name: &str, record: &Record, field_name: &str) -> String {
        let field_idx = match self.get_field_index_read_only_for_account(account, table_name, field_name) {
            Some(idx) => idx,
            None => return String::new(),
        };

        let raw_val = record.get_field_display_string(field_idx);
        let conv = self.get_conversion_code_read_only_for_account(account, table_name, field_name);

        if let Some(code) = conv {
            Self::apply_conversion(&raw_val, &code)
        } else {
            raw_val
        }
    }

    pub fn get_field_index_read_only(&self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_read_only_for_account(&self.current_account, table_name, field_name)
    }

    pub fn get_field_index_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" { return Some(0); }
        self.get_table_read_only_for_account(account, table_name)?.field_index(field_name)
    }

    pub fn get_field_index(&mut self, table_name: &str, field_name: &str) -> Option<usize> {
        let account = self.current_account.clone();
        self.get_field_index_for_account(&account, table_name, field_name)
    }

    pub fn get_field_index_for_account(&mut self, account: &str, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" { return Some(0); }
        let _ = self.get_table_mut_for_account(account, table_name).ok();
        self.get_field_index_read_only_for_account(account, table_name, field_name)
    }

    pub fn serialize_record(&self, table_name: &str, record: &Record) -> serde_json::Value {
        self.serialize_record_for_account(&self.current_account, table_name, record)
    }

    pub fn serialize_record_for_account(&self, account: &str, table_name: &str, record: &Record) -> serde_json::Value {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(table) => self.serialize_record_in(table, record),
            None => serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Serializes `record` using `table`, which the caller has already
    /// resolved. Spares the caller a second table lookup per record, which
    /// matters when serializing an entire result set.
    pub fn serialize_record_in(&self, table: &Table, record: &Record) -> serde_json::Value {
        let mut map = serde_json::Map::new();

        for (dict_key, dict_rec) in &table.dictionary {
            if let Some(f1) = dict_rec.fields.get(DICT_FIELD_IDX) {
                if let Some(v1) = f1.values.get(0) {
                    if let Some(idx_str) = v1.sub_values.get(0) {
                        if let Ok(idx) = idx_str.parse::<usize>() {
                            if idx > 0 {
                                let field_idx = idx - 1;
                                let raw_val = record.get_field_display_string(field_idx);
                                let value = match Table::conversion_code_from_dict_record(dict_rec) {
                                    Some(code) => Self::apply_conversion(&raw_val, &code),
                                    None => raw_val,
                                };
                                let camel_key = self.to_camel_case(dict_key);
                                map.insert(camel_key, serde_json::Value::String(value));
                            }
                        }
                    }
                }
            }
        }
        serde_json::Value::Object(map)
    }

    pub fn deserialize_record(&self, table_name: &str, data: &serde_json::Value) -> Option<Record> {
        self.deserialize_record_for_account(&self.current_account, table_name, data)
    }

    pub fn deserialize_record_for_account(&self, account: &str, table_name: &str, data: &serde_json::Value) -> Option<Record> {
        let obj = data.as_object()?;
        let mut record = Record::new();
        let table = self.get_table_read_only_for_account(account, table_name)?;

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

                                if let Some(code) = self.get_conversion_code_read_only_for_account(account, table_name, dict_key) {
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
                let val_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
                    _ => val.to_string(),
                };

                let final_val = if let Some(code) = conv_map.get(key) {
                    Self::apply_iconv(&val_str, code)
                } else {
                    val_str
                };

                record.fields[idx].values = vec![Value { sub_values: vec![final_val] }];
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

    pub fn log_error(&mut self, account: &str, message: &str) -> io::Result<()> {
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
                let table = db.get_table_mut("$LOGS")?;
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

    pub fn add_authorized_client(&mut self, name: &str, thumbprint: &str, allowed_accounts: Vec<String>, is_admin: bool) -> io::Result<()> {
        self.run_in_system_account(|db| {
            let thumbprint_lower = thumbprint.to_lowercase();

            // Update $CLIENTS table
            {
                let table = db.get_table_mut("$CLIENTS")?;
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

    pub fn add_client_account(&mut self, name: &str, account: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let mut success = false;
            {
                let table = db.get_table_mut("$CLIENTS")?;
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

    pub fn remove_client_account(&mut self, name: &str, account: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let mut success = false;
            {
                let table = db.get_table_mut("$CLIENTS")?;
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

    pub fn remove_authorized_client(&mut self, name: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let found = {
                let table = db.get_table_mut("$CLIENTS")?;
                if table.remove_record(name).is_some() {
                    true
                } else {
                    false
                }
            };

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
        for tp in &self.authorized_certs {
            certs_rec.fields[0].values.push(Value { sub_values: vec![tp.clone()] });
        }
        let mut map = HashMap::new();
        map.insert("certs".to_string(), certs_rec);
        Self::save_section(&map, &format!("{}/certs.reg", self.storage_dir))
    }

    pub fn create_test_account(&mut self, name: &str) -> io::Result<()> {
        let original_account = self.current_account.clone();
        self.create_account(name, None)?;
        self.logto(name)?;
        self.create_table("DIR")?;
        self.create_table("USERS")?;
        self.create_table("PRODUCTS")?;
        self.sync_dir_file()?;
        {
            let table = self.get_table_mut("USERS")?;
            table.dictionary.insert("NAME".to_string(), Record::from_display_string("1^NAME^L^15"));
            table.dictionary.insert("EMAIL".to_string(), Record::from_display_string("2^EMAIL^L^20"));
            table.records.insert("1".to_string(), Record::from_display_string("John Doe^john@example.com"));
            table.records.insert("2".to_string(), Record::from_display_string("Jane Smith^jane@example.com"));
            table.touch_all();
            table.mark_dict_dirty();
        }
        {
            let table = self.get_table_mut("PRODUCTS")?;
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
