//! Secondary indexes on dictionary fields.
//!
//! A keyed read is O(1): the key is hashed and one group is read. Everything
//! else used to be a full scan of a fully resident file, so `WITH ID.CODE = "X"`
//! cost the same whether it matched one record or a million. An index closes
//! that gap for the case it can be closed for cheaply: equality on a field the
//! dictionary already describes.
//!
//! # Layout
//!
//! An index is a section beside the records, in the same format:
//!
//! ```text
//! <account>/<file>/data.hf/          the records
//! <account>/<file>/index.CITY.hf/    an index on the CITY dictionary field
//! ```
//!
//! It reuses [`crate::db::hashfile`] whole - the same framing, the same
//! per-group checksums, the same `.tmp` + rename - so an index inherits the
//! crash-safety story of the data rather than inventing a second one. The key
//! of an index entry is an indexed *value*; the record it maps to holds the
//! keys of every record carrying that value, one per value mark.
//!
//! Beside the section's own `meta`, an index carries a `state` file naming the
//! dictionary field it indexes, the attribute that field resolved to, and the
//! `meta.version` of the data section it was last written against.
//!
//! # Staleness
//!
//! The flush order is: records first, then each index, then that index's
//! `state`. `state.data_version` therefore only ever names a data version that
//! is already on disk, and a crash anywhere in between leaves an index whose
//! `data_version` does not match the data's. That mismatch is the staleness
//! signal: such an index is rebuilt when the file is loaded and is never
//! consulted before it has been. The same check catches an index whose field
//! has been moved to a different attribute by a dictionary edit.
//!
//! An index is therefore never silently wrong. It is either consistent with the
//! data, or detectably stale - and a stale one is rebuilt rather than trusted.

use crate::db::hashfile::{self, FsyncPolicy, SectionMeta, SectionSource};
use crate::db::models::{Field, Record, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Prefix of an index section's path inside a file's directory. The field name
/// follows it, and [`hashfile::SECTION_SUFFIX`] follows that.
pub const SECTION_PREFIX: &str = "index.";

/// Longest an index key may be. A group file refuses to read back a key longer
/// than its own limit, so a value longer than this is indexed under a truncated
/// key instead of being dropped: several long values then share one entry,
/// which costs a few extra candidates and never a missing one.
pub const MAX_INDEX_KEY: usize = 512;

/// Marks a key that stands for values too long to index whole.
const TRUNCATION_MARK: &str = "\u{2026}";

/// The characters a field name may use to become a directory name.
///
/// Dictionary entries are named by people, and a name is about to become a path
/// component, so the set is closed rather than escaped: everything here is safe
/// in a path on every platform this runs on, and anything else is refused when
/// the index is created rather than sanitised into a name that no longer says
/// which field it indexes.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '$' | '#' | '%')
}

/// Whether `field` can name an index section.
pub fn is_valid_field_name(field: &str) -> bool {
    !field.is_empty() && field.len() <= 128 && field.chars().all(is_name_char) && field != "." && field != ".."
}

/// The section path of one index, without the `.hf` suffix
/// [`hashfile`] appends.
pub fn section_path(file_dir: &str, field: &str) -> String {
    format!("{}/{}{}", file_dir, SECTION_PREFIX, field)
}

/// The fields `file_dir` holds an index section for, sorted.
///
/// Read off the directory rather than from a manifest: the sections are the
/// record of which indexes exist, so there is no second list to disagree with
/// them after a crash or a manual removal.
pub fn indexed_fields(file_dir: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let Ok(entries) = fs::read_dir(file_dir) else {
        return fields;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix(SECTION_PREFIX) else {
            continue;
        };
        let Some(field) = rest.strip_suffix(hashfile::SECTION_SUFFIX) else {
            continue;
        };
        if is_valid_field_name(field) {
            fields.push(field.to_string());
        }
    }
    fields.sort();
    fields
}

