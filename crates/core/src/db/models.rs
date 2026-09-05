use crate::db::hashfile::SectionMeta;
use crate::db::index::FileIndex;
use crate::db::queue::QueueState;
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
// field 2 the per-file durability flag ("Y" = flush every write immediately),
// field 3 the queue flag, and fields 4 and 5 that queue's claim policy. A file
// carries its own policy because a queue of thirty-second jobs and a queue of
// hour-long ones need different answers, and DIR is where a per-file answer
// already survives a rebuild of the listing.
pub const DIR_TYPE_IDX: usize = 0;
pub const DIR_DURABLE_IDX: usize = 1;
pub const DIR_QUEUE_IDX: usize = 2;
/// Attribute 4: seconds a claim on this queue is held before it lapses. Empty
/// means [`crate::db::queue::DEFAULT_VISIBILITY`].
pub const DIR_QUEUE_TIMEOUT_IDX: usize = 3;
/// Attribute 5: deliveries this queue gives a record before dead lettering it.
/// Empty means [`crate::db::queue::DEFAULT_MAX_DELIVERIES`].
pub const DIR_QUEUE_RETRIES_IDX: usize = 4;

/// One `DIR` entry, read as what it says about the file rather than as five
/// attributes to be picked apart at each call site.
///
/// Three places need all of this at once - creating a file, changing it, and
/// rebuilding the listing - and the rebuild is the one that matters: it
/// reconstructs every entry from the filesystem, which knows none of this, so
/// each attribute has to be carried across in one piece or be lost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileAttributes {
    /// Every write to this file is flushed before it is acknowledged.
    pub durable: bool,
    /// The claim policy, for a file that is a queue. `None` for an ordinary one.
    pub queue: Option<crate::db::queue::QueuePolicy>,
}

impl FileAttributes {
    /// What a `DIR` record says. Anything unreadable reads as the default, so a
    /// hand-edited entry degrades to an ordinary buffered file rather than
    /// making the account's listing unopenable.
    pub fn of(record: &Record) -> Self {
        let attribute = |idx: usize| record.get_field_display_string(idx);
        let flag = |idx: usize| {
            matches!(
                attribute(idx).trim().to_uppercase().as_str(),
                "Y" | "YES" | "1" | "TRUE" | "DURABLE" | "QUEUE"
            )
        };
        FileAttributes {
            durable: flag(DIR_DURABLE_IDX),
            queue: flag(DIR_QUEUE_IDX).then(|| {
                crate::db::queue::QueuePolicy::from_attributes(
                    &attribute(DIR_QUEUE_TIMEOUT_IDX),
                    &attribute(DIR_QUEUE_RETRIES_IDX),
                )
            }),
        }
    }

    /// The `DIR` record that says this.
    ///
    /// A queue's timeout and retry limit are written out even when they are the
    /// defaults: the entry is what an administrator reads to find out what a
    /// queue will do, and "blank, which means sixty" is a worse answer than
    /// "60".
    pub fn to_record(self) -> Record {
        let mut rec = Record::new();
        while rec.fields.len() <= DIR_QUEUE_RETRIES_IDX {
            rec.fields.push(Field::default());
        }
        let mut set = |idx: usize, text: String| rec.fields[idx].values = vec![Value::text(text)];
        set(DIR_TYPE_IDX, "F".to_string());
        set(DIR_DURABLE_IDX, if self.durable { "Y" } else { "" }.to_string());
        set(DIR_QUEUE_IDX, if self.queue.is_some() { "Y" } else { "" }.to_string());
        let (timeout, retries) = match self.queue {
            Some(policy) => (
                policy.visibility_seconds().to_string(),
                policy.max_deliveries.to_string(),
            ),
            None => (String::new(), String::new()),
        };
        set(DIR_QUEUE_TIMEOUT_IDX, timeout);
        set(DIR_QUEUE_RETRIES_IDX, retries);
        rec
    }
}

// Dictionary record field indices
pub const DICT_FIELD_IDX: usize = 0;
pub const DICT_NAME_IDX: usize = 1;
pub const DICT_JUSTIFY_IDX: usize = 2;
pub const DICT_WIDTH_IDX: usize = 3;
/// Attribute 5: the controlling field this entry is associated with. Empty on a
/// controller and on an unassociated field - see [`Association`].
pub const DICT_ASSOC_IDX: usize = 4;
/// Attribute 6: the tier this entry associates at, [`ASSOC_VALUE`] or
/// [`ASSOC_SUB_VALUE`]. Only read when attribute 5 names a controller.
pub const DICT_ASSOC_DEPTH_IDX: usize = 5;
pub const DICT_CONV_IDX: usize = 7;

