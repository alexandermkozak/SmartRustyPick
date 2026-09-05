//! The table cache: what is in memory, whether it still matches the copy on
//! disk, and which one is dropped when the cache is over budget.
//!
//! Every method here answers a part of that one question - the account's file
//! listing, the disk stamps a cached table is checked against, the eviction
//! order, and the loader that fills the cache - which is why they sit together
//! rather than next to the account and statistics code they were interleaved
//! with. They are also where the `tables`, `available_tables`,
//! `available_stamps` and `lru_order` fields are read on every ordinary
//! request, so the lock order stated on [`Database`] can be checked against
//! this file by eye.

use super::{Database, TableHandle, TableKey, mlock, rlock, wlock};
use crate::db::error::{DbError, DbResult};
use crate::db::hashfile;
use crate::db::index::{self, FileIndex};
use crate::db::models::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::SystemTime;

impl Database {
    pub(super) fn ensure_available_tables(&self, account_name: &str) -> DbResult<()> {
        if self.get_account_dir(account_name).is_none() {
            let _ = self.refresh_account_registry();
        }
        let account_dir = self
            .get_account_dir(account_name)
            .ok_or_else(|| DbError::AccountNotFound(account_name.to_string()))?;

        // Re-scan whenever the account directory changed on disk, so tables created
        // by another process (e.g. the server while a local CLI is attached) are visible.
        let dir_stamp = Self::dir_modified(&account_dir);
        if rlock(&self.available_tables).contains_key(account_name)
            && rlock(&self.available_stamps).get(account_name) == Some(&dir_stamp)
        {
            return Ok(());
        }

        self.scan_available_tables(account_name)
    }

    /// True when the account is known to hold a file of this name, refreshing
    /// the listing first. Reading through the lock rather than handing out a
    /// borrow keeps the listing lock out of the caller's hands.
    pub(super) fn account_has_table(&self, account: &str, name: &str) -> bool {
        self.ensure_available_tables(account).is_ok()
            && rlock(&self.available_tables)
                .get(account)
                .map(|s| s.contains(name))
                .unwrap_or(false)
    }

    /// Every file the account holds, unsorted.
    pub(super) fn account_tables(&self, account: &str) -> DbResult<Vec<String>> {
        self.ensure_available_tables(account)?;
        Ok(rlock(&self.available_tables)
            .get(account)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default())
    }

