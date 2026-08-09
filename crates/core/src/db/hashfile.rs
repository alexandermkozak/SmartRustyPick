//! Hashed table storage.
//!
//! A section (the records of a table) used to live in one flat file that was
//! rewritten from scratch on every change, which made a single write cost
//! O(file size) and a bulk load O(n^2). Here the section is a directory of
//! *group* files: a record's key is hashed and lands in exactly one group, so a
//! write only rewrites that group. The modulus (number of groups) is chosen
//! dynamically to keep a roughly constant number of records per group, which
//! keeps the cost of a write independent of how large the table has grown.
//!
//! Layout of `<table>/data.hf/`:
//!
//! ```text
//! meta        version / modulus / record count, rewritten on every flush
//! g0000000a   one group, framed exactly like the legacy flat file
//! ```
//!
//! The group files use the same `[key_len][key][data_len][data]` framing as the
//! original format, so a group is just a flat file holding a subset of the keys.

use crate::db::models::Record;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Directory suffix appended to a section path (`.../data` -> `.../data.hf`).
pub const SECTION_SUFFIX: &str = ".hf";

/// Records per group the modulus aims for. Small enough that rewriting one
/// group stays cheap, large enough that a table does not explode into files.
pub const DEFAULT_RECORDS_PER_GROUP: usize = 16;

/// Never go below this many groups: tiny tables should not degrade into a
/// single flat file with the old rewrite-everything behaviour.
pub const MIN_MODULUS: u64 = 8;

const MAX_KEY_LEN: usize = 1024;
const MAX_DATA_LEN: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionMeta {
    /// Incremented on every flush; lets other processes detect a change even
    /// when the filesystem timestamp resolution is too coarse to help.
    pub version: u64,
    pub modulus: u64,
    pub records: u64,
}

impl SectionMeta {
    pub fn empty() -> Self {
        SectionMeta { version: 0, modulus: MIN_MODULUS, records: 0 }
    }
}

/// FNV-1a. Not cryptographic, but fast, dependency free and spreads the short
/// ASCII keys this database uses evenly across groups.
pub fn hash_key(key: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub fn group_of(key: &str, modulus: u64) -> u64 {
    if modulus == 0 { 0 } else { hash_key(key) % modulus }
}

/// Smallest power of two that keeps the table at or below `per_group` records
/// per group. Powers of two make growth a doubling, so the amortised cost of
/// the occasional full rehash is constant per inserted record.
pub fn target_modulus(records: u64, per_group: usize) -> u64 {
    let per_group = per_group.max(1) as u64;
    let mut modulus = MIN_MODULUS;
    while modulus * per_group < records {
        modulus *= 2;
    }
    modulus
}

/// Decides the modulus to flush with. Growth happens as soon as the target is
/// exceeded; shrinking waits until the table is a quarter of its capacity so a
/// table hovering around a boundary does not rehash on every flush.
pub fn plan_modulus(current: u64, records: u64, per_group: usize) -> u64 {
    let target = target_modulus(records, per_group);
    if current < MIN_MODULUS {
        return target;
    }
    if target > current || target * 4 <= current {
        return target;
    }
    current
}

pub fn section_dir(section_path: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", section_path, SECTION_SUFFIX))
}

fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta")
}

fn group_path(dir: &Path, group: u64) -> PathBuf {
    dir.join(format!("g{:08x}", group))
}

pub fn is_hashfile(section_path: &str) -> bool {
    meta_path(&section_dir(section_path)).exists()
}

pub fn read_meta(section_path: &str) -> Option<SectionMeta> {
    let content = fs::read_to_string(meta_path(&section_dir(section_path))).ok()?;
    let mut meta = SectionMeta::empty();
    for line in content.lines() {
        let (key, value) = line.split_once('=')?;
        let value: u64 = value.trim().parse().ok()?;
        match key.trim() {
            "version" => meta.version = value,
            "modulus" => meta.modulus = value,
            "records" => meta.records = value,
            _ => {}
        }
    }
    if meta.modulus == 0 { meta.modulus = MIN_MODULUS; }
    Some(meta)
}

fn write_meta(dir: &Path, meta: SectionMeta) -> io::Result<()> {
    let tmp = dir.join("meta.tmp");
    {
        let mut file = File::create(&tmp)?;
        write!(
            file,
            "version={}\nmodulus={}\nrecords={}\n",
            meta.version, meta.modulus, meta.records
        )?;
        file.flush()?;
    }
    fs::rename(tmp, meta_path(dir))
}

