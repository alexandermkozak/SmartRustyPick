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
use crate::db::health::{self, Health, Measure, Verdict};
use crate::db::models::{Field, Record, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexState {
    /// The dictionary field indexed.
    pub field: String,
    /// The Pick attribute number that field resolved to when the index was
    /// written. A dictionary edit that moves the field makes the index stale.
    pub attribute: usize,
    /// `meta.version` of the data section this index was last written against.
    pub data_version: u64,
    /// Values this index deliberately does not hold, sorted. Part of the state
    /// rather than of a separate manifest because it changes what the index
    /// *contains*: editing it makes the index stale exactly as moving the field
    /// does, and a restart has to come back with the same set or the index
    /// would quietly hold more than it says it does.
    pub excluded: BTreeSet<String>,
}

fn state_path(section_path: &str) -> PathBuf {
    hashfile::section_dir(section_path).join("state")
}

/// Escapes one excluded value for a `state` line.
///
/// `state` is line oriented and its reader trims, so a value carrying a newline
/// or edge whitespace would come back as a different value than it went in as -
/// and an exclusion that reads back wrong is an index that silently holds what
/// it says it does not. Only the characters that would actually break the
/// format are escaped, so the common cases (`""`, `ACTIVE`) stay readable in
/// the file.
fn escape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' ' => out.push_str("\\s"),
            _ => out.push(c),
        }
    }
    out
}

/// The inverse of [`escape_value`]. An unknown escape keeps its own character,
/// which is the reading that loses the least of a hand-edited file.
fn unescape_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
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
    let mut excluded = BTreeSet::new();
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
            // One line per excluded value rather than one delimited line: a
            // delimiter is a character a value is then not allowed to hold.
            "exclude" => {
                excluded.insert(unescape_value(value));
            }
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
        excluded,
    })
}

/// Writes an index section's `state`, atomically and with the checksum first,
/// exactly as `meta` is written: a truncation at a line boundary must not be
/// able to take the checksum with it and leave a plausible older file behind.
pub fn write_state(section_path: &str, state: &IndexState, fsync: FsyncPolicy) -> io::Result<()> {
    let dir = hashfile::section_dir(section_path);
    fs::create_dir_all(&dir)?;
    let mut body = format!(
        "field={}\nattribute={}\ndata_version={}\n",
        state.field, state.attribute, state.data_version
    );
    // Sorted, because `excluded` is a `BTreeSet`: the same exclusions must
    // produce the same bytes, or the checksum would change without the state
    // having changed.
    for value in &state.excluded {
        body.push_str(&format!("exclude={}\n", escape_value(value)));
    }
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
    /// Values this index deliberately does not hold, already through
    /// [`index_key`] so a lookup compares like with like.
    ///
    /// The motivating shape is a field where 90% of records carry one value:
    /// that field is excellent to index *for the other 10%*. Indexing the
    /// dominant value buys nothing - the lookup hands the scan behind it most
    /// of the file, which is the work it was going to do anyway - and costs the
    /// most, because it is the longest posting list and so the entry rewritten
    /// most expensively on every write that touches it.
    excluded: BTreeSet<String>,
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
    /// Lookups this index answered since the server started. Deliberately not
    /// persisted: the question they answer is "is anything querying this", and
    /// a count carried over from a previous run answers it wrongly.
    ///
    /// Relaxed atomics because they sit on the read path and nothing is
    /// decided by their exact interleaving - a management view reads a number
    /// that was true a moment ago, which is all it ever claims to be.
    usage: IndexUsage,
}

/// The read-path counters. See [`FileIndex::usage`].
#[derive(Default, Debug)]
struct IndexUsage {
    lookups: AtomicU64,
    candidates: AtomicU64,
    matched: AtomicU64,
    /// Lookups whose survivor count was attributed - see
    /// [`FileIndex::note_survivors`].
    measured: AtomicU64,
    /// Lookups that fell back to a scan because the value asked for is one this
    /// index excludes. The number that says whether an exclusion was the right
    /// call: a high one means the excluded value is queried a lot, and the scan
    /// behind it is doing the work either way.
    excluded_lookups: AtomicU64,
}

