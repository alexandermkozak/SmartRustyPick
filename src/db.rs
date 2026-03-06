use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

pub const FM: u8 = 254; // Field Mark
pub const VM: u8 = 253; // Value Mark
pub const SVM: u8 = 252; // Sub-Value Mark

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Value {
    pub sub_values: Vec<String>,
}

#[allow(dead_code)]
impl Record {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_bytes(data: &[u8]) -> Self {
        if data.is_empty() {
            return Record { fields: vec![] };
        }
        let fields = data.split(|&b| b == FM)
            .map(|f| {
                let values = f.split(|&b| b == VM)
                    .map(|v| {
                        let sub_values = v.split(|&b| b == SVM)
                            .map(|sv| String::from_utf8_lossy(sv).to_string())
                            .collect();
                        Value { sub_values }
                    })
                    .collect();
                Field { values }
            })
            .collect();
        Record { fields }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut res = Vec::new();
        for (i, f) in self.fields.iter().enumerate() {
            if i > 0 { res.push(FM); }
            for (j, v) in f.values.iter().enumerate() {
                if j > 0 { res.push(VM); }
                for (k, sv) in v.sub_values.iter().enumerate() {
                    if k > 0 { res.push(SVM); }
                    res.extend_from_slice(sv.as_bytes());
                }
            }
        }
        res
    }

    // Keep to_string/from_string for compatibility with existing tests/code if needed
    // but they will use the byte-based implementation under the hood
    #[allow(dead_code)]
    pub fn from_string(data: &str) -> Self {
        Self::from_bytes(data.as_bytes())
    }

    #[allow(dead_code)]
    pub fn to_string(&self) -> String {
        String::from_utf8_lossy(&self.to_bytes()).to_string()
    }

    pub fn to_display_string(&self) -> String {
        let display_bytes: Vec<u8> = self.to_bytes().iter().map(|&b| match b {
            FM => b'^',
            VM => b']',
            SVM => b'\\',
            _ => b
        }).collect();
        String::from_utf8_lossy(&display_bytes).to_string()
    }

    pub fn from_display_string(s: &str) -> Self {
        let translated_data: Vec<u8> = s.as_bytes().iter().map(|&b| match b {
            b'^' => FM,
            b']' => VM,
            b'\\' => SVM,
            _ => b
        }).collect();
        Self::from_bytes(&translated_data)
    }

    pub fn to_edit_string(&self) -> String {
        let display_bytes: Vec<u8> = self.to_bytes().iter().map(|&b| match b {
            FM => b'\n',
            VM => b']',
            SVM => b'\\',
            _ => b
        }).collect();
        String::from_utf8_lossy(&display_bytes).to_string()
    }

    pub fn from_edit_string(s: &str) -> Self {
        let mut content = s;
        if content.ends_with('\n') {
            content = &content[..content.len() - 1];
        }
        if content.ends_with('\r') {
            content = &content[..content.len() - 1];
        }

        let translated_data: Vec<u8> = content.as_bytes().iter().filter(|&&b| b != b'\r').map(|&b| match b {
            b'\n' => FM,
            b']' => VM,
            b'\\' => SVM,
            _ => b
        }).collect();
        Self::from_bytes(&translated_data)
    }

    pub fn get_field_display_string(&self, field_idx: usize) -> String {
        if let Some(field) = self.fields.get(field_idx) {
            let mut res = Vec::new();
            for (j, v) in field.values.iter().enumerate() {
                if j > 0 { res.push(VM); }
                for (k, sv) in v.sub_values.iter().enumerate() {
                    if k > 0 { res.push(SVM); }
                    res.extend_from_slice(sv.as_bytes());
                }
            }
            let display_bytes: Vec<u8> = res.iter().map(|&b| match b {
                VM => b']',
                SVM => b'\\',
                _ => b
            }).collect();
            String::from_utf8_lossy(&display_bytes).to_string()
        } else {
            String::new()
        }
    }
}

pub struct Table {
    pub records: HashMap<String, Record>,
    pub dictionary: HashMap<String, Record>,
    pub dirty: bool,
}