/// Reads the `[key_len][key][data_len][data]` frames of one file into `map`.
pub fn read_frames(map: &mut HashMap<String, Record>, path: &Path) -> io::Result<()> {
    if !path.exists() { return Ok(()); }
    let mut reader = BufReader::new(File::open(path)?);

    loop {
        let mut len_bytes = [0u8; 8];
        if let Err(e) = reader.read_exact(&mut len_bytes) {
            if e.kind() == io::ErrorKind::UnexpectedEof { break; }
            return Err(e);
        }
        let key_len = u64::from_le_bytes(len_bytes) as usize;
        if key_len > MAX_KEY_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Key too large: {} bytes in {}", key_len, path.display()),
            ));
        }
        let mut key_bytes = vec![0u8; key_len];
        reader.read_exact(&mut key_bytes)?;
        let key = String::from_utf8_lossy(&key_bytes).to_string();

        let mut data_len_bytes = [0u8; 8];
        reader.read_exact(&mut data_len_bytes)?;
        let data_len = u64::from_le_bytes(data_len_bytes) as usize;
        if data_len > MAX_DATA_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Record too large: {} bytes for key '{}' in {}", data_len, key, path.display()),
            ));
        }

        let mut data = vec![0u8; data_len];
        reader.read_exact(&mut data)?;
        map.insert(key, Record::from_bytes(&data));
    }
    Ok(())
}

/// Writes `keys` (and their records) as frames, atomically replacing `path`.
/// An empty group leaves no file behind at all.
fn write_frames(path: &Path, map: &HashMap<String, Record>, keys: &mut Vec<&String>) -> io::Result<()> {
    if keys.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    keys.sort();

    let tmp = path.with_extension("tmp");
    {
        let mut writer = BufWriter::new(File::create(&tmp)?);
        for key in keys.iter() {
            let record = &map[*key];
            let key_bytes = key.as_bytes();
            writer.write_all(&(key_bytes.len() as u64).to_le_bytes())?;
            writer.write_all(key_bytes)?;

            let data = record.to_bytes();
            writer.write_all(&(data.len() as u64).to_le_bytes())?;
            writer.write_all(&data)?;
        }
        writer.flush()?;
    }
    fs::rename(tmp, path)
}

/// Loads every group of a hashed section.
pub fn load(section_path: &str, map: &mut HashMap<String, Record>) -> io::Result<SectionMeta> {
    let dir = section_dir(section_path);
    let meta = read_meta(section_path).unwrap_or_else(SectionMeta::empty);
    for group in 0..meta.modulus {
        read_frames(map, &group_path(&dir, group))?;
    }
    Ok(meta)
}

/// Persists a section.
///
/// `dirty_keys` is the set of keys touched since the last flush. When it is
/// `Some`, only the groups those keys hash into are rewritten, which is the
/// whole point of the format. `None` (or a modulus change) forces a full
/// rewrite, which is what migration and bulk edits need.
pub fn save(
    section_path: &str,
    map: &HashMap<String, Record>,
    previous: SectionMeta,
    dirty_keys: Option<&HashSet<String>>,
    per_group: usize,
) -> io::Result<SectionMeta> {
    let dir = section_dir(section_path);
    fs::create_dir_all(&dir)?;

    let records = map.len() as u64;
    let modulus = plan_modulus(previous.modulus, records, per_group);
    let full_rewrite = dirty_keys.is_none() || modulus != previous.modulus || previous.version == 0;

    if full_rewrite {
        let mut buckets: HashMap<u64, Vec<&String>> = (0..modulus).map(|g| (g, Vec::new())).collect();
        for key in map.keys() {
            if let Some(bucket) = buckets.get_mut(&group_of(key, modulus)) {
                bucket.push(key);
            }
        }
        for (group, mut keys) in buckets {
            write_frames(&group_path(&dir, group), map, &mut keys)?;
        }
        remove_groups_beyond(&dir, modulus)?;
    } else {
        // Read-modify-write one group at a time. Deriving each group's contents
        // from the file rather than by filtering the in-memory table is what
        // makes a write cost O(group) instead of O(table): nothing here scales
        // with how many records the table holds.
        let mut by_group: HashMap<u64, Vec<&String>> = HashMap::new();
        for key in dirty_keys.unwrap() {
            by_group.entry(group_of(key, modulus)).or_default().push(key);
        }
        for (group, keys) in by_group {
            let path = group_path(&dir, group);
            let mut contents: HashMap<String, Record> = HashMap::new();
            read_frames(&mut contents, &path)?;
            for key in keys {
                match map.get(key) {
                    Some(record) => contents.insert(key.clone(), record.clone()),
                    None => contents.remove(key),
                };
            }
            let mut group_keys: Vec<&String> = contents.keys().collect();
            write_frames(&path, &contents, &mut group_keys)?;
        }
    }

    let meta = SectionMeta { version: previous.version + 1, modulus, records };
    write_meta(&dir, meta)?;
    Ok(meta)
}

/// Deletes group files left over from a larger modulus.
fn remove_groups_beyond(dir: &Path, modulus: u64) -> io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(name) => name,
            None => continue,
        };
        if !name.starts_with('g') { continue; }
        if let Ok(group) = u64::from_str_radix(&name[1..], 16) {
            if group >= modulus {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// Group file sizes, smallest first. Used by tests and diagnostics to confirm
/// the modulus really is spreading records instead of piling them up.
pub fn group_sizes(section_path: &str) -> Vec<u64> {
    let dir = section_dir(section_path);
    let mut sizes = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_str().unwrap_or_default().to_string();
            if name.starts_with('g') && !name.ends_with(".tmp") {
                if let Ok(meta) = entry.metadata() {
                    sizes.push(meta.len());
                }
            }
        }
    }
    sizes.sort();
    sizes
}