/// The key an indexed value is stored under.
///
/// Trimmed, because that is what a comparison does to a record's value before
/// testing it, and truncated past [`MAX_INDEX_KEY`] so a very long value cannot
/// write a key the group format refuses to read back. Both only ever widen the
/// candidate set a lookup produces, never narrow it.
pub fn index_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= MAX_INDEX_KEY {
        return trimmed.to_string();
    }
    let mut end = MAX_INDEX_KEY;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &trimmed[..end], TRUNCATION_MARK)
}

/// Every key one record contributes to an index on attribute `attr`.
///
/// Deliberately the same set of texts a query compares against: each sub-value
/// of the attribute, and the empty string when the attribute is absent or holds
/// nothing - because `WITH FIELD = ""` matches a record that does not have the
/// field at all. A multivalued attribute contributes each of its values, which
/// is what makes an index usable for the multivalue selections too.
pub fn keys_of(record: Option<&Record>, attr: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |text: &str, out: &mut Vec<String>| {
        let key = index_key(text);
        if !out.contains(&key) {
            out.push(key);
        }
    };
    let Some(field) = record.and_then(|record| record.fields.get(attr)) else {
        out.push(String::new());
        return out;
    };
    if field.values.is_empty() {
        out.push(String::new());
        return out;
    }
    for value in &field.values {
        if value.sub_values.is_empty() {
            push("", &mut out);
        }
        for sub in &value.sub_values {
            push(sub, &mut out);
        }
    }
    out
}

/// The record an index entry is stored as: one attribute holding the keys that
/// carry the value, one per value mark.
fn posting_record(keys: &BTreeSet<String>) -> Record {
    Record {
        fields: vec![Field {
            values: keys
                .iter()
                .map(|key| Value {
                    sub_values: vec![key.clone()],
                })
                .collect(),
        }],
    }
}

