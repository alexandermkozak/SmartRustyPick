use crate::db::models::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

pub struct Database {
    pub storage_dir: String,
    pub current_account: String,
    pub accounts_config: Record,
    pub loaded_tables: HashMap<String, Table>,
    pub available_tables: HashSet<String>,
    pub lru_order: VecDeque<String>,
    pub max_loaded: usize,
    pub active_select_list: Option<SelectList>,
    pub remote_select_lists: HashMap<String, SelectList>,
    pub remote_select_cursors: HashMap<String, usize>,
    pub authorized_certs: HashSet<String>,
    pub authorized_clients: HashMap<String, ClientInfo>,
    pub log_detail: String,
    pub max_log_records: usize,
}

impl Database {
    pub fn new(base_storage_dir: &str) -> io::Result<Self> {
        let config = crate::config::Config::load();
        let mut db = Database {
            storage_dir: base_storage_dir.to_string(),
            current_account: String::new(),
            accounts_config: Record::new(),
            loaded_tables: HashMap::new(),
            available_tables: HashSet::new(),
            lru_order: VecDeque::new(),
            max_loaded: 10,
            active_select_list: None,
            remote_select_lists: HashMap::new(),
            remote_select_cursors: HashMap::new(),
            authorized_certs: HashSet::new(),
            authorized_clients: HashMap::new(),
            log_detail: config.log_detail.unwrap_or_else(|| "normal".to_string()),
            max_log_records: config.max_log_records.unwrap_or(100),
        };

        if !Path::new(&db.storage_dir).exists() {
            fs::create_dir_all(&db.storage_dir)?;
        }

        // Load or create account registry
        let registry_path = format!("{}/accounts.reg", db.storage_dir);
        if Path::new(&registry_path).exists() {
            let mut map = HashMap::new();
            Self::load_section(&mut map, &registry_path)?;
            if let Some(reg_rec) = map.remove("registry") {
                db.accounts_config = reg_rec;
            }
        }

        // Ensure SYSTEM account exists
        if db.get_account_dir("SYSTEM").is_none() {
            db.create_account("SYSTEM", None)?;
        }

        // Explicitly log to SYSTEM to populate available_tables
        db.logto("SYSTEM")?;

        // Perform all system setup within a single account switch
        db.run_in_system_account(|db| {
            // Ensure DIR file exists for SYSTEM account
            if !db.available_tables.contains("DIR") {
                let _ = db.create_table("DIR");
                let _ = db.sync_dir_file();
            }

            // Ensure $LOGS file exists
            if !db.available_tables.contains("$LOGS") {
                let _ = db.create_table("$LOGS");
            }

            // Ensure $ACCOUNTS file exists
            if !db.available_tables.contains("$ACCOUNTS") {
                let _ = db.create_table("$ACCOUNTS");
            }
            // Populate $ACCOUNTS with all non-SYSTEM accounts
            let mut accounts_to_list = Vec::new();
            if let Some(names_field) = db.accounts_config.fields.get(0) {
                if let Some(dirs_field) = db.accounts_config.fields.get(1) {
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
            {
                let accounts_table = db.get_table_mut("$ACCOUNTS");
                for (name, dir) in accounts_to_list {
                    let mut record = Record::new();
                    record.fields.push(Field {
                        values: vec![Value { sub_values: vec![dir] }]
                    });
                    accounts_table.records.insert(name, record);
                }
                accounts_table.dirty = true;
            }

            // Ensure $CLIENTS file exists
            if !db.available_tables.contains("$CLIENTS") {
                let _ = db.create_table("$CLIENTS");
            }

            // Ensure $SAVEDLISTS file exists
            if !db.available_tables.contains("$SAVEDLISTS") {
                let _ = db.create_table("$SAVEDLISTS");
            }

            // One-time migration from certs.reg to $CLIENTS
            let certs_path = format!("{}/certs.reg", db.storage_dir);
            if Path::new(&certs_path).exists() {
                let mut map = HashMap::new();
                if Self::load_section(&mut map, &certs_path).is_ok() {
                    if let Some(certs_rec) = map.remove("certs") {
                        if let Some(f) = certs_rec.fields.get(0) {
                            let table = db.get_table_mut("$CLIENTS");
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
                                            rec.fields.push(Field { values: vec![Value { sub_values: vec![tp_lower] }] }); // Thumbprint
                                            rec.fields.push(Field { values: vec![] }); // No specific accounts
                                            rec.fields.push(Field { values: vec![Value { sub_values: vec!["Y".to_string()] }] }); // Admin by default for legacy certs
                                            table.records.insert(format!("migrated_{}", &sv[..8]), rec);
                                            table.dirty = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let _ = fs::rename(&certs_path, format!("{}.migrated", certs_path));
            }

            // Self-heal default dictionaries for all $ files and DIR
            let table_names: Vec<String> = db.available_tables.iter()
                .filter(|n| n.starts_with('$') || *n == "DIR")
                .cloned()
                .collect();

            let mut any_updated = false;
            for table_name in table_names {
                let mut updated = false;
                let table = db.get_table_mut(&table_name);
                match table_name.as_str() {
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
                    }
                    _ => {}
                }
                if updated {
                    table.dirty = true;
                    any_updated = true;
                }
            }

            // Populate in-memory structures from $CLIENTS
            {
                let table = db.get_table_mut("$CLIENTS");
                let mut clients = Vec::new();
                for record in table.records.values() {
                    if let Some(tp) = record.fields.get(0).and_then(|f| f.values.get(0)).and_then(|v| v.sub_values.get(0)) {
                        let tp_lower = tp.to_lowercase();
                        let mut allowed_accounts = Vec::new();
                        if let Some(acc_field) = record.fields.get(1) {
                            for v in &acc_field.values {
                                if let Some(acc) = v.sub_values.get(0) {
                                    if !acc.is_empty() {
                                        allowed_accounts.push(acc.clone());
                                    }
                                }
                            }
                        }
                        let is_admin = record.fields.get(2)
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
                db.authorized_clients.clear();
                db.authorized_certs.clear();
                for info in clients {
                    let tp = info.thumbprint.clone();
                    db.authorized_clients.insert(tp.clone(), info);
                    db.authorized_certs.insert(tp);
                }
            }
            if any_updated {
                db.save()?;
            }
            Ok(())
        })?;

        Ok(db)
    }

    pub fn run_in_system_account<F, R>(&mut self, f: F) -> io::Result<R>
    where
        F: FnOnce(&mut Database) -> io::Result<R>,
    {
        let original_account = self.current_account.clone();
        if original_account != "SYSTEM" {
            self.logto("SYSTEM")?;
        }
        let result = f(self);
        if original_account != "SYSTEM" {
            if original_account.is_empty() {
                self.current_account = String::new();
                self.loaded_tables.clear();
                self.available_tables.clear();
                self.lru_order.clear();
            } else {
                let _ = self.logto(&original_account);
            }
        }
        result
    }

    pub fn logto(&mut self, account_name: &str) -> io::Result<()> {
        let account_dir = self.get_account_dir(account_name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)))?;

        self.save()?; // Save current account's dirty tables
        self.current_account = account_name.to_string();
        self.loaded_tables.clear();
        self.lru_order.clear();
        self.available_tables.clear();

        // Populate available tables
        if let Ok(entries) = fs::read_dir(&account_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        self.available_tables.insert(name.to_string());
                    }
                }
            }
        }

        Ok(())
    }

    pub fn create_account(&mut self, name: &str, directory: Option<&str>) -> io::Result<()> {
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

        // Persist registry
        let mut map = HashMap::new();
        map.insert("registry".to_string(), self.accounts_config.clone());
        Self::save_section(&map, &format!("{}/accounts.reg", self.storage_dir))?;

        // Update $ACCOUNTS table if it exists
        self.run_in_system_account(|db| {
            if db.available_tables.contains("$ACCOUNTS") {
                let accounts_table = db.get_table_mut("$ACCOUNTS");
                let mut record = Record::new();
                record.fields.push(Field {
                    values: vec![Value { sub_values: vec![dir] }]
                });
                accounts_table.records.insert(name.to_string(), record);
                accounts_table.dirty = true;
                db.save()?;
            }
            Ok(())
        })?;

        if !prev_acc.is_empty() && prev_acc != "SYSTEM" {
            let _ = self.logto(&prev_acc);
        } else if prev_acc.is_empty() {
            self.current_account = String::new();
            self.loaded_tables.clear();
            self.available_tables.clear();
            self.lru_order.clear();
        }
        Ok(())
    }

    pub fn delete_account(&mut self, name: &str) -> io::Result<()> {
        if name == "SYSTEM" {
            return Err(io::Error::new(io::ErrorKind::Other, "Cannot delete SYSTEM account"));
        }

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
        let mut map = HashMap::new();
        map.insert("registry".to_string(), self.accounts_config.clone());
        Self::save_section(&map, &format!("{}/accounts.reg", self.storage_dir))?;

        // Remove from $ACCOUNTS table
        self.run_in_system_account(|db| {
            let table = db.get_table_mut("$ACCOUNTS");
            table.records.remove(name);
            table.dirty = true;
            db.save()
        })?;

        // Delete physical directory
        let _ = fs::remove_dir_all(dir);

        if self.current_account == name {
            self.current_account = String::new();
            self.loaded_tables.clear();
            self.available_tables.clear();
            self.lru_order.clear();
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
        self.loaded_tables.get(name)
    }

    pub fn get_table(&mut self, name: &str) -> Option<&Table> {
        if !self.available_tables.contains(name) {
            return None;
        }

        if !self.loaded_tables.contains_key(name) {
            if let Ok(table) = self.load_table(name) {
                if self.loaded_tables.len() >= self.max_loaded {
                    if let Some(oldest) = self.lru_order.pop_front() {
                        let _ = self.save_table(&oldest);
                        self.loaded_tables.remove(&oldest);
                    }
                }
                self.loaded_tables.insert(name.to_string(), table);
                self.lru_order.push_back(name.to_string());
            } else {
                return None;
            }
        } else {
            // Update LRU
            if let Some(pos) = self.lru_order.iter().position(|x| x == name) {
                let n = self.lru_order.remove(pos).unwrap();
                self.lru_order.push_back(n);
            }
        }

        self.loaded_tables.get(name)
    }

    pub fn get_table_mut(&mut self, name: &str) -> &mut Table {
        if !self.loaded_tables.contains_key(name) {
            if let Ok(table) = self.load_table(name) {
                if self.loaded_tables.len() >= self.max_loaded {
                    if let Some(oldest) = self.lru_order.pop_front() {
                        let _ = self.save_table(&oldest);
                        self.loaded_tables.remove(&oldest);
                    }
                }
                self.loaded_tables.insert(name.to_string(), table);
                self.lru_order.push_back(name.to_string());
            } else {
                // Return a fresh table if load fails
                let storage = self.current_storage_dir();
                let table_dir = format!("{}/{}", storage, name);
                if !Path::new(&table_dir).exists() {
                    let _ = fs::create_dir_all(&table_dir);
                    let _ = File::create(format!("{}/data", table_dir));
                    let _ = File::create(format!("{}/dict", table_dir));
                    self.available_tables.insert(name.to_string());
                }

                self.loaded_tables.insert(name.to_string(), Table::new());
                self.lru_order.push_back(name.to_string());
            }
        } else {
            // Update LRU
            if let Some(pos) = self.lru_order.iter().position(|x| x == name) {
                let n = self.lru_order.remove(pos).unwrap();
                self.lru_order.push_back(n);
            }
        }
        self.loaded_tables.get_mut(name).unwrap()
    }

    fn load_table(&self, name: &str) -> io::Result<Table> {
        let storage = self.current_storage_dir();
        let mut table = Table::new();
        Self::load_section(&mut table.records, &format!("{}/{}/data", storage, name))?;
        Self::load_section(&mut table.dictionary, &format!("{}/{}/dict", storage, name))?;
        Ok(table)
    }

    fn save_table(&self, name: &str) -> io::Result<()> {
        if let Some(table) = self.loaded_tables.get(name) {
            if table.dirty {
                let storage = self.current_storage_dir();
                Self::save_section(&table.records, &format!("{}/{}/data", storage, name))?;
                Self::save_section(&table.dictionary, &format!("{}/{}/dict", storage, name))?;
            }
        }
        Ok(())
    }

    pub fn save(&mut self) -> io::Result<()> {
        let names: Vec<String> = self.loaded_tables.keys().cloned().collect();
        for name in names {
            self.save_table(&name)?;
            if let Some(t) = self.loaded_tables.get_mut(&name) {
                t.dirty = false;
            }
        }
        Ok(())
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
        let mut tables: Vec<_> = self.available_tables.iter().cloned().collect();
        tables.sort();
        tables
    }

    pub fn create_table(&mut self, name: &str) -> io::Result<()> {
        if self.current_account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        if self.available_tables.contains(name) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("Table '{}' already exists", name)));
        }