    /// Unconditionally re-reads the account directory. Used when the cached listing
    /// does not contain a requested table, because directory mtime resolution is
    /// coarse on some filesystems and may hide a freshly created table.
    pub(super) fn scan_available_tables(&self, account_name: &str) -> DbResult<()> {
        let account_dir = self
            .get_account_dir(account_name)
            .ok_or_else(|| DbError::AccountNotFound(account_name.to_string()))?;
        let dir_stamp = Self::dir_modified(&account_dir);

        let mut tables = HashSet::new();
        if let Ok(entries) = fs::read_dir(&account_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir()
                    && let Some(name) = entry.file_name().to_str()
                {
                    tables.insert(name.to_string());
                }
            }
        }
        wlock(&self.available_tables).insert(account_name.to_string(), tables);
        wlock(&self.available_stamps).insert(account_name.to_string(), dir_stamp);
        Ok(())
    }

    fn dir_modified(path: &str) -> Option<SystemTime> {
        fs::metadata(path).ok().and_then(|m| m.modified().ok())
    }

    pub(super) fn file_stamp(path: &str) -> (Option<SystemTime>, u64) {
        match fs::metadata(path) {
            Ok(m) => (m.modified().ok(), m.len()),
            Err(_) => (None, 0),
        }
    }

    pub(super) fn disk_stamp(&self, account: &str, name: &str) -> TableStamp {
        let storage = self.account_storage_dir(account);
        let data_path = format!("{}/{}/data", storage, name);
        // A hashed section spreads its records over many files, so its identity
        // is the meta file: its flush counter changes on every write, which is
        // a stronger signal than a timestamp whose resolution may be coarse.
        let (data_modified, data_len) = match hashfile::read_meta(&data_path) {
            Some(meta) => (
                Self::file_stamp(
                    hashfile::section_dir(&data_path)
                        .join("meta")
                        .to_str()
                        .unwrap_or_default(),
                )
                .0,
                meta.version,
            ),
            None => Self::file_stamp(&data_path),
        };
        let (dict_modified, dict_len) = Self::file_stamp(&format!("{}/{}/dict", storage, name));
        TableStamp {
            data_modified,
            data_len,
            dict_modified,
            dict_len,
        }
    }

    /// The table, when it can be read through a shared reference alone: it is
    /// already in memory and either holds unflushed changes of ours or still
    /// matches the files on disk. Callers that only hold `&self` cannot load or
    /// invalidate a table, so they use this to decide whether they have to fall
    /// back to an exclusive borrow. Returning the table itself saves them a
    /// second lookup.
    pub fn table_ready_for_read(&self, account: &str, name: &str) -> Option<TableHandle> {
        let handle = self.get_table_read_only_for_account(account, name)?;
        let ready = {
            let table = handle.read();
            table.is_dirty() || table.stamp == Some(self.disk_stamp(account, name))
        };
        if ready {
            // A table served through this path is still in use, so it must not
            // look like the coldest one to the next eviction.
            self.touch_lru(&(account.to_string(), name.to_string()));
            Some(handle)
        } else {
            None
        }
    }

    /// Drops a cached table whose backing files were modified by another process,
    /// forcing a fresh read on the next access. Locally modified (dirty) tables are
    /// kept untouched so that pending changes are never silently discarded.
    fn invalidate_if_stale(&self, account: &str, name: &str) {
        let key = (account.to_string(), name.to_string());
        let handle = match rlock(&self.tables).get(&key) {
            Some(handle) => handle.clone(),
            None => return,
        };
        let stale = {
            let table = handle.read();
            !table.is_dirty() && table.stamp != Some(self.disk_stamp(account, name))
        };
        if !stale {
            return;
        }
        // Re-check under the map lock: another thread may have replaced the
        // entry, or made it dirty, since the check above.
        let mut tables = wlock(&self.tables);
        let still_stale = match tables.get(&key) {
            Some(current) => {
                let table = current.read();
                !table.is_dirty() && table.stamp != Some(self.disk_stamp(account, name))
            }
            None => false,
        };
        if still_stale {
            tables.remove(&key);
            drop(tables);
            self.forget_lru(&key);
        }
    }

    /// Moves a table to the young end of the eviction order.
    fn touch_lru(&self, key: &TableKey) {
        let mut lru = mlock(&self.lru_order);
        if let Some(pos) = lru.iter().position(|x| x == key) {
            let entry = lru.remove(pos).unwrap();
            lru.push_back(entry);
        } else {
            lru.push_back(key.clone());
        }
    }

    /// Removes a table from the eviction order entirely.
    pub(super) fn forget_lru(&self, key: &TableKey) {
        let mut lru = mlock(&self.lru_order);
        if let Some(pos) = lru.iter().position(|x| x == key) {
            lru.remove(pos);
        }
    }

    pub fn get_table_read_only(&self, name: &str) -> Option<TableHandle> {
        self.get_table_read_only_for_account(&self.current_account(), name)
    }

    /// The table if it is already in memory, without loading or refreshing it.
    pub fn get_table_read_only_for_account(&self, account: &str, name: &str) -> Option<TableHandle> {
        rlock(&self.tables)
            .get(&(account.to_string(), name.to_string()))
            .cloned()
    }

    pub fn get_table(&self, name: &str) -> Option<TableHandle> {
        self.get_table_for_account(&self.current_account(), name)
    }

    pub fn get_table_for_account(&self, account: &str, name: &str) -> Option<TableHandle> {
        self.resolve_table(account, name, false).ok()
    }

    pub fn get_table_mut(&self, name: &str) -> DbResult<TableHandle> {
        self.get_table_mut_for_account(&self.current_account(), name)
    }

    /// The table, loaded if it is not in memory yet.
    ///
    /// Named `_mut` for the callers that go on to write to it; the database
    /// itself is only borrowed shared, because the table is locked separately
    /// through the handle this returns.
    pub fn get_table_mut_for_account(&self, account: &str, name: &str) -> DbResult<TableHandle> {
        self.resolve_table(account, name, true)
    }

    /// Finds a table in the cache, loading it when it is not there.
    ///
    /// `create_missing` distinguishes the two callers: a reader wants `None`
    /// for a file whose records cannot be read, while a writer wants an empty
    /// table created for a directory that exists but holds no section yet.
    ///
    /// Two connections may load the same cold table at the same time. The
    /// second one to finish finds the first one's entry already in the map and
    /// discards its own copy, which costs a duplicate read of a file nobody had
    /// written to yet, and is what keeps the map lock off the disk.
    fn resolve_table(&self, account: &str, name: &str, create_missing: bool) -> DbResult<TableHandle> {
        let not_found = || DbError::FileNotFound {
            account: account.to_string(),
            file: name.to_string(),
        };

        self.ensure_available_tables(account)?;
        if !self.account_has_table(account, name) {
            // Might have been created by another process since the last scan.
            self.scan_available_tables(account)?;
        }
        // Strict validation: the name must be one the account listing knows,
        // and the listing's spelling of it is the one used from here on.
        let name_str = match rlock(&self.available_tables).get(account).and_then(|s| s.get(name)) {
            Some(validated) => validated.clone(),
            None => return Err(not_found()),
        };

        self.invalidate_if_stale(account, &name_str);

        let key = (account.to_string(), name_str.clone());
        if let Some(handle) = rlock(&self.tables).get(&key).cloned() {
            self.touch_lru(&key);
            return Ok(handle);
        }

        // Loading touches the disk, so it happens without the map locked.
        let table = match self.load_table_for_account(account, &name_str) {
            Ok(table) => table,
            Err(e) if e.kind() == io::ErrorKind::NotFound && create_missing => {
                let storage = self.account_storage_dir(account);
                let table_dir = format!("{}/{}", storage, name_str);
                if !Path::new(&table_dir).exists() {
                    fs::create_dir_all(&table_dir)?;
                }
                let mut table = Table::new();
                table.data_meta = Self::init_data_section(&table_dir, self.records_per_group)?;
                File::create(format!("{}/dict", table_dir))?;
                table
            }
            Err(e) => return Err(e.into()),
        };

        let handle = {
            let mut tables = wlock(&self.tables);
            tables
                .entry(key.clone())
                .or_insert_with(|| TableHandle::new(table))
                .clone()
        };
        self.touch_lru(&key);
        self.evict_if_over_budget();
        Ok(handle)
    }

    /// Writes out and drops the coldest tables once the cache is over budget.
    ///
    /// A table another thread still holds a handle to is skipped rather than
    /// evicted: dropping it from the map would let a third thread load a second
    /// copy from disk and the two would overwrite each other. The flush happens
    /// with the map still locked, for the same reason - nothing may reload the
    /// table between it leaving the cache and its changes reaching the disk.
    fn evict_if_over_budget(&self) {
        if rlock(&self.tables).len() <= self.max_loaded {
            return;
        }
        let mut tables = wlock(&self.tables);
        let mut lru = mlock(&self.lru_order);
        let mut skipped = Vec::new();
        while tables.len() > self.max_loaded {
            let key = match lru.pop_front() {
                Some(key) => key,
                None => break,
            };
            let handle = match tables.get(&key) {
                Some(handle) => handle.clone(),
                // Already gone; the order was just stale.
                None => continue,
            };
            // The map's own handle, our clone, and nothing else.
            if handle.refs() > 2 {
                skipped.push(key);
                continue;
            }
            let _ = self.flush_handle(&key, &handle);
            tables.remove(&key);
        }
        for key in skipped {
            lru.push_back(key);
        }
    }

    /// Files currently held in memory.
    pub fn loaded_table_count(&self) -> usize {
        rlock(&self.tables).len()
    }

    /// Drops every cached table without writing it out. Test support: a caller
    /// that wants to observe a load from disk needs the cache empty first.
    pub fn clear_loaded_tables(&self) {
        wlock(&self.tables).clear();
        mlock(&self.lru_order).clear();
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
        Self::load_indexes(&mut table, &format!("{}/{}", storage, name));
        Ok(table)
    }

    /// Attaches the index sections a file's directory holds.
    ///
    /// An index whose `state` does not name the data version now on disk, or
    /// whose field has moved to a different attribute, is rebuilt here from the
    /// records that were just read - which is the whole point of doing this at
    /// load time, when they are in hand. A section that cannot be read at all is
    /// treated the same way: an empty index marked for a rebuild, rather than a
    /// failed load of a file whose *records* are perfectly intact.
    fn load_indexes(table: &mut Table, file_dir: &str) {
        let data_version = if table.legacy_data { 0 } else { table.data_meta.version };
        for field in index::indexed_fields(file_dir) {
            let attr = table.field_index(&field);
            let path = index::section_path(file_dir, &field);
            let mut entries: HashMap<String, Record> = HashMap::new();
            let loaded = hashfile::load(&path, &mut entries);
            let index = match (attr, loaded) {
                (Some(attr), Ok(meta)) => FileIndex::loaded(
                    &field,
                    attr,
                    entries,
                    meta,
                    index::read_state(&path).as_ref(),
                    data_version,
                ),
                // Unreadable, or a field the dictionary no longer defines. Keep
                // the index present and stale so it is visible and rebuildable,
                // and never consulted in the meantime.
                (attr, _) => FileIndex::new(&field, attr.unwrap_or(0)),
            };
            table.indexes.insert(field, index);
        }
        table.rebuild_stale_indexes();
    }
}