/// Attribute 6 for a dependent that pairs value-for-value with its controller.
pub const ASSOC_VALUE: &str = "V";
/// Attribute 6 for a dependent that pairs sub-value-for-sub-value inside the
/// controller's value.
pub const ASSOC_SUB_VALUE: &str = "S";

/// How far up the chain of controllers a walk will follow before deciding the
/// dictionary describes a cycle. A dictionary deep enough to need more than
/// this is a dictionary that has gone wrong.
pub(crate) const ASSOC_MAX_DEPTH: usize = 16;

/// The JSON key a sub-value that is not valid UTF-8 travels under.
pub const BINARY_JSON_KEY: &str = "$base64";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub values: Vec<Value>,
}

/// One sub-value: the smallest addressable piece of a record.
///
/// Raw bytes rather than a `String`, because a record is a byte container and
/// always was on disk. Reading one used to go through `String::from_utf8_lossy`,
/// which turned every byte that was not valid UTF-8 into `U+FFFD` and lost the
/// original for good - a write that reported success and came back as something
/// else. Text is now a *view* of a sub-value ([`text`](SubValues::text)), taken
/// where a caller has asked for text, rather than the way it is stored.
pub type SubValue = Vec<u8>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Value {
    pub sub_values: Vec<SubValue>,
}