impl FileIndex {
    /// An empty index that has not been reconciled with any records yet.
    pub fn new(field: &str, attr: usize) -> Self {
        Self::with_exclusions(field, attr, BTreeSet::new())
    }

    /// [`FileIndex::new`], with a set of values the index will not hold.
    pub fn with_exclusions(field: &str, attr: usize, excluded: BTreeSet<String>) -> Self {
        FileIndex {
            field: field.to_string(),
            attr,
            postings: BTreeMap::new(),
            excluded: normalise_exclusions(excluded),
            dirty_values: HashSet::new(),
            dirty_all: true,
            meta: SectionMeta::empty(),
            data_version: 0,
            needs_rebuild: true,
            usage: IndexUsage::default(),
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
            // `state` is the only record of the exclusions, so an index whose
            // state cannot be read comes back without them and is rebuilt as an
            // ordinary index. That is a cost, never a wrong answer: an index
            // holding *more* than it needs to still only ever narrows.
            excluded: state.map(|state| state.excluded.clone()).unwrap_or_default(),
            dirty_values: HashSet::new(),
            dirty_all: false,
            meta,
            data_version: state.map(|state| state.data_version).unwrap_or(0),
            needs_rebuild: !matches,
            usage: IndexUsage::default(),
        }
    }

    /// The values this index does not hold, sorted.
    pub fn excluded(&self) -> &BTreeSet<String> {
        &self.excluded
    }

    /// Replaces the excluded set, reporting whether it actually changed.
    ///
    /// A change marks the index for rebuild rather than trying to repair it in
    /// place: adding an exclusion has to drop a posting list, removing one has
    /// to derive a posting list that was never kept, and the second of those
    /// needs every record anyway. Same rule as moving the field to a different
    /// attribute, and for the same reason - the index no longer describes the
    /// records the way it says it does.
    pub fn set_excluded(&mut self, values: BTreeSet<String>) -> bool {
        let values = normalise_exclusions(values);
        if values == self.excluded {
            return false;
        }
        self.excluded = values;
        self.needs_rebuild = true;
        true
    }