        let storage = self.current_storage_dir();
        let table_dir = format!("{}/{}", storage, name);
        if !Path::new(&table_dir).exists() {
            fs::create_dir_all(&table_dir)?;
        }
        File::create(format!("{}/data", table_dir))?;
        File::create(format!("{}/dict", table_dir))?;

        self.available_tables.insert(name.to_string());

        // Update DIR file if it exists and this is not the DIR file itself
        if name != "DIR" && self.available_tables.contains("DIR") {
            let _ = self.sync_dir_file();
        }

        // Set default dictionary for SYSTEM files
        if self.current_account == "SYSTEM" && name.starts_with('$') {
            let mut updated = false;
            let table = self.get_table_mut(name);
            match name {
                "$LOGS" => {
                    table.dictionary.insert("MESSAGE".to_string(), Record::from_display_string("1^MESSAGE^L^60"));
                    table.dictionary.insert("DETAIL".to_string(), Record::from_display_string("2^DETAIL^L^40"));
                    updated = true;
                }
                "$ACCOUNTS" => {
                    table.dictionary.insert("PATH".to_string(), Record::from_display_string("1^PATH^L^50"));
                    updated = true;
                }
                "$CLIENTS" => {
                    table.dictionary.insert("THUMBPRINT".to_string(), Record::from_display_string("1^THUMBPRINT^L^64"));
                    table.dictionary.insert("ACCOUNTS".to_string(), Record::from_display_string("2^ACCOUNTS^L^30"));
                    table.dictionary.insert("ADMIN".to_string(), Record::from_display_string("3^ADMIN^L^5"));
                    updated = true;
                }
                "$SAVEDLISTS" => {
                    table.dictionary.insert("TABLE".to_string(), Record::from_display_string("1^TABLE^L^20"));
                    table.dictionary.insert("IS_DICT".to_string(), Record::from_display_string("2^IS_DICT^L^1"));
                    updated = true;
                }
                _ => {}
            }
            if updated {
                table.dirty = true;
            }
        } else if name == "DIR" {
            let table = self.get_table_mut(name);
            table.dictionary.insert("TYPE".to_string(), Record::from_display_string("1^TYPE^L^1"));
            table.dirty = true;
        }

