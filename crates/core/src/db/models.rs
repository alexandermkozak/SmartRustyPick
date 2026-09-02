use crate::db::hashfile::SectionMeta;
use crate::db::index::FileIndex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
        let fields = data
            .split(|&b| b == FM)
            .map(|f| {
                let values = f
                    .split(|&b| b == VM)
                    .map(|v| {
                        let sub_values = v
                            .split(|&b| b == SVM)
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
            if i > 0 {
                res.push(FM);
            }
            for (j, v) in f.values.iter().enumerate() {
                if j > 0 {
                    res.push(VM);
                }
                for (k, sv) in v.sub_values.iter().enumerate() {
                    if k > 0 {
                        res.push(SVM);
                    }
                    res.extend_from_slice(sv.as_bytes());
                }
            }
        }
        res
    }

    pub fn to_display_string(&self) -> String {
        to_display_chars(&self.to_bytes())
    }

    /// A record whose attributes each hold one plain value, in order.
    ///
    /// [`from_display_string`](Self::from_display_string) cannot express an
    /// attribute that itself contains `^`, `]` or `\\`, because those are the
    /// marks it splits on. Anything assembled from text a caller supplied - a
    /// dictionary heading, say - is built here instead, so a stray mark
    /// character is stored rather than silently restructuring the record.
    pub fn from_attributes<I, S>(attributes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Record {
            fields: attributes
                .into_iter()
                .map(|text| Field {
                    values: vec![Value {
                        sub_values: vec![text.as_ref().to_string()],
                    }],
                })
                .collect(),
        }
    }

    pub fn from_display_string(s: &str) -> Self {
        let translated_data: Vec<u8> = s
            .as_bytes()
            .iter()
            .map(|&b| match b {
                b'^' => FM,
                b']' => VM,
                b'\\' => SVM,
                _ => b,
            })
            .collect();
        Self::from_bytes(&translated_data)
    }

    pub fn to_edit_string(&self) -> String {
        let display_bytes: Vec<u8> = self
            .to_bytes()
            .iter()
            .map(|&b| match b {
                FM => b'\n',
                VM => b']',
                SVM => b'\\',
                _ => b,
            })
            .collect();
        String::from_utf8_lossy(&display_bytes).to_string()
    }

    pub fn from_edit_string(s: &str) -> Self {
        let mut content = s;
        if content.ends_with('\n') {
            content = &content[..content.len() - 1];
        }
        let translated_data: Vec<u8> = content
            .as_bytes()
            .iter()
            .map(|&b| match b {
                b'\n' => FM,
                b']' => VM,
                b'\\' => SVM,
                _ => b,
            })
            .collect();
        Self::from_bytes(&translated_data)
    }

    /// Renders one position of a field: a single sub-value, a single value with
    /// its sub-values still joined by `\\`, or - for `None` - the whole field
    /// exactly as [`get_field_display_string`](Self::get_field_display_string)
    /// renders it.
    ///
    /// A position that is out of range renders empty rather than panicking, so
    /// a select list that outlived an edit to its records degrades quietly.
    pub fn get_value_display_string(&self, field_idx: usize, pos: Option<ValuePosition>) -> String {
        let Some(pos) = pos else {
            return self.get_field_display_string(field_idx);
        };
        let Some(field) = self.fields.get(field_idx) else {
            return String::new();
        };
        let Some(value) = field.values.get(pos.value) else {
            return String::new();
        };
        match pos.sub_value {
            Some(sv) => match value.sub_values.get(sv) {
                Some(s) => to_display_chars(s.as_bytes()),
                None => String::new(),
            },
            None => {
                let mut res = Vec::new();
                for (k, sv) in value.sub_values.iter().enumerate() {
                    if k > 0 {
                        res.push(SVM);
                    }
                    res.extend_from_slice(sv.as_bytes());
                }
                to_display_chars(&res)
            }
        }
    }

    pub fn get_field_display_string(&self, field_idx: usize) -> String {
        if let Some(field) = self.fields.get(field_idx) {
            let mut res = Vec::new();
            for (j, v) in field.values.iter().enumerate() {
                if j > 0 {
                    res.push(VM);
                }
                for (k, sv) in v.sub_values.iter().enumerate() {
                    if k > 0 {
                        res.push(SVM);
                    }
                    res.extend_from_slice(sv.as_bytes());
                }
            }
            to_display_chars(&res)
        } else {
            String::new()
        }
    }
}

/// Replaces the FM/VM/SVM marks with the printable characters the CLI and the
/// display-string format use for them.
fn to_display_chars(bytes: &[u8]) -> String {
    let display_bytes: Vec<u8> = bytes
        .iter()
        .map(|&b| match b {
            FM => b'^',
            VM => b']',
            SVM => b'\\',
            _ => b,
        })
        .collect();
    String::from_utf8_lossy(&display_bytes).to_string()
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
    /// Secondary indexes on this file's dictionary fields, by field name.
    ///
    /// Held beside the records rather than derived on demand: an index has to
    /// see the record a write replaces to know which value to withdraw, and
    /// that record is only still available at the moment of the write.
    pub indexes: BTreeMap<String, FileIndex>,
}

impl Default for Table {
    fn default() -> Self {
        Self::new()
    }
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
            indexes: BTreeMap::new(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_all || self.dict_dirty || !self.dirty_keys.is_empty() || self.indexes_dirty()
    }

    /// True when an index has changes to write out, or has fallen behind the
    /// records and still has to be rebuilt.
    pub fn indexes_dirty(&self) -> bool {
        self.indexes
            .values()
            .any(|index| index.is_dirty() || index.needs_rebuild)
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
    ///
    /// A change that names no keys is exactly the change an index cannot
    /// maintain incrementally, so every index is marked for a rebuild. Until
    /// that happens a query scans rather than trusting an index that may no
    /// longer describe the records.
    pub fn touch_all(&mut self) {
        self.dirty_all = true;
        self.dict_dirty = true;
        for index in self.indexes.values_mut() {
            index.needs_rebuild = true;
        }
    }

    pub fn mark_dict_dirty(&mut self) {
        self.dict_dirty = true;
        self.recheck_index_attributes();
    }

    pub fn insert_record(&mut self, key: &str, record: Record) {
        if !self.indexes.is_empty() {
            let previous = self.records.get(key);
            for index in self.indexes.values_mut() {
                index.apply(key, previous, Some(&record));
            }
        }
        self.records.insert(key.to_string(), record);
        self.mark_dirty(key);
    }

    pub fn remove_record(&mut self, key: &str) -> Option<Record> {
        let previous = self.records.remove(key);
        for index in self.indexes.values_mut() {
            index.apply(key, previous.as_ref(), None);
        }
        self.mark_dirty(key);
        previous
    }

    pub fn clear_dirty(&mut self) {
        self.dirty_keys.clear();
        self.dirty_all = false;
        self.dict_dirty = false;
    }

    /// The fields this file has an index on, sorted.
    pub fn index_fields(&self) -> Vec<String> {
        self.indexes.keys().cloned().collect()
    }

    pub fn has_index(&self, field: &str) -> bool {
        self.indexes.contains_key(field)
    }

    /// Adds an index on `field` and builds it from the records.
    ///
    /// `Err` carries the reason the field cannot be indexed, which is worth
    /// saying rather than leaving an index that answers nothing: a name that
    /// cannot become a directory, a field the dictionary does not define, or
    /// `ID` - which is the record key, already found in one hash lookup.
    pub fn create_index(&mut self, field: &str) -> crate::db::DbResult<()> {
        let field = field.trim().to_string();
        let refuse = |reason: &str| crate::db::DbError::InvalidField {
            field: field.clone(),
            reason: reason.to_string(),
        };
        if field == "ID" {
            return Err(refuse("it is the record key, already found without a scan"));
        }
        if !crate::db::index::is_valid_field_name(&field) {
            return Err(refuse("an index name may hold only letters, digits and . _ - $ # %"));
        }
        let attr = self
            .field_index(&field)
            .ok_or_else(|| refuse("it is not a dictionary field of this file"))?;
        let mut index = FileIndex::new(&field, attr);
        index.rebuild(&self.records, attr);
        self.indexes.insert(field, index);
        Ok(())
    }

    /// Forgets an index. The section on disk is removed by the caller that owns
    /// the file's directory.
    pub fn drop_index(&mut self, field: &str) -> bool {
        self.indexes.remove(field.trim()).is_some()
    }

    /// Derives one index from the records again.
    pub fn rebuild_index(&mut self, field: &str) -> crate::db::DbResult<()> {
        let field = field.trim();
        // A table does not know which file it is, so this says only that the
        // field is not indexed; the engine checks first and reports
        // `IndexNotFound` naming the file.
        if !self.indexes.contains_key(field) {
            return Err(crate::db::DbError::InvalidField {
                field: field.to_string(),
                reason: "it is not indexed on this file".to_string(),
            });
        }
        let attr = self
            .field_index(field)
            .ok_or_else(|| crate::db::DbError::InvalidField {
                field: field.to_string(),
                reason: "it is not a dictionary field of this file".to_string(),
            })?;
        // The indexes are moved aside rather than the records, so that reading
        // one while rebuilding the other needs no second borrow of `self`. This
        // way round on purpose: a panic in here would lose what is moved, and an
        // index can be derived again where the records cannot.
        let mut indexes = std::mem::take(&mut self.indexes);
        if let Some(index) = indexes.get_mut(field) {
            index.rebuild(&self.records, attr);
        }
        self.indexes = indexes;
        Ok(())
    }

    /// Rebuilds every index that has fallen behind the records. Called where a
    /// `&mut Table` is available - a load, or the start of a flush.
    pub fn rebuild_stale_indexes(&mut self) {
        if !self.indexes.values().any(|index| index.needs_rebuild) {
            return;
        }
        let attributes: Vec<(String, Option<usize>)> = self
            .indexes
            .keys()
            .map(|field| (field.clone(), self.field_index(field)))
            .collect();
        // See `rebuild_index`: the indexes move aside, never the records.
        let mut indexes = std::mem::take(&mut self.indexes);
        for (field, attr) in attributes {
            let Some(index) = indexes.get_mut(&field) else {
                continue;
            };
            if !index.needs_rebuild {
                continue;
            }
            match attr {
                Some(attr) => index.rebuild(&self.records, attr),
                // The dictionary no longer defines the field. Leave the index
                // marked so nothing consults it; dropping it here would delete
                // a section over what may well be a half-finished edit.
                None => index.needs_rebuild = true,
            }
        }
        self.indexes = indexes;
    }

    /// Marks an index stale when a dictionary edit has moved its field to a
    /// different attribute, which is the one way a change to the dictionary can
    /// make the postings wrong.
    fn recheck_index_attributes(&mut self) {
        if self.indexes.is_empty() {
            return;
        }
        let moved: Vec<String> = self
            .indexes
            .values()
            .filter(|index| self.field_index(&index.field) != Some(index.attr))
            .map(|index| index.field.clone())
            .collect();
        for field in moved {
            if let Some(index) = self.indexes.get_mut(&field) {
                index.needs_rebuild = true;
            }
        }
    }

    /// The keys an index says carry `value`, or `None` when no usable index can
    /// answer for that field.
    pub fn index_candidates(&self, field: &str, value: &str) -> Option<&BTreeSet<String>> {
        self.indexes
            .get(field)
            .filter(|index| self.field_index(field) == Some(index.attr))
            .and_then(|index| index.candidates(&crate::db::index::index_key(value)))
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

/// Where inside a multivalued field a match landed.
///
/// `sub_value` is `None` when the whole value is the unit of interest - either
/// nothing was matched against it, or the criterion was satisfied by the value
/// as a whole rather than by one of its sub-values.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValuePosition {
    pub value: usize,
    pub sub_value: Option<usize>,
}

impl ValuePosition {
    pub fn value(value: usize) -> Self {
        ValuePosition { value, sub_value: None }
    }

    pub fn sub_value(value: usize, sub_value: usize) -> Self {
        ValuePosition {
            value,
            sub_value: Some(sub_value),
        }
    }
}

/// One row of a select list: a record key, plus the position within an exploded
/// field that put it there. `position` is `None` for an ordinary, unexploded
/// selection, which is what every list held before `BY.EXP` existed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectEntry {
    pub key: String,
    pub position: Option<ValuePosition>,
}

impl SelectEntry {
    pub fn new(key: String) -> Self {
        SelectEntry { key, position: None }
    }

    pub fn at(key: String, position: ValuePosition) -> Self {
        SelectEntry {
            key,
            position: Some(position),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SelectList {
    pub table_name: String,
    pub is_dict: bool,
    /// The field the entries' positions refer to, when the list was built by a
    /// `BY.EXP` clause. A later `LIST` of the same file needs this to know which
    /// column those positions narrow.
    pub explode_field: Option<String>,
    pub entries: Vec<SelectEntry>,
}

impl SelectList {
    /// Builds an unexploded list, which is what every caller that has only keys
    /// wants.
    pub fn from_keys(table_name: String, is_dict: bool, keys: Vec<String>) -> Self {
        SelectList {
            table_name,
            is_dict,
            explode_field: None,
            entries: keys.into_iter().map(SelectEntry::new).collect(),
        }
    }

    /// The keys of the list, in order. An exploded list repeats a key once per
    /// matching position, so this is not deduplicated.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.key.as_str())
    }

    /// The distinct keys of the list, in first-seen order.
    ///
    /// An exploded list repeats a key once per matching position. Commands that
    /// act on records rather than on values - `GET`, `DELETE`, `CT` - want each
    /// record once, not once per value that matched.
    pub fn unique_keys(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.entries
            .iter()
            .filter(|e| seen.insert(e.key.as_str()))
            .map(|e| e.key.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClientInfo {
    /// The `$CLIENTS` record key: the name the client was authorized under.
    pub name: String,
    pub thumbprint: String,
    pub allowed_accounts: Vec<String>,
    pub is_admin: bool,
}

/// What a management view needs to know about one account, gathered without
/// loading any of its records.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct AccountStats {
    pub name: String,
    pub directory: String,
    /// Files in the account, including system and `DIR` files.
    pub file_count: usize,
    /// Records across every file, read from each file's section metadata.
    pub record_count: u64,
    /// Bytes on disk under the account directory.
    pub disk_bytes: u64,
}

/// Statistics for a single data file. Deliberately record free: the dashboard
/// navigates files, it does not browse their contents.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct FileStats {
    pub account: String,
    pub name: String,
    /// Records in the data section, from its metadata rather than a load.
    pub record_count: u64,
    /// Entries in the dictionary section.
    pub dict_count: usize,
    /// Hash modulus: how many groups the records are spread over.
    pub modulus: u64,
    /// Flush counter of the data section; increments on every write out.
    pub version: u64,
    /// Group files present on disk, and how their bytes are distributed.
    pub group_count: usize,
    pub smallest_group_bytes: u64,
    pub largest_group_bytes: u64,
    pub disk_bytes: u64,
    /// True once the section carries per-group checksums.
    pub checksums: bool,
    /// Still in the pre-hashfile flat format, converted on the next flush.
    pub legacy: bool,
    /// Every write to this file is flushed before it is acknowledged.
    pub durable: bool,
    /// Currently held in the server's table cache.
    pub loaded: bool,
    /// Seconds since the data section was last modified, when the filesystem
    /// reports a usable timestamp.
    pub modified_seconds_ago: Option<u64>,
    /// The secondary indexes this file carries, in field order.
    pub indexes: Vec<crate::db::index::IndexStats>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct QueryCondition {
    pub field_name: String,
    pub op: String,
    pub value: String,
}

/// A `BY.EXP` clause: the multivalued field whose values become one output row
/// each, and the criterion absorbed from the compact
/// `BY.EXP <field> <op> <value>` spelling, if any.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ExplodeSpec {
    pub field_name: String,
    pub condition: Option<QueryCondition>,
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