    /// Whether `key` (already through [`index_key`]) is one this index skips.
    pub fn excludes(&self, key: &str) -> bool {
        self.excluded.contains(key)
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
                if self.excluded.contains(&value) {
                    continue;
                }
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
    /// An excluded value is filtered out of *both* sides, so it is never
    /// inserted and never looked for on the way out. Filtering only the new
    /// side would leave an entry behind the first time a value was excluded
    /// after it had already been indexed.
    pub fn apply(&mut self, key: &str, old: Option<&Record>, new: Option<&Record>) {
        let before = self.retained(old);
        let after = self.retained(new);
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

    /// The record keys one record contributes to this index, exclusions removed.
    fn retained(&self, record: Option<&Record>) -> Vec<String> {
        let Some(record) = record else { return Vec::new() };
        keys_of(Some(record), self.attr)
            .into_iter()
            .filter(|value| !self.excluded.contains(value))
            .collect()
    }

    /// The keys carrying `value`, or `None` when this index cannot answer for
    /// it.
    ///
    /// The distinction between `None` and an empty set is the whole contract.
    /// An empty set means "no record carries this value"; `None` means "I
    /// cannot help, scan for it". Two things produce `None`:
    ///
    /// * an index that has fallen behind the records, and
    /// * a value this index excludes - which holds no posting list precisely
    ///   because it was not worth one, and whose empty list would otherwise be
    ///   read as "no records" and hand back nothing at all.
    ///
    /// That second case is sound only because "I do not know" was already an
    /// answer the planner handles: the index only ever narrows, and the
    /// evaluation behind it decides.
    pub fn candidates(&self, value: &str) -> Option<&BTreeSet<String>> {
        if self.needs_rebuild {
            return None;
        }
        if self.excluded.contains(value) {
            self.usage.excluded_lookups.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let keys = self
            .postings
            .get(value)
            .unwrap_or(EMPTY_POSTINGS.get_or_init(BTreeSet::new));
        self.usage.lookups.fetch_add(1, Ordering::Relaxed);
        self.usage.candidates.fetch_add(keys.len() as u64, Ordering::Relaxed);
        Some(keys)
    }

    /// Records how many of the candidates from one lookup survived the filter.
    ///
    /// Attributed only when a single index resolved the whole query, which is
    /// the only case where the survivors *are* this index's: once an `AND`
    /// intersects two indexes there is no honest way to say which of them the
    /// surviving records are owed to. A composed query therefore counts its
    /// lookups and its candidates and leaves the precision alone, and
    /// `measured` says how much of the usage the precision is actually over.
    pub fn note_survivors(&self, matched: u64) {
        self.usage.measured.fetch_add(1, Ordering::Relaxed);
        self.usage.matched.fetch_add(matched, Ordering::Relaxed);
    }

    /// The read-path counters, as a management view reports them.
    pub fn usage(&self) -> IndexUsageStats {
        IndexUsageStats {
            lookups: self.usage.lookups.load(Ordering::Relaxed),
            candidates: self.usage.candidates.load(Ordering::Relaxed),
            matched: self.usage.matched.load(Ordering::Relaxed),
            measured_lookups: self.usage.measured.load(Ordering::Relaxed),
            excluded_lookups: self.usage.excluded_lookups.load(Ordering::Relaxed),
        }
    }

    /// The `limit` values holding the most keys, largest first.
    ///
    /// What turns "this index is skewed" into "`STATUS = ACTIVE` is 91% of it",
    /// which is the difference between a diagnosis and a number. Ties break on
    /// the value so the same index always reports the same list.
    pub fn histogram(&self, limit: usize) -> Vec<IndexValue> {
        let mut values: Vec<IndexValue> = self
            .postings
            .iter()
            .map(|(value, keys)| IndexValue {
                value: value.clone(),
                keys: keys.len() as u64,
            })
            .collect();
        values.sort_by(|a, b| b.keys.cmp(&a.keys).then_with(|| a.value.cmp(&b.value)));
        values.truncate(limit);
        values
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
                excluded: self.excluded.clone(),
            },
            fsync,
        )?;
        self.data_version = data_version;
        Ok(())
    }

    /// What a management view reports about this index.
    ///
    /// `records` is the file's record count, which is what turns a posting
    /// count into a verdict: an index whose largest value holds forty keys is
    /// excellent on a file of forty thousand records and useless on one of
    /// sixty.
    pub fn stats(&self, file_dir: &str, loaded: bool, records: u64) -> IndexStats {
        let (disk_bytes, modified) = tree_stats(&hashfile::section_dir(&section_path(file_dir, &self.field)));
        let stats = IndexStats {
            file: String::new(),
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
            excluded: self.excluded.iter().cloned().collect(),
            usage: self.usage(),
            health: crate::db::health::Health::default(),
        };
        stats.judged(records)
    }
}

/// Excluded values as they are stored: through [`index_key`], because that is
/// what a lookup will compare against, and de-duplicated by the set itself.
fn normalise_exclusions(values: BTreeSet<String>) -> BTreeSet<String> {
    values.iter().map(|value| index_key(value)).collect()
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
    /// The file this index belongs to. Carried on the object rather than
    /// implied by the request, so the per-file listing and the account-wide one
    /// hand a client the same row and it renders one table for both.
    pub file: String,
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
    /// Values this index deliberately does not hold. A query for one of these
    /// falls back to a scan rather than trusting an empty posting list.
    pub excluded: Vec<String>,
    /// What the read path has actually asked of this index since the server
    /// started. Never persisted.
    pub usage: IndexUsageStats,
    /// The verdicts on all of the above.
    pub health: crate::db::health::Health,
}

/// One value of an index and how many record keys carry it.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct IndexValue {
    pub value: String,
    pub keys: u64,
}