/// Bytes as text, replacing anything that is not valid UTF-8.
///
/// Lossy on purpose and only here: a caller reaching for this has asked for
/// something displayable and has accepted that answer. Nothing on the storage
/// path may use it - see [`Record::to_bytes`].
pub fn text_of(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

impl Value {
    /// A value holding one sub-value of text.
    pub fn text(s: impl AsRef<str>) -> Self {
        Value {
            sub_values: vec![s.as_ref().as_bytes().to_vec()],
        }
    }

    /// A value holding one sub-value of raw bytes.
    pub fn bytes(b: impl Into<SubValue>) -> Self {
        Value {
            sub_values: vec![b.into()],
        }
    }

    /// A value whose sub-values are each a piece of text, in order.
    pub fn texts<I, S>(subs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Value {
            sub_values: subs.into_iter().map(|s| s.as_ref().as_bytes().to_vec()).collect(),
        }
    }

    /// The first sub-value as text, or `None` when the value is empty.
    pub fn first_text(&self) -> Option<std::borrow::Cow<'_, str>> {
        self.sub_values.first().map(|sub| text_of(sub))
    }

    /// This value as the sole value of a field.
    pub fn into_field(self) -> Field {
        Field { values: vec![self] }
    }

    /// The first sub-value's bytes, or an empty slice when there is none.
    ///
    /// The shape most callers want: a value with exactly one sub-value is an
    /// ordinary single value, and reading it should not have to say so.
    pub fn first_bytes(&self) -> &[u8] {
        self.sub_values.first().map(Vec::as_slice).unwrap_or(&[])
    }
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
                        let sub_values = v.split(|&b| b == SVM).map(<[u8]>::to_vec).collect();
                        Value { sub_values }
                    })
                    .collect();
                Field { values }
            })
            .collect();
        Record { fields }
    }

    /// The record as it is stored: sub-values joined by `SVM`, values by `VM`,
    /// fields by `FM`.
    ///
    /// Byte-exact in both directions -
    /// `Record::from_bytes(r.to_bytes()) == r` - for any record whose
    /// sub-values hold no mark byte. Every other byte, valid UTF-8 or not,
    /// survives untouched.
    ///
    /// The exception is not a bug to fix here: `FM`, `VM` and `SVM` *are* the
    /// structure, so a mark inside a sub-value is indistinguishable from the
    /// separator it is, and reading it back splits the value in two. That is
    /// the MultiValue data model, and it is why content that may contain
    /// arbitrary bytes belongs in a blob section referenced by the record
    /// rather than inlined into one (see #32).
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
                    res.extend_from_slice(sv);
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
                .map(|attribute| Field {
                    values: vec![Value::text(attribute)],
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
                Some(s) => to_display_chars(s),
                None => String::new(),
            },
            None => {
                let mut res = Vec::new();
                for (k, sv) in value.sub_values.iter().enumerate() {
                    if k > 0 {
                        res.push(SVM);
                    }
                    res.extend_from_slice(sv);
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
                    res.extend_from_slice(sv);
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
    /// The order and the claims, for a file whose `DIR` entry marks it a queue.
    ///
    /// `None` for every ordinary file, which is what keeps a queue's cost off
    /// the path every other file takes. Attached when the table is loaded and
    /// rebuilt from the records each time - see [`crate::db::queue`].
    pub queue: Option<QueueState>,
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
            queue: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_all || self.dict_dirty || !self.dirty_keys.is_empty() || self.indexes_dirty() || self.queue_dirty()
    }

    /// Brings a queue's order back in line with its records - see
    /// [`QueueState::reconcile`]. A no-op on a file that is not a queue.
    pub fn reconcile_queue(&mut self) {
        if let Some(state) = self.queue.as_mut() {
            state.reconcile(&self.records);
        }
    }

    /// True when a queue has claimed, returned or minted something the `queue`
    /// file beside the records does not yet say. A dequeue changes no record,
    /// so without this a delivery count could be lost to a restart that the
    /// table did not otherwise think it had anything to write.
    pub fn queue_dirty(&self) -> bool {
        self.queue.as_ref().is_some_and(|queue| queue.is_dirty())
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
        self.create_index_excluding(field, BTreeSet::new())
    }

    /// [`Table::create_index`], with a set of values the index will not hold.
    pub fn create_index_excluding(&mut self, field: &str, excluded: BTreeSet<String>) -> crate::db::DbResult<()> {
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
        let mut index = FileIndex::with_exclusions(&field, attr, excluded);
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

/// Which tier of the record hierarchy an association pairs its members on.
///
/// PICK has two, and so does this: a value tier, where row *n* is value *n* of
/// every member, and a sub-value tier nested inside it, where a row is one
/// sub-value of one controlling value. A member declares its own tier in
/// attribute 6, so a group can pair some of its fields value-for-value and
/// others sub-value-for-sub-value at the same time.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssociationDepth {
    #[default]
    Value,
    SubValue,
}

impl AssociationDepth {
    /// Reads attribute 6. Only [`ASSOC_SUB_VALUE`] asks for the second tier;
    /// everything else - an empty attribute, or one that has been mistyped by
    /// hand - is the value tier, which is what an association means by default.
    pub fn from_attribute(text: &str) -> Self {
        if text.trim().eq_ignore_ascii_case(ASSOC_SUB_VALUE) {
            AssociationDepth::SubValue
        } else {
            AssociationDepth::Value
        }
    }

    /// The attribute 6 text for this tier.
    pub fn as_attribute(self) -> &'static str {
        match self {
            AssociationDepth::Value => ASSOC_VALUE,
            AssociationDepth::SubValue => ASSOC_SUB_VALUE,
        }
    }
}

/// One field of an association group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationMember {
    pub name: String,
    /// 0-based index into a record's fields, as [`Table::field_index`] resolves
    /// it.
    ///
    /// [`Table::field_index`]: crate::db::engine::Table::field_index
    pub index: usize,
    pub depth: AssociationDepth,
}

/// A set of dictionary fields that explode in lockstep: PICK's correlated
/// multivalued attributes.
///
/// Exploding any one member explodes all of them, so a record whose `ACCOUNTS`
/// and `ACCT.DATES` each hold three values yields three rows pairing them,
/// rather than three rows repeating every date or nine rows pairing nothing.
///
/// The relationship is recorded on the *dependent*, in attribute 5, naming its
/// controller. One name per entry means a field is in at most one group by
/// construction, and there is no list on the controller to fall out of step
/// with the entries it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Association {
    /// The field every member's attribute 5 leads to. It is a member itself
    /// whenever the dictionary defines it, and always at the value tier: the
    /// controller *is* the value axis the rest are paired against.
    pub controller: String,
    /// Every member, the controller included, in attribute order.
    pub members: Vec<AssociationMember>,
}

impl Association {
    pub fn member(&self, name: &str) -> Option<&AssociationMember> {
        self.members.iter().find(|member| member.name == name)
    }

