use crate::db::hashfile::SectionMeta;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const FM: u8 = 254; // Field Mark
pub const VM: u8 = 253; // Value Mark
pub const SVM: u8 = 252; // Sub-Value Mark

// System file field indices
pub const SYS_ACCOUNTS_PATH_IDX: usize = 0;
pub const SYS_CLIENTS_THUMBPRINT_IDX: usize = 0;
pub const SYS_CLIENTS_ACCOUNTS_IDX: usize = 1;
pub const SYS_CLIENTS_ADMIN_IDX: usize = 2;
pub const SYS_LOGS_MESSAGE_IDX: usize = 0;
pub const SYS_LOGS_DETAIL_IDX: usize = 1;
// DIR entries describe the files of an account: field 1 is the entry type,
// field 2 the per-file durability flag ("Y" = flush every write immediately).
pub const DIR_TYPE_IDX: usize = 0;
pub const DIR_DURABLE_IDX: usize = 1;

// Dictionary record field indices
pub const DICT_FIELD_IDX: usize = 0;
pub const DICT_NAME_IDX: usize = 1;
pub const DICT_JUSTIFY_IDX: usize = 2;
pub const DICT_WIDTH_IDX: usize = 3;
pub const DICT_CONV_IDX: usize = 7;

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
        let translated_data: Vec<u8> = content.as_bytes().iter().map(|&b| match b {
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
                FM => b'^',
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
    pub stamp: Option<TableStamp>,
    /// Layout and version of the record section on disk. Carried on the table
    /// so a flush knows the current modulus without re-reading the meta file.
    pub data_meta: SectionMeta,
    /// Set when the records were read from the pre-hashfile flat file; the
    /// next flush converts the table and removes the old file.
    pub legacy_data: bool,
    /// Keys changed since the last flush. Only the groups these hash into are
    /// rewritten, which is what keeps a write independent of table size.
    pub dirty_keys: HashSet<String>,
    /// Set when a change cannot be attributed to individual keys (bulk edits,
    /// migrations); forces a full rewrite of every group.
    pub dirty_all: bool,
    pub dict_dirty: bool,
}

impl Table {
    pub fn new() -> Self {
        Table {
            records: HashMap::new(),
            dictionary: HashMap::new(),
            stamp: None,
            data_meta: SectionMeta::empty(),
            legacy_data: false,
            dirty_keys: HashSet::new(),
            dirty_all: false,
            dict_dirty: false,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_all || self.dict_dirty || !self.dirty_keys.is_empty()
    }

    pub fn records_dirty(&self) -> bool {
        self.dirty_all || !self.dirty_keys.is_empty()
    }

    /// Marks a single record as changed. Preferred over [`Table::touch_all`]:
    /// it lets the flush rewrite one group instead of the whole table.
    pub fn mark_dirty(&mut self, key: &str) {
        self.dirty_keys.insert(key.to_string());
    }

    /// Marks the whole table - records and dictionary - as changed. Only for
    /// edits whose key set is not known, since it costs a full rewrite.
    pub fn touch_all(&mut self) {
        self.dirty_all = true;
        self.dict_dirty = true;
    }

    pub fn mark_dict_dirty(&mut self) {
        self.dict_dirty = true;
    }

    pub fn insert_record(&mut self, key: &str, record: Record) {
        self.records.insert(key.to_string(), record);
        self.mark_dirty(key);
    }

    pub fn remove_record(&mut self, key: &str) -> Option<Record> {
        let previous = self.records.remove(key);
        self.mark_dirty(key);
        previous
    }

    pub fn clear_dirty(&mut self) {
        self.dirty_keys.clear();
        self.dirty_all = false;
        self.dict_dirty = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableStamp {
    pub data_modified: Option<std::time::SystemTime>,
    /// Flush counter for a hashed section, byte length for a legacy flat file.
    /// Either way it changes whenever another process rewrites the records.
    pub data_len: u64,
    pub dict_modified: Option<std::time::SystemTime>,
    pub dict_len: u64,
}

#[derive(Clone, Debug)]
pub struct SelectList {
    pub table_name: String,
    pub is_dict: bool,
    pub keys: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub thumbprint: String,
    pub allowed_accounts: Vec<String>,
    pub is_admin: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct QueryCondition {
    pub field_name: String,
    pub op: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SortSpec {
    pub field_name: String,
    pub descending: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum QueryNode {
    Condition(QueryCondition),
    Logical {
        op: LogicalOp,
        left: Box<QueryNode>,
        right: Box<QueryNode>,
    },
}