/// What the read path asked of one index since the server started.
///
/// A hit rate of zero is the clearest possible signal that an index should go:
/// it is maintained on every write to its field whether or not anything ever
/// queries it, and a perfectly shaped index nobody uses is pure cost. The
/// candidates-against-survivors ratio is the only honest measure of how
/// selective an index is *for the queries actually being run*, which is not the
/// same thing as how selective it is over the data.
///
/// Explicitly not persisted, and reset by a restart. The question is "is
/// anything querying this", and a number carried over from a previous run
/// answers it wrongly.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct IndexUsageStats {
    /// Lookups this index answered.
    pub lookups: u64,
    /// Record keys those lookups handed to the filter behind them.
    pub candidates: u64,
    /// Candidates that survived the filter, over the `measured_lookups` below.
    pub matched: u64,
    /// Lookups whose survivors could be attributed to this index - a query one
    /// index resolved on its own. A composed query counts in `lookups` and
    /// `candidates` but not here, because there is no honest way to say which
    /// of two indexes a surviving record is owed to.
    pub measured_lookups: u64,
    /// Lookups that fell back to a scan because the value asked for is
    /// excluded. A high count means the excluded value is queried often - which
    /// is not an argument against the exclusion, since the scan behind it was
    /// going to do that work anyway.
    pub excluded_lookups: u64,
}

/// One index, its value distribution and the diagnosis, as `INDEX.STATS`
/// answers.
///
/// Its own command rather than a wider `LIST.INDEXES`: the listing is per file
/// and is read on every navigation, so it should stay cheap, while this sorts
/// every distinct value the index holds and is asked for deliberately.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct IndexReport {
    /// Records in the file, which is what the value counts are read against.
    pub record_count: u64,
    pub index: IndexStats,
    /// The values holding the most keys, largest first.
    pub top_values: Vec<IndexValue>,
    /// Values the histogram could not be read for, because the index is stale
    /// or the file is not in memory and its section would not read.
    pub values_available: bool,
}

impl IndexStats {
    /// Names the file this index belongs to.
    ///
    /// Filled in by the engine rather than by the section reader, which knows a
    /// directory and not what the database calls it.
    pub fn in_file(mut self, file: &str) -> IndexStats {
        self.file = file.to_string();
        self
    }

    /// Records an average lookup hands back to the filter behind it.
    ///
    /// Postings over values rather than records over values: on a multivalued
    /// field one record contributes several postings, and it is the postings a
    /// lookup returns.
    pub fn per_lookup(&self) -> f64 {
        if self.values == 0 {
            0.0
        } else {
            self.postings as f64 / self.values as f64
        }
    }

    /// The share of the file the commonest indexed value covers.
    pub fn dominant_share(&self, records: u64) -> f64 {
        if records == 0 {
            0.0
        } else {
            self.largest_postings as f64 / records as f64
        }
    }

    /// Fills in [`IndexStats::health`] against the file's record count.
    ///
    /// Taken by value and returned, so the statistics and the verdicts on them
    /// cannot be built apart and then get out of step.
    pub fn judged(mut self, records: u64) -> IndexStats {
        self.health = index_health(&self, records);
        self
    }
}

