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
//! Each group ends with a fixed size trailer holding a magic, the record count
//! and a CRC32C of everything before it, so a torn write is detected instead of
//! being read back as "these records simply do not exist".
//!
//! Durability is ordered: every group is made durable *before* `meta` is
//! rewritten, so `meta.version` only ever advances once the data it describes
//! is on disk. How much of that ordering is paid for with real `fsync` calls is
//! chosen by [`FsyncPolicy`].

use crate::db::models::Record;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
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

/// Marks a group file that carries a trailer. Sits at a fixed offset from the
/// end of the file, so it can be found without reading the frames first.
const GROUP_MAGIC: [u8; 8] = *b"SRPHFG01";

/// `magic` + record count + CRC32C.
const TRAILER_LEN: usize = 8 + 8 + 4;

/// How much of a flush is forced all the way to the platter.
///
/// The `.tmp` + `rename` dance only ever gave *atomic replacement*: the rename
/// can be visible while the data blocks are not. These policies decide how much
/// of that gap is closed, because closing it entirely costs throughput that the
/// buffered write path does not want to pay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    /// Every rewritten group is `sync_all`ed before its rename, the directory
    /// is synced after the renames, and `meta` is synced after that. An
    /// acknowledged write survives a power loss.
    Always,
    /// Group files are left to the page cache, but the directory and `meta` are
    /// synced. The namespace and the metadata itself cannot be torn; the record
    /// data still can.
    Meta,
    /// No syncing at all. Fastest, and only as safe as the page cache.
    Never,
}

impl Default for FsyncPolicy {
    /// `Never`, which is what the buffered path has always done. Syncing is a
    /// hundredfold cost on a real disk (see `docs/storage.md`), so it is opted
    /// into globally - a file marked durable gets it regardless.
    fn default() -> Self {
        FsyncPolicy::Never
    }
}

impl FsyncPolicy {
    /// Parses the `fsync` config value. Anything unrecognised falls back to the
    /// default rather than failing a database open.
    pub fn from_config(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("always") => FsyncPolicy::Always,
            Some("meta") => FsyncPolicy::Meta,
            Some("never") | Some("none") | Some("off") => FsyncPolicy::Never,
            _ => FsyncPolicy::default(),
        }
    }

    fn syncs_groups(self) -> bool {
        self == FsyncPolicy::Always
    }

    fn syncs_meta(self) -> bool {
        self != FsyncPolicy::Never
    }
}

/// CRC32C (Castagnoli) lookup table, built at compile time so the checksum
/// costs a byte-per-table-lookup and no dependency is pulled in for it.
const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// CRC32C of `data`. Cheap, and strong enough to catch the truncated or
/// half-written tails this format has to worry about.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ *byte as u32) & 0xFF) as usize];
    }
    !crc
}