        Ok(())
    }

    pub fn delete_table(&mut self, name: &str) -> io::Result<()> {
        if self.current_account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        if !self.available_tables.contains(name) {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Table '{}' not found", name)));
        }

        self.loaded_tables.remove(name);
        self.available_tables.remove(name);
        if let Some(pos) = self.lru_order.iter().position(|x| x == name) {
            self.lru_order.remove(pos);
        }

        let storage = self.current_storage_dir();
        let table_dir = format!("{}/{}", storage, name);
        let _ = fs::remove_dir_all(table_dir);

        Ok(())
    }

    pub fn sync_dir_file(&mut self) -> io::Result<()> {
        let tables = self.list_tables();
        let dir_table = self.get_table_mut("DIR");
        dir_table.records.clear();
        for t in tables {
            if t != "DIR" {
                let mut rec = Record::new();
                rec.fields.push(Field {
                    values: vec![Value { sub_values: vec!["F".to_string()] }]
                });
                dir_table.records.insert(t, rec);
            }
        }
        dir_table.dirty = true;
        Ok(())
    }

    pub fn ensure_dir_file(&mut self) -> io::Result<bool> {
        if self.available_tables.contains("DIR") {
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
        let table = self.get_table_read_only(table_name)?;
        if let Some(rec) = table.dictionary.get(field_name) {
            // Pick MDn conversion is in Field 8
            if let Some(f8) = rec.fields.get(7) {
                if let Some(v) = f8.values.get(0) {
                    let code: &String = v.sub_values.get(0)?;
                    if !code.is_empty() {
                        return Some(code.clone());
                    }
                }
            }
        }
        None
    }

    pub fn get_conversion_code(&mut self, table_name: &str, field_name: &str) -> Option<String> {
        self.get_conversion_code_read_only(table_name, field_name)
    }

    pub fn apply_conversion(val: &str, code: &str) -> String {
        if code.starts_with("MD") && code.len() > 2 {
            if let Ok(decimals) = code[2..].parse::<usize>() {
                if let Ok(num) = val.parse::<i64>() {
                    let divisor = 10f64.powi(decimals as i32);
                    let mut s = format!("{:.width$}", num as f64 / divisor, width = decimals);
                    if decimals == 0 {
                        s = format!("{}", num);
                    }
                    return s;
                }
            }
        }
        val.to_string()
    }

    pub fn format_record_field(&self, table_name: &str, record: &Record, field_name: &str) -> String {
        let field_idx = match self.get_field_index_read_only(table_name, field_name) {
            Some(idx) => idx,
            None => return String::new(),
        };

        let raw_val = record.get_field_display_string(field_idx);
        let conv = self.get_conversion_code_read_only(table_name, field_name);

        if let Some(code) = conv {
            Self::apply_conversion(&raw_val, &code)
        } else {
            raw_val
        }
    }

    pub fn get_field_index_read_only(&self, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" { return Some(0); }
        let table = self.get_table_read_only(table_name)?;
        if let Some(rec) = table.dictionary.get(field_name) {
            if let Some(f1) = rec.fields.get(0) {
                if let Some(v1) = f1.values.get(0) {
                    let idx_str: &String = v1.sub_values.get(0)?;
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        // Pick attribute 1 is 0-indexed 0 in our internal fields vector
                        if idx > 0 {
                            return Some(idx - 1);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn get_field_index(&mut self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_read_only(table_name, field_name)
    }

    pub fn log_error(&mut self, account: &str, message: &str) -> io::Result<()> {
        self.run_in_system_account(|db| {
            let now = time::OffsetDateTime::now_utc();
            let date_str = format!("{:04}{:02}{:02}", now.year(), now.month() as u8, now.day());
            let time_str = format!("{:02}{:02}{:02}", now.hour(), now.minute(), now.second());
            // Add a microsecond component to ensure key uniqueness during fast tests
            let key = format!("{}*{}*{}*{}", date_str, time_str, now.microsecond(), account);

            let mut record = Record::new();
            // Field 1: Message
            record.fields.push(Field {
                values: vec![Value { sub_values: vec![message.to_string()] }]
            });

            // Field 2: Detail
            if db.log_detail == "detailed" {
                record.fields.push(Field {
                    values: vec![Value { sub_values: vec![format!("UTC: {}", now)] }]
                });
            }

            let max_records = db.max_log_records;
            {
                let table = db.get_table_mut("$LOGS");
                table.records.insert(key, record);
                table.dirty = true;

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
                let table = db.get_table_mut("$CLIENTS");
                let mut record = Record::new();
                // Field 0: Thumbprint
                record.fields.push(Field {
                    values: vec![Value { sub_values: vec![thumbprint_lower.clone()] }]
                });
                // Field 1: Allowed Accounts
                let mut accounts_field = Field::default();
                for acc in &allowed_accounts {
                    accounts_field.values.push(Value { sub_values: vec![acc.clone()] });
                }
                record.fields.push(accounts_field);
                // Field 2: Admin flag
                record.fields.push(Field {
                    values: vec![Value { sub_values: vec![if is_admin { "Y".to_string() } else { "".to_string() }] }]
                });

                table.records.insert(name.to_string(), record);
                table.dirty = true;
            }
            db.save()?;

            // Update in-memory structures
            db.authorized_clients.insert(thumbprint_lower.clone(), ClientInfo {
                thumbprint: thumbprint_lower.clone(),
                allowed_accounts,
                is_admin,
            });
            db.authorized_certs.insert(thumbprint_lower);

            // Sync with certs.reg for backward compatibility (optional but safe)
            db.save_certs()
        })
    }

    pub fn add_client_account(&mut self, name: &str, account: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let mut thumbprint = None;
            let mut success = false;
            let mut existing_accounts = Vec::new();
            let mut is_admin = false;
            {
                let table = db.get_table_mut("$CLIENTS");
                if let Some(record) = table.records.get_mut(name) {
                    // Get thumbprint for updating in-memory map later
                    thumbprint = record.fields.get(0)
                        .and_then(|f| f.values.get(0))
                        .and_then(|v| v.sub_values.get(0))
                        .cloned();

                    // Ensure Field 1 exists
                    while record.fields.len() <= 1 {
                        record.fields.push(Field::default());
                    }

                    // Check if account already exists in Field 1
                    let already_exists = record.fields[1].values.iter().any(|v| v.sub_values.get(0) == Some(&account.to_string()));

                    if !already_exists {
                        record.fields[1].values.push(Value { sub_values: vec![account.to_string()] });
                        table.dirty = true;
                        success = true;
                    }

                    // Collect all accounts for in-memory update
                    for v in &record.fields[1].values {
                        if let Some(acc) = v.sub_values.get(0) {
                            existing_accounts.push(acc.clone());
                        }
                    }

                    is_admin = record.fields.get(2)
                        .and_then(|f| f.values.get(0))
                        .and_then(|v| v.sub_values.get(0))
                        .map(|s| s == "Y")
                        .unwrap_or(false);
                }
            }

            if success {
                db.save()?;
                if let Some(tp) = thumbprint {
                    let tp_lower = tp.to_lowercase();
                    db.authorized_clients.insert(tp_lower.clone(), ClientInfo {
                        thumbprint: tp_lower,
                        allowed_accounts: existing_accounts,
                        is_admin,
                    });
                }
            }

            Ok(success)
        })
    }

    pub fn remove_client_account(&mut self, name: &str, account: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let mut thumbprint = None;
            let mut success = false;
            let mut existing_accounts = Vec::new();
            let mut is_admin = false;
            {
                let table = db.get_table_mut("$CLIENTS");
                if let Some(record) = table.records.get_mut(name) {
                    thumbprint = record.fields.get(0)
                        .and_then(|f| f.values.get(0))
                        .and_then(|v| v.sub_values.get(0))
                        .cloned();

                    if record.fields.len() > 1 {
                        let original_len = record.fields[1].values.len();
                        record.fields[1].values.retain(|v| v.sub_values.get(0).map(|s| s != account).unwrap_or(true));

                        if record.fields[1].values.len() < original_len {
                            table.dirty = true;
                            success = true;
                        }

                        for v in &record.fields[1].values {
                            if let Some(acc) = v.sub_values.get(0) {
                                existing_accounts.push(acc.clone());
                            }
                        }
                    }

                    is_admin = record.fields.get(2)
                        .and_then(|f| f.values.get(0))
                        .and_then(|v| v.sub_values.get(0))
                        .map(|s| s == "Y")
                        .unwrap_or(false);
                }
            }

            if success {
                db.save()?;
                if let Some(tp) = thumbprint {
                    let tp_lower = tp.to_lowercase();
                    db.authorized_clients.insert(tp_lower.clone(), ClientInfo {
                        thumbprint: tp_lower,
                        allowed_accounts: existing_accounts,
                        is_admin,
                    });
                }
            }
            Ok(success)
        })
    }

    pub fn remove_authorized_client(&mut self, name: &str) -> io::Result<bool> {
        self.run_in_system_account(|db| {
            let mut removed_thumbprint = None;
            let found = {
                let table = db.get_table_mut("$CLIENTS");
                if let Some(record) = table.records.remove(name) {
                    if let Some(f) = record.fields.get(0) {
                        if let Some(v) = f.values.get(0) {
                            if let Some(tp) = v.sub_values.get(0) {
                                removed_thumbprint = Some(tp.clone());
                            }
                        }
                    }
                    table.dirty = true;
                    true
                } else {
                    false
                }
            };

            if found {
                db.save()?;
                if let Some(tp) = removed_thumbprint {
                    let tp_lower = tp.to_lowercase();
                    db.authorized_certs.remove(&tp_lower);
                    db.authorized_clients.remove(&tp_lower);
                    let _ = db.save_certs();
                }
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
            let table = self.get_table_mut("USERS");
            table.dictionary.insert("NAME".to_string(), Record::from_display_string("1^NAME^L^15"));
            table.dictionary.insert("EMAIL".to_string(), Record::from_display_string("2^EMAIL^L^20"));
            table.records.insert("1".to_string(), Record::from_display_string("John Doe^john@example.com"));
            table.records.insert("2".to_string(), Record::from_display_string("Jane Smith^jane@example.com"));
            table.dirty = true;
        }
        {
            let table = self.get_table_mut("PRODUCTS");
            table.dictionary.insert("DESC".to_string(), Record::from_display_string("1^DESCRIPTION^L^20"));
            table.dictionary.insert("PRICE".to_string(), Record::from_display_string("2^PRICE^R^10^^^^MD2"));
            table.records.insert("P1".to_string(), Record::from_display_string("Laptop^120000"));
            table.records.insert("P2".to_string(), Record::from_display_string("Mouse^2500"));
            table.dirty = true;
        }
        self.save()?;
        if !original_account.is_empty() {
            let _ = self.logto(&original_account);
        } else {
            self.current_account = String::new();
            self.loaded_tables.clear();
            self.available_tables.clear();
            self.lru_order.clear();
        }
        Ok(())
    }
}