    pub fn member_at(&self, index: usize) -> Option<&AssociationMember> {
        self.members.iter().find(|member| member.index == index)
    }

    /// True when some member pairs at the sub-value tier, so a row of this
    /// group can be one sub-value rather than one whole value.
    pub fn has_sub_value_tier(&self) -> bool {
        self.members
            .iter()
            .any(|member| member.depth == AssociationDepth::SubValue)
    }
}

/// How much of a row's position applies to one column.
///
/// Resolved once per column rather than per cell: a report asks the same
/// question of every row, and the answer depends only on the dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Narrowing {
    /// Not part of what exploded: the column shows its whole field, repeated
    /// down the rows.
    Whole,
    /// The position as the row carries it - a value, or a sub-value when the
    /// match went that deep.
    AsGiven,
    /// The value the position names, sub-values and all. A member pairing at
    /// the value tier shows a whole value even on a row a sub-value tier
    /// member put there.
    ValueOnly,
}

impl Narrowing {
    /// The position to read a column at on a row carrying `position`.
    pub fn apply(self, position: Option<ValuePosition>) -> Option<ValuePosition> {
        match self {
            Narrowing::Whole => None,
            Narrowing::AsGiven => position,
            Narrowing::ValueOnly => position.map(|pos| ValuePosition::value(pos.value)),
        }
    }
}

/// What a `BY.EXP` clause resolved to against a dictionary.
///
/// A field in no association explodes alone, exactly as `BY.EXP` has always
/// done. A field in one explodes the whole group, and the single position a row
/// already carried is now read against every member of it - which is why
/// [`SelectEntry`], the saved-list encoding and the wire's `positions` all keep
/// their shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplodeTarget {
    Field { name: String, index: usize },
    Group(Association),
}

impl ExplodeTarget {
    /// The fields whose values become rows.
    pub fn indices(&self) -> Vec<usize> {
        match self {
            ExplodeTarget::Field { index, .. } => vec![*index],
            ExplodeTarget::Group(group) => group.members.iter().map(|member| member.index).collect(),
        }
    }

    /// The group this resolved to, or `None` for a lone field.
    pub fn group(&self) -> Option<&Association> {
        match self {
            ExplodeTarget::Field { .. } => None,
            ExplodeTarget::Group(group) => Some(group),
        }
    }

    /// How a row's position applies to the column at `field_idx`.
    pub fn narrowing_at(&self, field_idx: usize) -> Narrowing {
        match self {
            ExplodeTarget::Field { index, .. } if *index == field_idx => Narrowing::AsGiven,
            ExplodeTarget::Field { .. } => Narrowing::Whole,
            ExplodeTarget::Group(group) => match group.member_at(field_idx) {
                Some(member) if member.depth == AssociationDepth::SubValue => Narrowing::AsGiven,
                Some(_) => Narrowing::ValueOnly,
                None => Narrowing::Whole,
            },
        }
    }