/// The keys held by a stored index entry.
fn posting_keys(record: &Record) -> BTreeSet<String> {
    record
        .fields
        .first()
        .map(|field| {
            field
                .values
                .iter()
                .filter_map(|value| value.sub_values.first().cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// What an index section's `state` file says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexState {
    /// The dictionary field indexed.
    pub field: String,
    /// The Pick attribute number that field resolved to when the index was
    /// written. A dictionary edit that moves the field makes the index stale.
    pub attribute: usize,
    /// `meta.version` of the data section this index was last written against.
    pub data_version: u64,
}

fn state_path(section_path: &str) -> PathBuf {
    hashfile::section_dir(section_path).join("state")
}

/// Reads an index section's `state`, or `None` when it has none.
///
/// A `state` that cannot be read or does not check out is reported as absent
/// rather than as an error: every use of it is "does this index still match the
/// data", and an unreadable one answers that with "no".
pub fn read_state(section_path: &str) -> Option<IndexState> {
    let content = fs::read_to_string(state_path(section_path)).ok()?;
    if !content.ends_with('\n') {
        return None;
    }
    let mut field = None;
    let mut attribute = None;
    let mut data_version = None;
    let mut stored: Option<u32> = None;
    let mut body = String::new();
    for line in content.lines() {
        let (key, value) = line.split_once('=')?;
        let (key, value) = (key.trim(), value.trim());
        if key == "checksum" {
            stored = u32::from_str_radix(value, 16).ok();
            continue;
        }
        body.push_str(line);
        body.push('\n');
        match key {
            "field" => field = Some(value.to_string()),
            "attribute" => attribute = value.parse::<usize>().ok(),
            "data_version" => data_version = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    if stored? != hashfile::crc32c(body.as_bytes()) {
        return None;
    }
    Some(IndexState {
        field: field?,
        attribute: attribute?,
        data_version: data_version?,
    })
}

/// Writes an index section's `state`, atomically and with the checksum first,
/// exactly as `meta` is written: a truncation at a line boundary must not be
/// able to take the checksum with it and leave a plausible older file behind.
pub fn write_state(section_path: &str, state: &IndexState, fsync: FsyncPolicy) -> io::Result<()> {
    let dir = hashfile::section_dir(section_path);
    fs::create_dir_all(&dir)?;
    let body = format!(
        "field={}\nattribute={}\ndata_version={}\n",
        state.field, state.attribute, state.data_version
    );
    let tmp = dir.join("state.tmp");
    {
        let mut file = File::create(&tmp)?;
        writeln!(file, "checksum={:08x}", hashfile::crc32c(body.as_bytes()))?;
        file.write_all(body.as_bytes())?;
        file.flush()?;
        if fsync != FsyncPolicy::Never {
            file.sync_all()?;
        }
    }
    fs::rename(tmp, state_path(section_path))
}

/// Removes an index section entirely.
pub fn remove_section(section_path: &str) -> io::Result<()> {
    let dir = hashfile::section_dir(section_path);
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// One index, held in memory beside the records it describes.
///
/// The postings are a value mapped to the keys carrying it rather than a map of
/// records, because that is the shape maintenance needs: a write changes a
/// handful of values and must not touch the rest. [`SectionSource`] is what
/// turns it back into records, one entry at a time, when a flush asks.
pub struct FileIndex {
    /// The dictionary field indexed.
    pub field: String,
    /// The 0-based position in a record's attributes that field resolves to.
    pub attr: usize,
    postings: BTreeMap<String, BTreeSet<String>>,
    /// Values whose entry has changed since the last flush. Only those entries
    /// are rewritten, which is what keeps a write independent of index size.
    dirty_values: HashSet<String>,
    /// Set when the whole index has to be written out: a rebuild, or a section
    /// that has never been written.
    dirty_all: bool,
    /// Layout and version of the section on disk.
    pub meta: SectionMeta,
    /// The data version the persisted form of this index matches.
    pub data_version: u64,
    /// True until a rebuild has reconciled this index with the records. A query
    /// never consults an index in this state; it scans instead.
    pub needs_rebuild: bool,
}

impl FileIndex {
    /// An empty index that has not been reconciled with any records yet.
    pub fn new(field: &str, attr: usize) -> Self {
        FileIndex {
            field: field.to_string(),
            attr,
            postings: BTreeMap::new(),
            dirty_values: HashSet::new(),
            dirty_all: true,
            meta: SectionMeta::empty(),
            data_version: 0,
            needs_rebuild: true,
        }
    }

    /// An index read back from disk, trusted only if `state` says it still
    /// matches the data and the field still resolves to the same attribute.
    pub fn loaded(
        field: &str,
        attr: usize,
        entries: HashMap<String, Record>,
        meta: SectionMeta,
        state: Option<&IndexState>,
        data_version: u64,
    ) -> Self {
        let matches = state.is_some_and(|state| {
            state.field == field && state.attribute == attr + 1 && state.data_version == data_version
        });
        FileIndex {
            field: field.to_string(),
            attr,
            postings: entries
                .into_iter()
                .map(|(value, record)| (value, posting_keys(&record)))
                .collect(),
            dirty_values: HashSet::new(),
            dirty_all: false,
            meta,
            data_version: state.map(|state| state.data_version).unwrap_or(0),
            needs_rebuild: !matches,
        }
    }

    /// Discards the postings and derives them again from every record.
    ///
    /// The one O(records) operation an index has, and the answer to every way
    /// one can fall behind: a crash between the two flushes, a dictionary edit
    /// that moved the field, a bulk change that named no keys, or an operator
    /// who asked for it.
    pub fn rebuild(&mut self, records: &HashMap<String, Record>, attr: usize) {
        self.attr = attr;
        self.postings.clear();
        for (key, record) in records {
            for value in keys_of(Some(record), attr) {
                self.postings.entry(value).or_default().insert(key.clone());
            }
        }
        self.dirty_values.clear();
        self.dirty_all = true;
        self.needs_rebuild = false;
    }

    /// Applies one record's change: `old` is what the key held before, `new`
    /// what it holds now, and either may be `None` for an insert or a delete.
    ///
    /// Only the values that actually moved are touched, so a write that leaves
    /// the indexed attribute alone costs a comparison of two short lists and no
    /// index work at all.
    pub fn apply(&mut self, key: &str, old: Option<&Record>, new: Option<&Record>) {
        let before = old.map(|record| keys_of(Some(record), self.attr)).unwrap_or_default();
        let after = new.map(|record| keys_of(Some(record), self.attr)).unwrap_or_default();
        for value in &before {
            if after.contains(value) {
                continue;
            }
            if let Some(keys) = self.postings.get_mut(value) {
                keys.remove(key);
                if keys.is_empty() {
                    self.postings.remove(value);
                }
            }
            self.dirty_values.insert(value.clone());
        }
        for value in after {
            if before.contains(&value) {
                continue;
            }
            self.postings.entry(value.clone()).or_default().insert(key.to_string());
            self.dirty_values.insert(value);
        }
    }

    /// The keys carrying `value`, or `None` when this index cannot answer for
    /// it - which is the case for an index that has fallen behind.
    pub fn candidates(&self, value: &str) -> Option<&BTreeSet<String>> {
        if self.needs_rebuild {
            return None;
        }
        Some(
            self.postings
                .get(value)
                .unwrap_or(EMPTY_POSTINGS.get_or_init(BTreeSet::new)),
        )
    }

    /// Distinct values held.
    pub fn value_count(&self) -> u64 {
        self.postings.len() as u64
    }

    /// Total (value, key) pairs: how much work maintaining this index is, and
    /// with [`value_count`](Self::value_count) how selective it is.
    pub fn posting_count(&self) -> u64 {
        self.postings.values().map(|keys| keys.len() as u64).sum()
    }

    /// The biggest posting list. A large one is the shape that makes an index
    /// worth little: the lookup still hands a scan most of the file.
    pub fn largest_postings(&self) -> u64 {
        self.postings.values().map(|keys| keys.len() as u64).max().unwrap_or(0)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_all || !self.dirty_values.is_empty()
    }

    /// Writes the index out and records the data version it now matches.
    ///
    /// Called after the records have been written, never before: `state` must
    /// not name a data version that is not on disk yet.
    pub fn save(
        &mut self,
        section_path: &str,
        data_version: u64,
        per_group: usize,
        fsync: FsyncPolicy,
    ) -> io::Result<()> {
        if self.is_dirty() {
            let dirty = if self.dirty_all { None } else { Some(&self.dirty_values) };
            self.meta = hashfile::save_with_fsync(section_path, self, self.meta, dirty, per_group, fsync)?;
            self.dirty_values.clear();
            self.dirty_all = false;
        }
        // Even an index nothing changed has its `state` rewritten, because the
        // data version it must name has moved on.
        write_state(
            section_path,
            &IndexState {
                field: self.field.clone(),
                attribute: self.attr + 1,
                data_version,
            },
            fsync,
        )?;
        self.data_version = data_version;
        Ok(())
    }

    /// What a management view reports about this index.
    pub fn stats(&self, file_dir: &str, loaded: bool) -> IndexStats {
        let (disk_bytes, modified) = tree_stats(&hashfile::section_dir(&section_path(file_dir, &self.field)));
        IndexStats {
            field: self.field.clone(),
            attribute: self.attr + 1,
            values: self.value_count(),
            postings: self.posting_count(),
            largest_postings: self.largest_postings(),
            modulus: self.meta.modulus,
            version: self.meta.version,
            group_count: hashfile::group_sizes(&section_path(file_dir, &self.field)).len(),
            disk_bytes,
            data_version: self.data_version,
            stale: self.needs_rebuild,
            loaded,
            built_seconds_ago: seconds_ago(modified),
        }
    }
}

/// Shared empty posting list, so a lookup that finds nothing still returns a
/// borrowed set rather than forcing every caller to own one.
static EMPTY_POSTINGS: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();

impl SectionSource for FileIndex {
    fn record_count(&self) -> u64 {
        self.postings.len() as u64
    }

    fn record(&self, key: &str) -> Option<Cow<'_, Record>> {
        self.postings.get(key).map(|keys| Cow::Owned(posting_record(keys)))
    }

    fn for_each_key<'a>(&'a self, f: &mut dyn FnMut(&'a str)) {
        for value in self.postings.keys() {
            f(value.as_str());
        }
    }
}

/// One index, as a management view describes it.
///
/// The three counts are what an operator decides with. `values` against the
/// file's record count is how selective the field is; `postings` is what
/// maintaining it costs per write; `largest_postings` is the skew the average
/// hides - an index whose biggest value covers half the file saves nothing on
/// that value.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct IndexStats {
    /// The dictionary field indexed.
    pub field: String,
    /// The Pick attribute number that field resolves to.
    pub attribute: usize,
    /// Distinct values held.
    pub values: u64,
    /// Total (value, key) pairs.
    pub postings: u64,
    /// Size of the biggest posting list.
    pub largest_postings: u64,
    /// Hash modulus of the index section.
    pub modulus: u64,
    /// Flush counter of the index section.
    pub version: u64,
    /// Group files the section is spread over.
    pub group_count: usize,
    /// Bytes on disk under the index section.
    pub disk_bytes: u64,
    /// The data section version this index matches.
    pub data_version: u64,
    /// True when the index does not match the data and has to be rebuilt before
    /// it can be used. A loaded file rebuilds a stale index as it opens, so this
    /// is normally only ever seen for a file that is not in memory.
    pub stale: bool,
    /// The file this index belongs to is in the server's table cache.
    pub loaded: bool,
    /// Seconds since the section was last written, when the filesystem reports
    /// a usable timestamp.
    pub built_seconds_ago: Option<u64>,
}

/// Describes an index section without loading its postings.
///
/// Used for a file that is not in memory: `meta` already carries the distinct
/// value count and the layout, so the cheap half of the answer costs one small
/// read. The posting counts need the entries themselves, which is why they are
/// filled in only when the section is small enough to be worth reading - see
/// [`stats_from_disk`].
pub fn stats_from_meta(file_dir: &str, field: &str, data_version: u64) -> IndexStats {
    let path = section_path(file_dir, field);
    let meta = hashfile::read_meta(&path);
    let state = read_state(&path);
    let (disk_bytes, modified) = tree_stats(&hashfile::section_dir(&path));
    IndexStats {
        field: field.to_string(),
        attribute: state.as_ref().map(|state| state.attribute).unwrap_or(0),
        values: meta.map(|meta| meta.records).unwrap_or(0),
        postings: 0,
        largest_postings: 0,
        modulus: meta.map(|meta| meta.modulus).unwrap_or(0),
        version: meta.map(|meta| meta.version).unwrap_or(0),
        group_count: hashfile::group_sizes(&path).len(),
        disk_bytes,
        data_version: state.as_ref().map(|state| state.data_version).unwrap_or(0),
        stale: state.is_none_or(|state| state.data_version != data_version),
        loaded: false,
        built_seconds_ago: seconds_ago(modified),
    }
}

/// [`stats_from_meta`], plus the posting counts, which need the section read.
///
/// An index is much smaller than the records it describes - values and keys, no
/// record bodies - so reading one to answer a management request is affordable
/// where reading the file would not be. A section that cannot be read reports
/// what its metadata said and stays marked stale.
pub fn stats_from_disk(file_dir: &str, field: &str, data_version: u64) -> IndexStats {
    let mut stats = stats_from_meta(file_dir, field, data_version);
    let mut entries: HashMap<String, Record> = HashMap::new();
    if hashfile::load(&section_path(file_dir, field), &mut entries).is_err() {
        stats.stale = true;
        return stats;
    }
    stats.values = entries.len() as u64;
    let sizes = entries.values().map(|record| posting_keys(record).len() as u64);
    let mut total = 0;
    let mut largest = 0;
    for size in sizes {
        total += size;
        largest = largest.max(size);
    }
    stats.postings = total;
    stats.largest_postings = largest;
    stats
}

/// Total bytes under `path` and the most recent modification time found there.
fn tree_stats(path: &Path) -> (u64, Option<std::time::SystemTime>) {
    let mut bytes = 0;
    let mut newest = None;
    let Ok(entries) = fs::read_dir(path) else {
        return (0, None);
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        if metadata.is_dir() {
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
    (bytes, newest)
}

fn seconds_ago(time: Option<std::time::SystemTime>) -> Option<u64> {
    time.and_then(|time| std::time::SystemTime::now().duration_since(time).ok())
        .map(|elapsed| elapsed.as_secs())
}
