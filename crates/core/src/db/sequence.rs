//! Minted keys: the counter two file types allocate their record ids from, and
//! the small checksummed file it survives a restart in.
//!
//! A queue file mints the key of every enqueued record, and an autokey file
//! mints the key of every keyless `WRITE`. Those are the same problem - hand
//! out an identifier nobody else will get, in arrival order, without reusing
//! one across a restart - so they are the same counter, and this is it.
//!
//! # The shape of a key
//!
//! Twenty decimal digits:
//!
//! ```text
//!  01764950412345 000001
//!  ^ milliseconds ^ counter within that millisecond
//! ```
//!
//! `milliseconds * 1_000_000 + counter`, zero padded to a fixed width so the
//! keys sort into arrival order **as text as well as as numbers**. The width is
//! part of the interface rather than an implementation detail: a caller reading
//! a key range depends on it, and a width chosen per file would make the first
//! file that outgrew it a migration.
//!
//! Two consequences are worth stating plainly. The sequence is forced upwards
//! ([`Sequence::mint`]), so a clock that steps backwards still yields keys in
//! arrival order, but the time those keys carry is behind the wall clock until
//! it catches up. And a millisecond holds a million keys; minting faster than
//! that borrows from the next millisecond rather than colliding.
//!
//! # Why the clock is in the key
//!
//! It is what lets a queue read the oldest unacknowledged age off its smallest
//! live key rather than from a timestamp stored per record - and that is the
//! difference between a queue whose persistent state is the size of its
//! in-flight set and one whose state is the size of its depth. An autokey file
//! gets the same property for free: the key says when the record arrived.
//!
//! It also means the counter is safe across a restart *even when its persisted
//! state is lost*, because the clock has moved on in the meantime. The state
//! file below is what covers the remaining case, a clock that has stepped back.
//!
//! # The state file
//!
//! Both counters persist into a small text file inside the file's own
//! directory - `queue` for a queue, `autokey` for an autokey file - in the
//! format [`crate::db::statefile`] writes. A file that does not check out is
//! treated as absent here: the records are the file, and this only says where
//! the counter had got to, so starting it over costs a gap in the keys rather
//! than anything a caller can see.

use crate::db::hashfile::FsyncPolicy;
use crate::db::statefile;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Digits in a minted key. Twenty holds `u64::MAX`, so every key this mints is
/// the same width and text order is numeric order.
pub const KEY_DIGITS: usize = 20;

/// Sequence numbers per millisecond. The low part of a key is a counter within
/// its millisecond; the high part is the millisecond itself.
pub const SUB_MILLISECOND: u64 = 1_000_000;

/// A sequence number as the key it is stored under.
pub fn format_key(sequence: u64) -> String {
    format!("{:0width$}", sequence, width = KEY_DIGITS)
}

/// The sequence number a key carries, or `None` for a key this did not mint.
///
/// Written by hand into a file that also mints keys, a key that is not a
/// sequence number is still a perfectly good record - it simply has no place in
/// the order, which is what the callers use this to find out.
pub fn key_sequence(key: &str) -> Option<u64> {
    if key.len() != KEY_DIGITS || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u64>().ok()
}

/// When the record under `key` was minted, in milliseconds since the epoch.
pub fn key_millis(key: &str) -> Option<u64> {
    key_sequence(key).map(|sequence| sequence / SUB_MILLISECOND)
}

/// Milliseconds since the epoch, saturating rather than panicking on a clock
/// set before it.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// The counter one file mints its keys from.
///
/// Never decreases, whatever the clock does, and remembers whether the state
/// file beside the records is still behind it - which is what stops a flush
/// that changed no record from leaving the counter where a restart would mint a
/// key twice.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sequence {
    next: u64,
    dirty: bool,
}

impl Sequence {
    /// A counter that has not minted anything and has nothing to persist.
    pub fn new() -> Self {
        Sequence::default()
    }

    /// A counter restored from what was persisted, clean because it says
    /// exactly what the state file does.
    pub fn restored(next: u64) -> Self {
        Sequence { next, dirty: false }
    }

    /// The next sequence number, advanced to the current millisecond when the
    /// clock has moved on and forced upwards when it has not.
    pub fn mint(&mut self, now_millis: u64) -> u64 {
        let sequence = self.next.max(now_millis.saturating_mul(SUB_MILLISECOND));
        self.next = sequence.saturating_add(1);
        self.dirty = true;
        sequence
    }

    /// Pulls the counter past a key that is already there.
    ///
    /// Called for every key present when the file is loaded, so a state file
    /// that was lost or is behind cannot mint a key that collides with a record
    /// that still exists. It does not dirty the counter: raising it to describe
    /// records that are already on disk discovers state rather than changing
    /// it, and the next `mint` is what makes it worth writing down.
    pub fn raise_past(&mut self, key: &str) {
        if let Some(sequence) = key_sequence(key) {
            self.next = self.next.max(sequence.saturating_add(1));
        }
    }

    /// The sequence number this would mint next. Reported by `FILE.STATS`; the
    /// minting path reads it through [`mint`](Self::mint).
    pub fn peek(&self) -> u64 {
        self.next
    }

    /// Whether the state file is behind what is held here.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }
}

/// The `autokey` file inside a file's directory.
pub fn autokey_path(file_dir: &str) -> PathBuf {
    Path::new(file_dir).join("autokey")
}

/// Reads an autokey file's counter, or `None` when there is none to read.
pub fn read_autokey(file_dir: &str) -> Option<u64> {
    let body = statefile::read(&autokey_path(file_dir)).body()?;
    statefile::field(&body, "next")?.parse().ok()
}

/// Writes an autokey file's counter.
///
/// Written after the records, exactly as a queue's book and an index's `state`
/// are. A counter ahead of the records costs a gap in the keys; a counter
/// behind them would mint one twice, which is the lost record this exists to
/// prevent - and [`Sequence::raise_past`] is the second line of defence for it.
pub fn write_autokey(file_dir: &str, next: u64, fsync: FsyncPolicy) -> io::Result<()> {
    statefile::write(&autokey_path(file_dir), &format!("next={}\n", next), fsync)
}

/// Removes an autokey file's counter, for a file that no longer mints keys.
pub fn remove_autokey(file_dir: &str) -> io::Result<()> {
    statefile::remove_if_present(&autokey_path(file_dir))
}