/// `fsync` of a directory, so a rename inside it is durable.
///
/// Not every filesystem allows it; a refusal is not a reason to fail a write
/// that otherwise succeeded, so those errors are swallowed.
fn sync_dir(dir: &Path) -> io::Result<()> {
    match File::open(dir) {
        Ok(handle) => match handle.sync_all() {
            Ok(()) => Ok(()),
            Err(e) if matches!(e.kind(), io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied) => Ok(()),
            Err(e) => Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes `.tmp` files left behind by a crash between `create` and `rename`.
/// They are never part of the section, so dropping them is always safe.
fn remove_stale_tmp(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(name) => name,
            None => continue,
        };
        if name.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionMeta {
    /// Incremented on every flush; lets other processes detect a change even
    /// when the filesystem timestamp resolution is too coarse to help.
    pub version: u64,
    pub modulus: u64,
    pub records: u64,
    /// True once the section has been written by a version that appends a
    /// checksum trailer to every group. Sections written before that are read
    /// leniently, so an upgrade does not declare an intact database corrupt.
    pub checksums: bool,
}

impl SectionMeta {
    pub fn empty() -> Self {
        SectionMeta {
            version: 0,
            modulus: MIN_MODULUS,
            records: 0,
            checksums: false,
        }
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

/// Reads `meta`, or `None` when it is absent *or* unreadable.
///
/// Callers that must not confuse "no section here" with "the section is
/// damaged" should use [`read_meta_checked`] instead.
pub fn read_meta(section_path: &str) -> Option<SectionMeta> {
    read_meta_checked(section_path).ok().flatten()
}

/// Reads `meta` and verifies its checksum.
///
/// `Ok(None)` means there is no `meta` file. An unparsable, truncated or
/// mis-checksummed `meta` is an error: it describes which group files exist and
/// how keys hash into them, so guessing would quietly hash records into groups
/// that do not hold them.
pub fn read_meta_checked(section_path: &str) -> io::Result<Option<SectionMeta>> {
    let path = meta_path(&section_dir(section_path));
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    let corrupt = |detail: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Corrupt section metadata in {}: {}", path.display(), detail),
        )
    };

    // Every line ends in a newline, so a file that does not was cut short
    // mid-line. Catching that here matters for a `meta` written before
    // checksums existed, which has nothing else to give the truncation away.
    if !content.ends_with('\n') {
        return Err(corrupt("truncated"));
    }

    let mut meta = SectionMeta::empty();
    let mut stored_checksum: Option<u32> = None;
    let mut seen = 0usize;
    let mut body = String::new();
    for line in content.lines() {
        let (key, value) = line.split_once('=').ok_or_else(|| corrupt("malformed line"))?;
        let key = key.trim();
        let value = value.trim();
        if key == "checksum" {
            let parsed = u32::from_str_radix(value, 16).map_err(|_| corrupt("malformed checksum"))?;
            stored_checksum = Some(parsed);
            continue;
        }
        body.push_str(line);
        body.push('\n');
        let number: u64 = value.parse().map_err(|_| corrupt("malformed value"))?;
        match key {
            "version" => {
                meta.version = number;
                seen += 1;
            }
            "modulus" => {
                meta.modulus = number;
                seen += 1;
            }
            "records" => {
                meta.records = number;
                seen += 1;
            }
            "checksums" => meta.checksums = number != 0,
            _ => {}
        }
    }

    // A truncation that happens to land on a line boundary leaves a file that
    // parses but says less than it should.
    if seen != 3 {
        return Err(corrupt("incomplete"));
    }

    // A `meta` written before checksums existed has none; one that claims to be
    // checksummed must match, or the file was torn mid-rewrite.
    if let Some(expected) = stored_checksum {
        let actual = crc32c(body.as_bytes());
        if actual != expected {
            return Err(corrupt(&format!(
                "checksum mismatch ({:08x} != {:08x})",
                actual, expected
            )));
        }
    } else if meta.checksums {
        return Err(corrupt("checksum line missing"));
    }

    if meta.modulus == 0 {
        meta.modulus = MIN_MODULUS;
    }
    Ok(Some(meta))
}

fn write_meta(dir: &Path, meta: SectionMeta, fsync: FsyncPolicy) -> io::Result<()> {
    let body = format!(
        "version={}\nmodulus={}\nrecords={}\nchecksums={}\n",
        meta.version,
        meta.modulus,
        meta.records,
        if meta.checksums { 1 } else { 0 }
    );
    let tmp = dir.join("meta.tmp");
    {
        let mut file = File::create(&tmp)?;
        // The checksum goes first, on purpose. A file cut short at a line
        // boundary would otherwise lose the checksum along with the lines it
        // covers and read back as a perfectly plausible, older `meta`.
        writeln!(file, "checksum={:08x}", crc32c(body.as_bytes()))?;
        file.write_all(body.as_bytes())?;
        file.flush()?;
        if fsync.syncs_meta() {
            file.sync_all()?;
        }
    }
    fs::rename(tmp, meta_path(dir))?;
    if fsync.syncs_meta() {
        sync_dir(dir)?;
    }
    Ok(())
}

/// Wraps a reader and keeps a running CRC32C of everything read through it, so
/// a group can be verified without holding the whole file in memory.
struct CrcReader<R: Read> {
    inner: R,
    crc: u32,
}

impl<R: Read> CrcReader<R> {
    fn new(inner: R) -> Self {
        CrcReader {
            inner,
            crc: 0xFFFF_FFFF,
        }
    }

    fn finish(self) -> u32 {
        !self.crc
    }
}

impl<R: Read> Read for CrcReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        for byte in &buf[..read] {
            self.crc = (self.crc >> 8) ^ CRC32C_TABLE[((self.crc ^ *byte as u32) & 0xFF) as usize];
        }
        Ok(read)
    }
}

fn corrupt_group(path: &Path, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Corrupt group file {}: {}", path.display(), detail),
    )
}