    /// The position to read `field_idx` at on a row carrying `position`.
    pub fn position_at(&self, field_idx: usize, position: Option<ValuePosition>) -> Option<ValuePosition> {
        self.narrowing_at(field_idx).apply(position)
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
    /// columns those positions narrow.
    ///
    /// One name even where the clause named a whole [`Association`]: any member
    /// resolves to the group, and resolving it again when the list is read back
    /// means a saved list keeps its shape and follows a dictionary that has
    /// moved on since - rather than pinning a group as it stood.
    pub explode_field: Option<String>,
    pub entries: Vec<SelectEntry>,
}

/// A select list held for the remote protocol, and how far a client has read it.
///
/// The account is here rather than on [`SelectList`] because it is the remote
/// protocol that has to answer the question. A CLI list belongs to whatever
/// account the session is already in; these are held by the server, keyed by
/// name across every connection, and `GET.NEXT` has to page one against the
/// account its `SELECT` ran in rather than whichever account the paging request
/// happens to mention. Reading a list's keys against a different account finds
/// the same-named file over there and answers from whatever matches - a wrong
/// answer rather than a refusal, which is the worst shape a bug can take.
///
/// The cursor sits beside the list for the same kind of reason: as a parallel
/// map it was a second lookup that had to exist for every list and be removed
/// with it, and the code read it with an `unwrap`.
#[derive(Clone, Debug)]
pub struct RemoteSelectList {
    /// The account the `SELECT` ran in.
    pub account: String,
    pub list: SelectList,
    /// How far `GET.NEXT` has read. `SELECT` resets it by replacing the entry.
    pub cursor: usize,
}

impl RemoteSelectList {
    pub fn new(account: String, list: SelectList) -> Self {
        RemoteSelectList {
            account,
            list,
            cursor: 0,
        }
    }
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
    /// Secondary indexes across every file in the account.
    pub index_count: usize,
    /// How many of those do not match the records they describe.
    pub stale_indexes: usize,
    /// Files whose cheap health check is not `good`, so a problem file can be
    /// found without opening every file in the account in turn.
    pub unhealthy_files: usize,
    /// The worst file verdict in the account.
    pub health: crate::db::health::HealthSummary,
}

/// What a queue file is doing, as `FILE.STATS` reports it.
///
/// A queue nobody is draining is the thing an administrator most needs to see,
/// and it does not show up in a record count: a queue at a steady depth with
/// nothing in flight and an oldest entry an hour old is stalled, and one at the
/// same depth with three claims a few seconds old is working. All four numbers
/// are read from the in-memory queue state, so none of them costs a scan.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct QueueStats {
    /// Records available to be claimed.
    pub depth: u64,
    /// Records claimed and not yet acknowledged.
    pub in_flight: u64,
    /// Age of the oldest record still in the queue, claimed or not, taken from
    /// the millisecond its sequence key carries. `None` for an empty queue, or
    /// one holding only records whose keys the engine did not mint.
    pub oldest_unacknowledged_seconds: Option<u64>,
    /// Records in this queue's dead-letter file. `0` when it has none yet.
    pub dead_letters: u64,
    /// The sequence number the next enqueue will use.
    pub next_sequence: u64,
    /// Seconds a claim on this queue is held before it lapses.
    pub visibility_timeout_seconds: u64,
    /// Deliveries a record gets before it is moved to the dead-letter file.
    pub max_deliveries: u32,
    /// True when this file is itself the dead-letter file of another queue.
    pub dead_letter: bool,
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
    /// What this file is doing as a queue, or `None` when it is not one. The
    /// numbers an administrator needs about a queue are not the numbers they
    /// need about a hash file, so they arrive as their own object rather than
    /// as null columns on every other file's statistics. Sent as `null` for a
    /// file that is not a queue, like the other optional measures here, so a
    /// reader can tell "not a queue" from "a build that does not know about
    /// queues".
    pub queue: Option<QueueStats>,

    // --- Derived measures. Everything below is computed from the section
    // metadata and the group trailers; none of it reads a record.
    /// Bytes in the data section's group files alone. `disk_bytes` is the whole
    /// file directory, so the difference is the dictionary, the indexes and the
    /// small metadata files.
    pub group_bytes: u64,
    /// Bytes under this file's index sections.
    pub index_bytes: u64,
    /// How the records are spread over the groups, read from the trailers.
    pub group_records: GroupDistribution,
    /// Records per group the modulus is aiming for.
    pub records_per_group_target: u64,
    /// `records / (modulus * records_per_group_target)`. Above 1 the next flush
    /// picks a larger modulus and rewrites every group.
    pub load_factor: f64,
    /// Records this file takes before the modulus doubles.
    pub records_until_growth: u64,
    /// Records it has to lose before the modulus halves, or `null` when the
    /// modulus is already at its floor.
    pub records_until_shrink: Option<u64>,
    /// Records in the largest group as a share of every record in the file.
    pub largest_group_share: f64,
    /// Largest group over the mean group, in records. The scale-free way to
    /// read skew: 1.0 is a perfectly even hash whatever the file's size.
    pub skew: f64,
    /// The verdicts on all of the above.
    pub health: crate::db::health::Health,
}