/// The verdicts on one index. See [`crate::db::health`] for what a verdict is
/// and where the numbers behind these come from.
fn index_health(stats: &IndexStats, records: u64) -> Health {
    use crate::db::health::thresholds as t;
    let mut measures = Vec::new();

    measures.push(if stats.stale {
        Measure::new(
            "freshness",
            "Matches the records",
            "no",
            Verdict::Act,
            "an index whose data version differs from the file's is stale",
            "This index does not describe the records as they now are, so every query on its field \
             scans instead. Rebuild it.",
        )
    } else {
        Measure::new(
            "freshness",
            "Matches the records",
            "yes",
            Verdict::Good,
            "an index whose data version differs from the file's is stale",
            "The index was built against the records currently on disk.",
        )
    });

    // Selectivity: what an average lookup costs the scan behind it.
    let per_lookup = stats.per_lookup();
    let lookup_share = if records == 0 { 0.0 } else { per_lookup / records as f64 };
    measures.push(if stats.values == 0 {
        Measure::new(
            "selectivity",
            "Records per lookup",
            "—",
            Verdict::Watch,
            format!("watch above {} of the file", health::percent(t::LOOKUP_SHARE_WATCH)).as_str(),
            "Empty: nothing in the file carries this field yet, so the index costs writes and saves \
             nothing. That changes as soon as records do carry it.",
        )
    } else if lookup_share >= t::LOOKUP_SHARE_WATCH {
        Measure::new(
            "selectivity",
            "Records per lookup",
            health::ratio(per_lookup),
            Verdict::Watch,
            format!("watch above {} of the file", health::percent(t::LOOKUP_SHARE_WATCH)).as_str(),
            format!(
                "An average lookup hands back {} of the file ({} records), so the scan behind it \
                 still does most of the work. The field may simply not have enough distinct values \
                 to be worth indexing.",
                health::percent(lookup_share),
                health::ratio(per_lookup),
            ),
        )
    } else {
        Measure::new(
            "selectivity",
            "Records per lookup",
            health::ratio(per_lookup),
            Verdict::Good,
            format!("watch above {} of the file", health::percent(t::LOOKUP_SHARE_WATCH)).as_str(),
            format!(
                "An average lookup narrows {} records to about {}.",
                records,
                health::ratio(per_lookup)
            ),
        )
    });

    // Skew: the commonest value, which the average hides.
    let share = stats.dominant_share(records);
    let threshold = format!(
        "act above {} of the file, watch above {}, both only once the list passes {} keys",
        health::percent(t::DOMINANT_SHARE_ACT),
        health::percent(t::DOMINANT_SHARE_WATCH),
        t::DOMINANT_MIN_POSTINGS,
    );
    let judged = stats.largest_postings >= t::DOMINANT_MIN_POSTINGS;
    measures.push(if judged && share >= t::DOMINANT_SHARE_ACT {
        Measure::new(
            "dominant_value",
            "Commonest value",
            health::percent(share),
            Verdict::Act,
            &threshold,
            format!(
                "One value covers {} of the file. Indexing it buys nothing - the lookup hands the \
                 scan most of the file anyway - and costs the most, because it is the longest \
                 posting list and so the entry rewritten on every write that touches it. Read the \
                 value histogram and exclude it.",
                health::percent(share)
            ),
        )
    } else if judged && share >= t::DOMINANT_SHARE_WATCH {
        Measure::new(
            "dominant_value",
            "Commonest value",
            health::percent(share),
            Verdict::Watch,
            &threshold,
            format!(
                "One value covers {} of the file. Worth a look at the histogram: excluding it would \
                 leave the rest of the index doing the work it is good at.",
                health::percent(share)
            ),
        )
    } else {
        Measure::new(
            "dominant_value",
            "Commonest value",
            if records == 0 {
                "—".to_string()
            } else {
                health::percent(share)
            },
            Verdict::Good,
            &threshold,
            if judged {
                format!("The commonest value covers {} of the file.", health::percent(share))
            } else {
                "No value holds enough keys for its share to cost anything.".to_string()
            },
        )
    });

    // Usage: is anything actually querying this?
    let usage = &stats.usage;
    measures.push(if usage.lookups < t::USAGE_MIN_LOOKUPS {
        Measure::new(
            "usage",
            "Lookups served",
            "0",
            Verdict::Watch,
            "watch at zero lookups since the server started",
            "No query has used this index since the server started, so right now it is maintained on \
             every write to its field and saves nothing. The counter resets on a restart, so read it \
             against how long the server has been up before dropping anything.",
        )
    } else {
        Measure::new(
            "usage",
            "Lookups served",
            usage.lookups.to_string(),
            Verdict::Good,
            "watch at zero lookups since the server started",
            format!(
                "{} lookups since the server started, handing back {} candidates in total.",
                usage.lookups, usage.candidates
            ),
        )
    });

    // Precision: of what it handed back, how much the query actually wanted.
    let precision_threshold = format!(
        "watch below {} of candidates surviving the filter",
        health::percent(t::PRECISION_WATCH)
    );
    if usage.measured_lookups == 0 || usage.candidates == 0 {
        measures.push(Measure::new(
            "precision",
            "Candidates that matched",
            "—",
            Verdict::Good,
            &precision_threshold,
            "Not measured yet. Only a query one index resolved on its own can have its survivors \
             attributed to that index, so this fills in once such a query has run.",
        ));
    } else {
        let precision = usage.matched as f64 / usage.candidates as f64;
        measures.push(if precision < t::PRECISION_WATCH {
            Measure::new(
                "precision",
                "Candidates that matched",
                health::percent(precision),
                Verdict::Watch,
                &precision_threshold,
                format!(
                    "Only {} of what this index handed back survived the filter, so the queries \
                     actually being run are narrowed far less than the index's shape over the data \
                     suggests. Check the histogram for the values they ask for.",
                    health::percent(precision)
                ),
            )
        } else {
            Measure::new(
                "precision",
                "Candidates that matched",
                health::percent(precision),
                Verdict::Good,
                &precision_threshold,
                format!(
                    "{} of the candidates this index produced survived the filter behind it.",
                    health::percent(precision)
                ),
            )
        });
    }

    if !stats.excluded.is_empty() {
        measures.push(Measure::new(
            "exclusions",
            "Excluded values",
            stats.excluded.len().to_string(),
            Verdict::Good,
            "informational: excluded values are a deliberate choice, never a fault",
            format!(
                "{} deliberately not indexed. A query for one of them falls back to a scan rather \
                 than trusting an empty posting list; {} lookups have done so.",
                excluded_list(&stats.excluded),
                usage.excluded_lookups,
            ),
        ));
    }

    Health::of(measures)
}