/// The trailer of a group file, if it has one.
fn read_trailer(path: &Path, len: u64) -> io::Result<Option<(u64, u32)>> {
    if len < TRAILER_LEN as u64 {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    file.seek(io::SeekFrom::Start(len - TRAILER_LEN as u64))?;
    let mut buf = [0u8; TRAILER_LEN];
    file.read_exact(&mut buf)?;
    if buf[..8] != GROUP_MAGIC {
        return Ok(None);
    }
    let count = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    let checksum = u32::from_le_bytes(buf[16..20].try_into().unwrap());
    Ok(Some((count, checksum)))
}

/// Reads the `[key_len][key][data_len][data]` frames of one file into `map`.
///
/// Tolerates a file without a trailer, which is what the pre-checksum format
/// and the legacy flat file look like.
pub fn read_frames(map: &mut HashMap<String, Record>, path: &Path) -> io::Result<()> {
    read_group(map, path, false)
}

/// Reads one group file.
///
/// When the file carries a trailer, the record count and the CRC32C are checked
/// and a mismatch is an error - a half-written group must never be reported as
/// a group that merely holds fewer records. `require_trailer` additionally
/// rejects a group with no trailer at all, which is how a truncation that took
/// the trailer with it is caught in a section known to be checksummed.
pub fn read_group(map: &mut HashMap<String, Record>, path: &Path, require_trailer: bool) -> io::Result<()> {
    let len = match fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let trailer = read_trailer(path, len)?;
    if trailer.is_none() && require_trailer {
        return Err(corrupt_group(path, "trailer missing or truncated"));
    }
    let body_len = match trailer {
        Some(_) => len - TRAILER_LEN as u64,
        None => len,
    };

    let mut reader = CrcReader::new(BufReader::new(File::open(path)?).take(body_len));
    let mut count: u64 = 0;
    let mut frames: Vec<(String, Record)> = Vec::new();

    loop {
        let mut len_bytes = [0u8; 8];
        match reader.read_exact(&mut len_bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
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
                format!(
                    "Record too large: {} bytes for key '{}' in {}",
                    data_len,
                    key,
                    path.display()
                ),
            ));
        }

        let mut data = vec![0u8; data_len];
        reader.read_exact(&mut data)?;
        frames.push((key, Record::from_bytes(&data)));
        count += 1;
    }

    if let Some((expected_count, expected_crc)) = trailer {
        let actual = reader.finish();
        if actual != expected_crc {
            return Err(corrupt_group(
                path,
                &format!("checksum mismatch ({:08x} != {:08x})", actual, expected_crc),
            ));
        }
        if count != expected_count {
            return Err(corrupt_group(
                path,
                &format!("record count mismatch ({} != {})", count, expected_count),
            ));
        }
    }

    // Nothing is published into `map` until the group has been vouched for, so
    // a rejected group cannot leave a half-applied table behind.
    for (key, record) in frames {
        map.insert(key, record);
    }
    Ok(())
}

/// Writes `keys` (and their records) as frames plus a trailer, atomically
/// replacing `path`. An empty group leaves no file behind at all.
fn write_frames(
    path: &Path,
    map: &HashMap<String, Record>,
    keys: &mut Vec<&String>,
    fsync: FsyncPolicy,
) -> io::Result<()> {
    if keys.is_empty() {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    keys.sort();

    let tmp = path.with_extension("tmp");
    {
        let file = File::create(&tmp)?;
        let mut writer = BufWriter::new(&file);
        let mut crc: u32 = 0xFFFF_FFFF;
        let mut write = |writer: &mut BufWriter<&File>, bytes: &[u8]| -> io::Result<()> {
            for byte in bytes {
                crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ *byte as u32) & 0xFF) as usize];
            }
            writer.write_all(bytes)
        };

        for key in keys.iter() {
            let record = &map[*key];
            let key_bytes = key.as_bytes();
            write(&mut writer, &(key_bytes.len() as u64).to_le_bytes())?;
            write(&mut writer, key_bytes)?;

            let data = record.to_bytes();
            write(&mut writer, &(data.len() as u64).to_le_bytes())?;
            write(&mut writer, &data)?;
        }

        writer.write_all(&GROUP_MAGIC)?;
        writer.write_all(&(keys.len() as u64).to_le_bytes())?;
        writer.write_all(&(!crc).to_le_bytes())?;
        writer.flush()?;
        drop(writer);
        if fsync.syncs_groups() {
            file.sync_all()?;
        }
    }
    fs::rename(tmp, path)
}