/// How records are spread over a section's groups.
///
/// A hash file's failure mode is not size, it is imbalance: a group holding a
/// large share of the records makes every write to it rewrite that whole group,
/// which is the one cost this format exists to avoid. Two extremes in *bytes*
/// hid that; these are records, and they come from the group trailers rather
/// than from loading anything.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct GroupDistribution {
    /// Groups these figures are over: the modulus, not the group *files*. A
    /// group holding nothing has no file at all, so counting the files would
    /// average four full groups out of a modulus of thirty-two and report a
    /// perfectly even file - which is precisely the case skew exists to catch.
    pub groups: usize,
    pub min: u64,
    pub max: u64,
    pub mean: f64,
    pub median: u64,
    /// Groups holding no records at all, whether or not they have a file.
    pub empty: usize,
    /// Groups above [`thresholds::OVERWEIGHT_FACTOR`] times the mean. One
    /// outlier is noise; a count of them says the hash is not spreading.
    ///
    /// [`thresholds::OVERWEIGHT_FACTOR`]: crate::db::health::thresholds::OVERWEIGHT_FACTOR
    pub overweight: usize,
    /// Groups written before the format appended a trailer, whose record count
    /// cannot be read without loading them. Counted rather than guessed at.
    pub unreadable: usize,
    /// The shape, bucketed for drawing. Empty for a section with no groups.
    pub buckets: Vec<DistributionBucket>,
}

/// One column of a group distribution: how many groups hold between `min` and
/// `max` records, inclusive.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct DistributionBucket {
    pub min: u64,
    pub max: u64,
    pub groups: usize,
}

impl GroupDistribution {
    /// Summarises the group profile of a section.
    ///
    /// Groups whose record count could not be read - the pre-trailer format -
    /// are counted in `unreadable` and left out of every other figure, rather
    /// than folded in as zero. A group holding an unknown number of records is
    /// not a group holding none, and averaging the second into the first is how
    /// a diagnostic quietly starts lying.
    pub fn of(groups: &[crate::db::hashfile::GroupEntry], modulus: u64) -> Self {
        let counts: Vec<u64> = groups.iter().filter_map(|group| group.records).collect();
        let unreadable = groups.len() - counts.len();
        // The groups the modulus has that no file exists for. They hold nothing,
        // which is a fact about the distribution and not an absence from it.
        let absent = (modulus as usize).saturating_sub(groups.len());
        if counts.is_empty() && absent == 0 {
            return GroupDistribution {
                groups: groups.len(),
                unreadable,
                ..Default::default()
            };
        }
        let mut sorted = counts;
        sorted.extend(std::iter::repeat_n(0, absent));
        sorted.sort_unstable();
        let total: u64 = sorted.iter().sum();
        let mean = total as f64 / sorted.len() as f64;
        let overweight_at = mean * crate::db::health::thresholds::OVERWEIGHT_FACTOR;
        GroupDistribution {
            groups: sorted.len() + unreadable,
            min: sorted[0],
            max: sorted[sorted.len() - 1],
            mean,
            median: sorted[sorted.len() / 2],
            empty: sorted.iter().take_while(|count| **count == 0).count(),
            overweight: sorted.iter().filter(|count| **count as f64 > overweight_at).count(),
            unreadable,
            buckets: Self::bucket(&sorted),
        }
    }

    /// The shape, as at most [`DISTRIBUTION_BUCKETS`] equal-width columns over
    /// the record counts.
    ///
    /// Bucketed rather than sent per group so that a file with a modulus of
    /// 65,536 still answers in a small reply, and equal-width rather than
    /// equal-population because the point of drawing it is to see one column
    /// standing far out to the right.
    ///
    /// [`DISTRIBUTION_BUCKETS`]: crate::db::health::thresholds::DISTRIBUTION_BUCKETS
    fn bucket(sorted: &[u64]) -> Vec<DistributionBucket> {
        let (low, high) = (sorted[0], sorted[sorted.len() - 1]);
        let span = high - low + 1;
        let wanted = crate::db::health::thresholds::DISTRIBUTION_BUCKETS as u64;
        // A narrow spread gets one column per record count, which is exact; a
        // wide one gets equal-width columns that still cover the whole range.
        let width = span.div_ceil(wanted).max(1);
        let count = span.div_ceil(width) as usize;
        let mut buckets: Vec<DistributionBucket> = (0..count)
            .map(|i| {
                let min = low + i as u64 * width;
                DistributionBucket {
                    min,
                    max: (min + width - 1).min(high),
                    groups: 0,
                }
            })
            .collect();
        for value in sorted {
            let index = (((value - low) / width) as usize).min(buckets.len() - 1);
            buckets[index].groups += 1;
        }
        buckets
    }
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