/// Excluded values as prose, with the empty string named rather than shown as
/// nothing at all.
fn excluded_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| {
            if value.is_empty() {
                "the empty value".to_string()
            } else {
                format!("\"{}\"", value)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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
        file: String::new(),
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
        stale: state.as_ref().is_none_or(|state| state.data_version != data_version),
        loaded: false,
        built_seconds_ago: seconds_ago(modified),
        excluded: state
            .as_ref()
            .map(|state| state.excluded.iter().cloned().collect())
            .unwrap_or_default(),
        // The counters live on the in-memory index, so a file that is not
        // loaded has never served a lookup in this run by definition.
        usage: IndexUsageStats::default(),
        health: Health::default(),
    }
}

/// [`stats_from_meta`], plus the posting counts, which need the section read.
///
/// An index is much smaller than the records it describes - values and keys, no
/// record bodies - so reading one to answer a management request is affordable
/// where reading the file would not be. A section that cannot be read reports
/// what its metadata said and stays marked stale.
pub fn stats_from_disk(file_dir: &str, field: &str, data_version: u64, records: u64) -> IndexStats {
    stats_and_values_from_disk(file_dir, field, data_version, records, 0).0
}

/// [`stats_from_disk`], and the `limit` values holding the most keys.
///
/// One read of the section answers both, so the histogram costs nothing beyond
/// the statistics it is read alongside. `values_available` is false when the
/// section would not read at all - the statistics then say what the metadata
/// said and the index is reported stale, which is the honest answer rather than
/// an empty histogram that looks like an empty index.
pub fn stats_and_values_from_disk(
    file_dir: &str,
    field: &str,
    data_version: u64,
    records: u64,
    limit: usize,
) -> (IndexStats, Vec<IndexValue>, bool) {
    let mut stats = stats_from_meta(file_dir, field, data_version);
    let mut entries: HashMap<String, Record> = HashMap::new();
    if hashfile::load(&section_path(file_dir, field), &mut entries).is_err() {
        stats.stale = true;
        return (stats.judged(records), Vec::new(), false);
    }
    stats.values = entries.len() as u64;
    let mut total = 0;
    let mut largest = 0;
    let mut values: Vec<IndexValue> = Vec::with_capacity(if limit == 0 { 0 } else { entries.len() });
    for (value, record) in &entries {
        let size = posting_keys(record).len() as u64;
        total += size;
        largest = largest.max(size);
        if limit > 0 {
            values.push(IndexValue {
                value: value.clone(),
                keys: size,
            });
        }
    }
    stats.postings = total;
    stats.largest_postings = largest;
    values.sort_by(|a, b| b.keys.cmp(&a.keys).then_with(|| a.value.cmp(&b.value)));
    values.truncate(limit);
    (stats.judged(records), values, true)
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