#[derive(Clone, Debug)]
pub struct SelectList {
    pub table_name: String,
    pub is_dict: bool,
    pub keys: Vec<String>,
}

impl Table {
    pub fn new() -> Self {
        Table {
            records: HashMap::new(),
            dictionary: HashMap::new(),
            dirty: false,
        }
    }
}

pub struct Database {
    pub storage_dir: String,
    pub current_account: String,
    pub accounts_config: Record, // Map account name to its directory path
    loaded_tables: HashMap<String, Table>,
    available_tables: HashSet<String>,
    lru_order: VecDeque<String>,
    max_loaded: usize,
    pub active_select_list: Option<SelectList>,
    pub authorized_certs: HashSet<String>, // Set of SHA-256 thumbprints
}

impl Database {
    pub fn new(base_storage_dir: &str) -> io::Result<Self> {
        let mut db = Database {
            storage_dir: base_storage_dir.to_string(),
            current_account: String::new(),
            accounts_config: Record::new(),
            loaded_tables: HashMap::new(),
            available_tables: HashSet::new(),
            lru_order: VecDeque::new(),
            max_loaded: 10,
            active_select_list: None,
            authorized_certs: HashSet::new(),
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

        // Load authorized certificates
        let certs_path = format!("{}/certs.reg", db.storage_dir);
        if Path::new(&certs_path).exists() {
            let mut map = HashMap::new();
            Self::load_section(&mut map, &certs_path)?;
            if let Some(certs_rec) = map.remove("certs") {
                if let Some(f) = certs_rec.fields.get(0) {
                    for v in &f.values {
                        for sv in &v.sub_values {
                            db.authorized_certs.insert(sv.clone());
                        }
                    }
                }
            }
        }
        
        Ok(db)
    }

    pub fn save_certs(&self) -> io::Result<()> {
        let mut certs_rec = Record::new();
        let mut field = Field::default();
        for thumbprint in &self.authorized_certs {
            field.values.push(Value { sub_values: vec![thumbprint.clone()] });
        }
        certs_rec.fields.push(field);

        let mut map = HashMap::new();
        map.insert("certs".to_string(), certs_rec);
        let certs_path = format!("{}/certs.reg", self.storage_dir);
        Self::save_section(&certs_path, &map)?;
        Ok(())
    }

    pub fn save_registry(&self) -> io::Result<()> {
        let mut map = HashMap::new();
        map.insert("registry".to_string(), self.accounts_config.clone());
        let registry_path = format!("{}/accounts.reg", self.storage_dir);
        Self::save_section(&registry_path, &map)?;
        Ok(())
    }

    pub fn logto(&mut self, account_name: &str) -> io::Result<()> {
        // If switching accounts, flush current loaded tables
        self.save()?;
        self.loaded_tables.clear();
        self.available_tables.clear();
        self.lru_order.clear();
        self.active_select_list = None;

        let account_dir = if let Some(dir) = self.get_account_dir(account_name) {
            dir
        } else {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", account_name)));
        };

        self.current_account = account_name.to_string();
        self.init_available_in_dir(&account_dir)?;

        if !self.available_tables.contains("$SAVEDLISTS") {
            self.available_tables.insert("$SAVEDLISTS".to_string());
        }

        Ok(())
    }

    pub fn ensure_dir_file(&mut self) -> io::Result<bool> {
        if self.current_account.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other, "Not logged into an account"));
        }
        if self.available_tables.contains("DIR") {
            return Ok(true);
        }
        Ok(false)
    }

    pub fn create_dir_file(&mut self) -> io::Result<()> {
        self.create_table("DIR")?;
        self.sync_dir_file()?;
        Ok(())
    }

    pub fn sync_dir_file(&mut self) -> io::Result<()> {
        let tables = self.list_tables();
        let dir_table = self.get_table_mut("DIR");

        for table_name in tables {
            if table_name == "DIR" { continue; }
            if !dir_table.records.contains_key(&table_name) {
                let mut record = Record::new();
                record.fields.push(Field {
                    values: vec![Value { sub_values: vec!["F".to_string()] }]
                });
                dir_table.records.insert(table_name, record);
                dir_table.dirty = true;
            }
        }
        self.save()?;
        Ok(())
    }

    fn get_account_dir(&self, account_name: &str) -> Option<String> {
        // Search in accounts_config (Record: fields correspond to accounts)
        // Let's say field 1 contains account names and field 2 contains their directories.
        // Actually, easier: field 1 is names, field 2 is directories.
        let names_field = self.accounts_config.fields.get(0)?;
        let dirs_field = self.accounts_config.fields.get(1)?;

        for (i, v) in names_field.values.iter().enumerate() {
            if let Some(name) = v.sub_values.get(0) {
                if name == account_name {
                    return dirs_field.values.get(i)?.sub_values.get(0).cloned();
                }
            }
        }
        None
    }

    pub fn create_account(&mut self, name: &str, directory: Option<&str>) -> io::Result<()> {
        if self.get_account_dir(name).is_some() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("Account '{}' already exists", name)));
        }

        let dir = directory.map(|d| d.to_string())
            .unwrap_or_else(|| format!("{}/{}", self.storage_dir, name));

        if !Path::new(&dir).exists() {
            fs::create_dir_all(&dir)?;
        }

        // Add to registry
        if self.accounts_config.fields.is_empty() {
            self.accounts_config.fields.push(Field::default()); // Names
            self.accounts_config.fields.push(Field::default()); // Dirs
        }

        self.accounts_config.fields[0].values.push(Value { sub_values: vec![name.to_string()] });
        self.accounts_config.fields[1].values.push(Value { sub_values: vec![dir] });

        self.save_registry()?;

        // Initialize DIR file for the new account
        let prev_account = self.current_account.clone();
        if self.logto(name).is_ok() {
            let _ = self.create_dir_file();
            // Restore previous account context if any
            if !prev_account.is_empty() {
                let _ = self.logto(&prev_account);
            } else {
                self.current_account = String::new();
                self.loaded_tables.clear();
                self.available_tables.clear();
                self.lru_order.clear();
            }
        }

        Ok(())
    }

    pub fn delete_account(&mut self, name: &str) -> io::Result<()> {
        let dir = if let Some(d) = self.get_account_dir(name) {
            d
        } else {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("Account '{}' not found", name)));
        };

        // If current account is the one being deleted, log out
        if self.current_account == name {
            self.current_account = String::new();
            self.loaded_tables.clear();
            self.available_tables.clear();
            self.lru_order.clear();
        }

        // Remove from registry
        let mut idx_to_remove = None;
        {
            let names_field = &self.accounts_config.fields[0];
            for (i, v) in names_field.values.iter().enumerate() {
                if let Some(acc_name) = v.sub_values.get(0) {
                    if acc_name == name {
                        idx_to_remove = Some(i);
                        break;
                    }
                }
            }
        }

        if let Some(i) = idx_to_remove {
            self.accounts_config.fields[0].values.remove(i);
            self.accounts_config.fields[1].values.remove(i);
        }

        // Delete files
        let _ = fs::remove_dir_all(dir);

        self.save_registry()?;
        Ok(())
    }


    fn init_available_in_dir(&mut self, dir: &str) -> io::Result<()> {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)?;
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name() {
                    self.available_tables.insert(name.to_string_lossy().to_string());
                }
            }
        }
        Ok(())
    }

    fn current_storage_dir(&self) -> String {
        self.get_account_dir(&self.current_account).unwrap_or_else(|| self.storage_dir.clone())
    }

    fn mark_used(&mut self, name: &str) {
        if let Some(pos) = self.lru_order.iter().position(|x| x == name) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push_front(name.to_string());
        
        if self.lru_order.len() > self.max_loaded {
            let to_evict = self.lru_order.pop_back().unwrap();
            if let Some(table) = self.loaded_tables.remove(&to_evict) {
                if table.dirty {
                    let _ = Self::flush_table_internal(&self.current_storage_dir(), &to_evict, &table);
                }
            }
        }
    }

    fn load_table_into(&self, name: &str, table: &mut Table) -> io::Result<()> {
        let storage = self.current_storage_dir();
        Self::load_section(&mut table.records, &format!("{}/{}/data", storage, name))?;
        Self::load_section(&mut table.dictionary, &format!("{}/{}/dict", storage, name))?;
        table.dirty = false;
        Ok(())
    }

    fn flush_table_internal(storage_dir: &str, name: &str, table: &Table) -> io::Result<()> {
        let table_dir = format!("{}/{}", storage_dir, name);
        if !Path::new(&table_dir).exists() {
            fs::create_dir_all(&table_dir)?;
        }
        Self::save_section(&format!("{}/data", table_dir), &table.records)?;
        Self::save_section(&format!("{}/dict", table_dir), &table.dictionary)?;
        Ok(())
    }

    pub fn get_table_mut(&mut self, name: &str) -> &mut Table {
        if !self.loaded_tables.contains_key(name) {
            let mut table = Table::new();
            let _ = self.load_table_into(name, &mut table);
            self.loaded_tables.insert(name.to_string(), table);
            self.available_tables.insert(name.to_string());
        }
        self.mark_used(name);
        self.loaded_tables.get_mut(name).unwrap()
    }

    pub fn get_table(&mut self, name: &str) -> Option<&Table> {
        if !self.loaded_tables.contains_key(name) {
            if !self.available_tables.contains(name) {
                return None;
            }
            let mut table = Table::new();
            if self.load_table_into(name, &mut table).is_err() {
                return None;
            }
            self.loaded_tables.insert(name.to_string(), table);
        }
        self.mark_used(name);
        self.loaded_tables.get(name)
    }

    pub fn save(&mut self) -> io::Result<()> {
        let storage = self.current_storage_dir();
        if !Path::new(&storage).exists() {
            fs::create_dir_all(&storage)?;
        }
        
        let mut dirty_names = Vec::new();
        for (name, table) in &self.loaded_tables {
            if table.dirty {
                dirty_names.push(name.clone());
            }
        }

        for name in dirty_names {
            if let Some(table) = self.loaded_tables.get_mut(&name) {
                Self::flush_table_internal(&storage, &name, table)?;
                table.dirty = false;
            }
        }
        Ok(())
    }

    fn save_section(path: &str, records: &HashMap<String, Record>) -> io::Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        for (key, record) in records {
            let key_bytes = key.as_bytes();
            let data_bytes = record.to_bytes();
            writer.write_all(&(key_bytes.len() as u32).to_le_bytes())?;
            writer.write_all(key_bytes)?;
            writer.write_all(&(data_bytes.len() as u32).to_le_bytes())?;
            writer.write_all(&data_bytes)?;
        }
        writer.flush()?;
        Ok(())
    }

    fn load_section(map: &mut HashMap<String, Record>, path: &str) -> io::Result<()> {
        if !Path::new(path).exists() { return Ok(()); }
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        loop {
            let mut len_buf = [0u8; 4];
            if reader.read_exact(&mut len_buf).is_err() { break; }
            let key_len = u32::from_le_bytes(len_buf) as usize;
            let mut key_bytes = vec![0u8; key_len];
            reader.read_exact(&mut key_bytes)?;
            let key = String::from_utf8_lossy(&key_bytes).to_string();

            if reader.read_exact(&mut len_buf).is_err() { break; }
            let data_len = u32::from_le_bytes(len_buf) as usize;
            let mut data_bytes = vec![0u8; data_len];
            reader.read_exact(&mut data_bytes)?;
            map.insert(key, Record::from_bytes(&data_bytes));
        }
        Ok(())
    }

    pub fn get_field_index(&mut self, table_name: &str, dict_name: &str) -> Option<usize> {
        let table = self.get_table(table_name)?;
        let dict_item = table.dictionary.get(dict_name)?;
        let field_no_str = dict_item.fields.get(0)
            .and_then(|f| f.values.get(0))
            .and_then(|v| v.sub_values.get(0))?;
        
        match field_no_str.parse::<usize>() {
            Ok(idx) if idx > 0 => Some(idx - 1),
            _ => None,
        }
    }

    pub fn get_conversion_code(&mut self, table_name: &str, dict_name: &str) -> Option<String> {
        let table = self.get_table(table_name)?;
        let dict_item = table.dictionary.get(dict_name)?;
        dict_item.fields.get(6)
            .and_then(|f| f.values.get(0))
            .and_then(|v| v.sub_values.get(0))
            .cloned()
    }

    pub fn apply_conversion(value: &str, code: &str) -> String {
        if code.is_empty() {
            return value.to_string();
        }

        if code.starts_with('D') {
            // Dates are stored as ms since epoch
            if let Ok(ms) = value.parse::<i64>() {
                let seconds = ms / 1000;
                let date = match time::OffsetDateTime::from_unix_timestamp(seconds) {
                    Ok(d) => d,
                    Err(_) => return value.to_string(),
                };

                let format = &code[1..];
                let year = date.year();
                let month = date.month() as u8;
                let day = date.day();

                if format.starts_with('4') {
                    let sep = format.chars().nth(1).unwrap_or('-');
                    return format!("{:02}{}{:02}{}{:04}", month, sep, day, sep, year);
                } else if format.starts_with('2') {
                    let sep = format.chars().nth(1).unwrap_or('/');
                    return format!("{:02}{}{:02}{}{:02}", month, sep, day, sep, year % 100);
                }
            }
        } else if code.starts_with('M') {
            // Numbers are stored as integers
            if let Ok(num) = value.parse::<i64>() {
                let mut res = value.to_string();
                if code.len() >= 3 && code.chars().nth(1) == Some('R') {
                    if let Some(precision_char) = code.chars().nth(2) {
                        if let Some(precision) = precision_char.to_digit(10) {
                            if precision > 0 {
                                let factor = 10f64.powi(precision as i32);
                                let float_val = num as f64 / factor;
                                res = format!("{:.*}", precision as usize, float_val);
                            }
                        }
                    }
                }
                return res;
            }
        }

        value.to_string()
    }

    pub fn query(&mut self, table_name: &str, use_dict_section: bool, dict_name: &str, op: &str, value: &str, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        let field_idx = match self.get_field_index(table_name, dict_name) {
            Some(idx) => idx,
            None => return vec![],
        };

        let table = self.get_table(table_name).unwrap(); // safe because get_field_index succeeded

        let mut results = Vec::new();
        let source_map = if use_dict_section { &table.dictionary } else { &table.records };
        
        if let Some(filter_keys) = keys_to_filter {
            for key in filter_keys {
                if let Some(record) = source_map.get(key) {
                    if let Some(field) = record.fields.get(field_idx) {
                        let mut match_found = false;
                        for v in &field.values {
                            if v.sub_values.iter().any(|sv| Self::compare_values(sv, op, value)) {
                                match_found = true;
                                break;
                            }
                        }
                        if match_found {
                            results.push((key.clone(), record.clone()));
                        }
                    }
                }
            }
        } else {
            for (key, record) in source_map {
                if let Some(field) = record.fields.get(field_idx) {
                    let mut match_found = false;
                    for v in &field.values {
                        if v.sub_values.iter().any(|sv| Self::compare_values(sv, op, value)) {
                            match_found = true;
                            break;
                        }
                    }
                    if match_found {
                        results.push((key.clone(), record.clone()));
                    }
                }
            }
        }
        results.sort_by(|a, b| a.0.cmp(&b.0));
        results
    }

    fn compare_values(record_val: &str, op: &str, search_val: &str) -> bool {
        match op {
            "=" => {
                if search_val.starts_with('[') && search_val.ends_with(']') {
                    record_val.contains(&search_val[1..search_val.len() - 1])
                } else if search_val.ends_with(']') {
                    record_val.starts_with(&search_val[..search_val.len() - 1])
                } else if search_val.starts_with('[') {
                    record_val.ends_with(&search_val[1..])
                } else {
                    record_val == search_val
                }
            }
            "#" => !Self::compare_values(record_val, "=", search_val),
            "<" => record_val < search_val,
            ">" => record_val > search_val,
            "<=" => record_val <= search_val,
            ">=" => record_val >= search_val,
            "[" => record_val.ends_with(search_val),
            "]" => record_val.starts_with(search_val),
            "[]" => record_val.contains(search_val),
            _ => false,
        }
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
        fs::create_dir_all(&table_dir)?;
        File::create(format!("{}/data", table_dir))?;
        File::create(format!("{}/dict", table_dir))?;

        self.available_tables.insert(name.to_string());

        // Update DIR file if it exists and this is not the DIR file itself
        if name != "DIR" && self.available_tables.contains("DIR") {
            let _ = self.sync_dir_file();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accounts() -> io::Result<()> {
        let base_dir = "test_accounts_dir";
        if Path::new(base_dir).exists() { fs::remove_dir_all(base_dir)?; }

        {
            let mut db = Database::new(base_dir)?;
            db.create_account("ACC1", None)?;
            db.create_account("ACC2", None)?;

            // Log to ACC1 and create a table
            db.logto("ACC1")?;
            db.create_table("T1")?;
            let t1 = db.get_table_mut("T1");
            t1.records.insert("K1".to_string(), Record::from_string("VAL1"));
            t1.dirty = true;
            db.save()?;

            // Log to ACC2 and create a table with same name but different content
            db.logto("ACC2")?;
            db.create_table("T1")?;
            let t1_acc2 = db.get_table_mut("T1");
            t1_acc2.records.insert("K1".to_string(), Record::from_string("VAL2"));
            t1_acc2.dirty = true;
            db.save()?;
        }

        // Re-open and verify isolation
        {
            let mut db = Database::new(base_dir)?;
            db.logto("ACC1")?;
            let t1 = db.get_table("T1").unwrap();
            assert_eq!(t1.records.get("K1").unwrap().to_string(), "VAL1");

            db.logto("ACC2")?;
            let t1 = db.get_table("T1").unwrap();
            assert_eq!(t1.records.get("K1").unwrap().to_string(), "VAL2");

            // Test delete account
            db.delete_account("ACC1")?;
        }

        let mut db = Database::new(base_dir)?;
        assert!(db.logto("ACC1").is_err());
        assert!(db.logto("ACC2").is_ok());

        fs::remove_dir_all(base_dir)?;
        Ok(())
    }

    #[test]
    fn test_conversions() {
        // Date conversions
        // 2026-03-26 is 1774483200000 ms since epoch
        let date_ms_str = "1774483200000";
        
        assert_eq!(Database::apply_conversion(date_ms_str, "D4-"), "03-26-2026");
        assert_eq!(Database::apply_conversion(date_ms_str, "D2/"), "03/26/26");
        
        // Number conversions
        assert_eq!(Database::apply_conversion("12345", "MR2"), "123.45");
        assert_eq!(Database::apply_conversion("12345", "MR4"), "1.2345");
        assert_eq!(Database::apply_conversion("12345", "MR0"), "12345"); // MR0 should not have decimals
        assert_eq!(Database::apply_conversion("12345", "M"), "12345"); // No R precision
    }

    #[test]
    fn test_record_serialization() {
        let mut record = Record::new();
        let v1 = Value { sub_values: vec!["sv1".to_string(), "sv2".to_string()] };
        let v2 = Value { sub_values: vec!["v2".to_string()] };
        let f1 = Field { values: vec![v1, v2] };
        let f2 = Field { values: vec![Value { sub_values: vec!["f2v1".to_string()] }] };
        record.fields = vec![f1, f2];

        let serialized = record.to_bytes();
        let deserialized = Record::from_bytes(&serialized);

        assert_eq!(record, deserialized);
    }

    #[test]
    fn test_empty_record() {
        let record = Record::from_string("");
        assert_eq!(record.to_string(), "");
    }

    #[test]
    fn test_database_persistence() -> io::Result<()> {
        let dir = "test_db_dir";
        {
            let mut db = Database::new(dir)?;
            let table = db.get_table_mut("USERS");
            let mut record = Record::new();
            record.fields = vec![Field { values: vec![Value { sub_values: vec!["data".to_string()] }] }];
            table.records.insert("key1".to_string(), record);
            table.dirty = true;
            db.save()?;
        }

        {
            let mut db = Database::new(dir)?;
            let table = db.get_table("USERS").unwrap();
            let record = table.records.get("key1").unwrap();
            assert_eq!(record.fields[0].values[0].sub_values[0], "data");
        }

        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn test_query() -> io::Result<()> {
        let mut db = Database::new("test_query_dir")?;
        let table = db.get_table_mut("USERS");
        
        // Dictionary item: First.Name points to field 1
        let mut dict_item = Record::new();
        dict_item.fields = vec![Field { values: vec![Value { sub_values: vec!["1".to_string()] }] }];
        table.dictionary.insert("First.Name".to_string(), dict_item);

        // Data records
        let mut r1 = Record::new();
        r1.fields = vec![Field { values: vec![Value { sub_values: vec!["Ted".to_string()] }] }];
        table.records.insert("K1".to_string(), r1);

        let mut r2 = Record::new();
        r2.fields = vec![Field { values: vec![Value { sub_values: vec!["John".to_string()] }] }];
        table.records.insert("K2".to_string(), r2);

        let results = db.query("USERS", false, "First.Name", "=", "Ted", None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "K1");

        if Path::new("test_query_dir").exists() {
            std::fs::remove_dir_all("test_query_dir")?;
        }
        Ok(())
    }

    #[test]
    fn test_create_delete_table() -> io::Result<()> {
        let dir = "test_file_ops_dir";
        if Path::new(dir).exists() { fs::remove_dir_all(dir)?; }
        
        let mut db = Database::new(dir)?;
        
        // Create table
        db.create_table("MYTABLE")?;
        assert!(Path::new(&format!("{}/MYTABLE/data", dir)).exists());
        assert!(Path::new(&format!("{}/MYTABLE/dict", dir)).exists());
        assert!(db.available_tables.contains("MYTABLE"));
        
        // Create duplicate should fail
        assert!(db.create_table("MYTABLE").is_err());
        
        // Delete table
        db.delete_table("MYTABLE")?;
        assert!(!Path::new(&format!("{}/MYTABLE", dir)).exists());
        assert!(!db.available_tables.contains("MYTABLE"));
        
        // Delete non-existent should fail
        assert!(db.delete_table("NONEXISTENT").is_err());
        
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn test_lru_eviction() -> io::Result<()> {
        let dir = "test_lru_dir";
        if Path::new(dir).exists() { fs::remove_dir_all(dir)?; }
        
        {
            let mut db = Database::new(dir)?;
            db.max_loaded = 2;
            
            // Access 3 tables
            let t1 = db.get_table_mut("T1");
            t1.records.insert("k".to_string(), Record::from_string("v1"));
            t1.dirty = true;
            
            let t2 = db.get_table_mut("T2");
            t2.records.insert("k".to_string(), Record::from_string("v2"));
            t2.dirty = true;
            
            // This should evict T1 (it was the oldest)
            let _t3 = db.get_table_mut("T3");
            
            assert!(!db.loaded_tables.contains_key("T1"));
            assert!(db.loaded_tables.contains_key("T2"));
            assert!(db.loaded_tables.contains_key("T3"));
        }
        
        // Re-open and check if T1 was flushed
        {
            let mut db = Database::new(dir)?;
            let t1 = db.get_table("T1").expect("T1 should be available on disk");
            assert_eq!(t1.records.get("k").unwrap().to_string(), "v1");
        }
        
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn test_dict_query() -> io::Result<()> {
        let mut db = Database::new("test_dict_query_dir")?;
        let table = db.get_table_mut("USERS");
        
        // Dictionary item describing field 1 (let's call it "ATTR")
        let mut attr_dict = Record::new();
        attr_dict.fields = vec![Field { values: vec![Value { sub_values: vec!["1".to_string()] }] }];
        table.dictionary.insert("ATTR".to_string(), attr_dict);

        // Another dictionary item "First.Name" which is for field 1
        let mut name_dict = Record::new();
        name_dict.fields = vec![Field { values: vec![Value { sub_values: vec!["1".to_string()] }] }];
        table.dictionary.insert("First.Name".to_string(), name_dict);

        // Querying the dictionary section: Find all dictionary items where ATTR is "1"
        let results = db.query("USERS", true, "ATTR", "=", "1", None);
        
        // Should find both "ATTR" and "First.Name"
        assert_eq!(results.len(), 2);
        let keys: std::collections::HashSet<_> = results.iter().map(|(k, _)| k.clone()).collect();
        assert!(keys.contains("ATTR"));
        assert!(keys.contains("First.Name"));

        if Path::new("test_dict_query_dir").exists() {
            std::fs::remove_dir_all("test_dict_query_dir")?;
        }
        Ok(())
    }

    #[test]
    fn test_record_edit_translation() {
        let mut record = Record::new();
        // A^B]C\D^E
        record.fields = vec![
            Field { values: vec![Value { sub_values: vec!["A".to_string()] }] },
            Field { values: vec![
                Value { sub_values: vec!["B".to_string()] },
                Value { sub_values: vec!["C".to_string(), "D".to_string()] }
            ] },
            Field { values: vec![Value { sub_values: vec!["E".to_string()] }] },
        ];

        let edit_string = record.to_edit_string();
        // We expect A\nB]C\D\nE
        assert_eq!(edit_string, "A\nB]C\\D\nE");

        let deserialized = Record::from_edit_string(&edit_string);
        assert_eq!(record, deserialized);
        
        // Test with trailing newline (common from editors)
        let edit_string_with_nl = edit_string.clone() + "\n";
        let deserialized_with_nl = Record::from_edit_string(&edit_string_with_nl);
        assert_eq!(record, deserialized_with_nl);
    }

    #[test]
    fn test_query_operators() -> io::Result<()> {
        let mut db = Database::new("test_op_dir")?;
        let table = db.get_table_mut("T1");
        
        let mut dict = Record::new();
        dict.fields = vec![Field { values: vec![Value { sub_values: vec!["1".to_string()] }] }];
        table.dictionary.insert("F1".to_string(), dict);

        let mut r1 = Record::new();
        r1.fields = vec![Field { values: vec![Value { sub_values: vec!["Apple".to_string()] }] }];
        table.records.insert("K1".to_string(), r1);

        let mut r2 = Record::new();
        r2.fields = vec![Field { values: vec![Value { sub_values: vec!["Banana".to_string()] }] }];
        table.records.insert("K2".to_string(), r2);

        let mut r3 = Record::new();
        r3.fields = vec![Field { values: vec![Value { sub_values: vec!["Cherry".to_string()] }] }];
        table.records.insert("K3".to_string(), r3);

        // Test =
        assert_eq!(db.query("T1", false, "F1", "=", "Apple", None).len(), 1);
        
        // Test #
        assert_eq!(db.query("T1", false, "F1", "#", "Apple", None).len(), 2);

        // Test <
        assert_eq!(db.query("T1", false, "F1", "<", "Banana", None).len(), 1); // Apple

        // Test >
        assert_eq!(db.query("T1", false, "F1", ">", "Banana", None).len(), 1); // Cherry

        // Test <=
        assert_eq!(db.query("T1", false, "F1", "<=", "Banana", None).len(), 2); // Apple, Banana

        // Test >=
        assert_eq!(db.query("T1", false, "F1", ">=", "Banana", None).len(), 2); // Banana, Cherry

        // Test wildcards with =
        assert_eq!(db.query("T1", false, "F1", "=", "App]", None).len(), 1);
        assert_eq!(db.query("T1", false, "F1", "=", "[ana]", None).len(), 1); // Banana contains ana
        assert_eq!(db.query("T1", false, "F1", "=", "[ple", None).len(), 1); // Apple ends with ple

        // Test operators [ and ]
        assert_eq!(db.query("T1", false, "F1", "]", "App", None).len(), 1); // Starts with App
        assert_eq!(db.query("T1", false, "F1", "[", "ple", None).len(), 1); // Ends with ple
        assert_eq!(db.query("T1", false, "F1", "[]", "ana", None).len(), 1); // Contains ana

        fs::remove_dir_all("test_op_dir")?;
        Ok(())
    }
}