/// Loads every group of a hashed section.
///
/// A damaged `meta` or a damaged group is an error rather than an empty result:
/// reporting corruption as "no records" is the silent data loss this format is
/// meant to rule out.
pub fn load(section_path: &str, map: &mut HashMap<String, Record>) -> io::Result<SectionMeta> {
    let dir = section_dir(section_path);
    let meta = read_meta_checked(section_path)?.unwrap_or_else(SectionMeta::empty);
    // Sweep the debris of a crash between `create` and `rename` here rather
    // than on the write path: this costs one directory scan when a table is
    // opened, while doing it per flush would make a write scan every group and
    // scale with the size of the table again.
    if dir.is_dir() {
        remove_stale_tmp(&dir)?;
    }
    for group in 0..meta.modulus {
        read_group(map, &group_path(&dir, group), meta.checksums)?;
    }
    Ok(meta)
}

/// Persists a section.
///
/// `dirty_keys` is the set of keys touched since the last flush. When it is
/// `Some`, only the groups those keys hash into are rewritten, which is the
/// whole point of the format. `None` (or a modulus change) forces a full
/// rewrite, which is what migration and bulk edits need.
///
/// Uses the default [`FsyncPolicy`]; see [`save_with_fsync`] to choose one.
pub fn save(
    section_path: &str,
    map: &HashMap<String, Record>,
    previous: SectionMeta,
    dirty_keys: Option<&HashSet<String>>,
    per_group: usize,
) -> io::Result<SectionMeta> {
    save_with_fsync(
        section_path,
        map,
        previous,
        dirty_keys,
        per_group,
        FsyncPolicy::default(),
    )
}

/// [`save`], with an explicit durability policy.
///
/// The write order is deliberate: the groups are written and made durable
/// first, then `meta`. `meta.version` therefore only ever advances once the
/// data it describes is on disk, which is what lets a reader trust it after a
/// crash. The reverse order would let a surviving `meta` advertise a modulus
/// the group files do not implement, and records would hash into groups that
/// never received them.
pub fn save_with_fsync(
    section_path: &str,
    map: &HashMap<String, Record>,
    previous: SectionMeta,
    dirty_keys: Option<&HashSet<String>>,
    per_group: usize,
    fsync: FsyncPolicy,
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
            write_frames(&group_path(&dir, group), map, &mut keys, fsync)?;
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
            read_group(&mut contents, &path, previous.checksums)?;
            for key in keys {
                match map.get(key) {
                    Some(record) => contents.insert(key.clone(), record.clone()),
                    None => contents.remove(key),
                };
            }
            let mut group_keys: Vec<&String> = contents.keys().collect();
            write_frames(&path, &contents, &mut group_keys, fsync)?;
        }
    }

    // The renames themselves have to be durable before `meta` names them.
    if fsync.syncs_meta() {
        sync_dir(&dir)?;
    }

    // Only a full rewrite puts a trailer on *every* group; until then the
    // section still holds groups from before the trailer existed and must be
    // read leniently.
    let checksums = previous.checksums || full_rewrite;
    let meta = SectionMeta {
        version: previous.version + 1,
        modulus,
        records,
        checksums,
    };
    write_meta(&dir, meta, fsync)?;
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
        if !name.starts_with('g') {
            continue;
        }
        if let Ok(group) = u64::from_str_radix(&name[1..], 16)
            && group >= modulus
        {
            let _ = fs::remove_file(entry.path());
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
            if name.starts_with('g')
                && !name.ends_with(".tmp")
                && let Ok(meta) = entry.metadata()
            {
                sizes.push(meta.len());
            }
        }
    }
    sizes.sort();
    sizes
}
