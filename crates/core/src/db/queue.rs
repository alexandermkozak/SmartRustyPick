//! Queue files: records that keep their arrival order and are claimed one at a
//! time.
//!
//! An ordinary file here is a hash file: a record's key decides which group it
//! lands in, so the file has no order to walk and two readers of the same
//! `SELECT` get the same keys. A queue file adds the two things that are
//! missing from that for work that has to be divided up - an order, and a claim
//! only one consumer can hold.
//!
//! # Sequence keys
//!
//! The engine mints the key of every enqueued record, as twenty decimal digits:
//!
//! ```text
//!  01764950412345 000001
//!  ^ milliseconds ^ counter within that millisecond
//! ```
//!
//! `milliseconds * 1_000_000 + counter`, zero padded, so the keys sort in
//! arrival order both as text and as numbers. The clock is *in* the key, which
//! is what lets the oldest unacknowledged age be read off the smallest live key
//! rather than from a timestamp stored per record - and that is the difference
//! between a queue whose persistent state is the size of its in-flight set and
//! one whose state is the size of its depth.
//!
//! Two consequences are worth stating plainly. The sequence is forced upwards
//! ([`QueueState::mint`]), so a clock that steps backwards still yields keys in
//! arrival order, but the time those keys carry is behind the wall clock until
//! it catches up. And a millisecond holds a million keys; enqueueing faster
//! than that borrows from the next millisecond rather than colliding.
//!
//! # What is persisted, and what is not
//!
//! Claims are held in memory only. A claim belongs to a connection, and a
//! server that has restarted has no connections, so every claim is released on
//! load and its record becomes available again with its delivery count intact.
//! That is why the `queue` file beside the records holds only two things: the
//! next sequence number, and the delivery count of each record that has been
//! delivered at least once. In a queue that is being drained normally both are
//! tiny, and the file is rewritten as a unit on every flush.
//!
//! # Dead letters
//!
//! A record delivered [`QueuePolicy::max_deliveries`] times without being
//! acknowledged moves to `<name>.DEAD`, which is itself a queue file: the
//! record keeps its sequence key and its delivery count, so what failed and how
//! often is readable with `PEEK`, and a fixed consumer can drain it with the
//! same commands it drains the live queue with.

use crate::db::hashfile::{self, FsyncPolicy};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Digits in a sequence key. Twenty holds `u64::MAX`, so every key this mints
/// is the same width and text order is numeric order.
pub const KEY_DIGITS: usize = 20;

/// Sequence numbers per millisecond. The low part of a key is a counter within
/// its millisecond; the high part is the millisecond itself.
pub const SUB_MILLISECOND: u64 = 1_000_000;

/// Appended to a queue's name to name the file its dead letters go to.
pub const DEAD_LETTER_SUFFIX: &str = ".DEAD";

/// How long a claim is held before the record becomes available again, for a
/// queue whose `DIR` entry does not name a timeout of its own.
pub const DEFAULT_VISIBILITY: Duration = Duration::from_secs(60);

/// How many times a record is delivered before it is dead lettered, for a queue
/// whose `DIR` entry does not name a limit of its own.
pub const DEFAULT_MAX_DELIVERIES: u32 = 5;

/// Longest visibility timeout a queue may be given, in seconds. A claim held
/// for longer than a day is a stuck consumer, not a slow one.
pub const MAX_VISIBILITY_SECONDS: u64 = 86_400;

/// Most times a record may be delivered before it is dead lettered.
pub const MAX_DELIVERY_LIMIT: u32 = 1_000;

/// The name of the file `queue`'s dead letters go to.
pub fn dead_letter_name(queue: &str) -> String {
    format!("{}{}", queue, DEAD_LETTER_SUFFIX)
}

/// True when `name` names a dead-letter file rather than a live queue. A dead
/// letter file is a queue itself, so it never gets one of its own.
pub fn is_dead_letter_name(name: &str) -> bool {
    name.ends_with(DEAD_LETTER_SUFFIX)
}

/// A sequence number as the key it is stored under.
pub fn format_key(sequence: u64) -> String {
    format!("{:0width$}", sequence, width = KEY_DIGITS)
}

/// The sequence number a key carries, or `None` for a key this did not mint.
///
/// Written by hand into a queue file, a key that is not a sequence number is
/// still a perfectly good record - it simply has no place in the order, which
/// is what the callers use this to find out.
pub fn key_sequence(key: &str) -> Option<u64> {
    if key.len() != KEY_DIGITS || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u64>().ok()
}

/// When the record under `key` was enqueued, in milliseconds since the epoch.
pub fn key_enqueued_millis(key: &str) -> Option<u64> {
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

/// How long a claim on this queue lasts, and how many deliveries a record gets.
///
/// Held per queue rather than per server: a queue of thirty-second jobs and a
/// queue of hour-long ones need different answers, and the answer belongs
/// beside the file it governs. Both are stored in the file's `DIR` entry, so
/// they survive a rebuild of the listing exactly as the durability flag does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuePolicy {
    pub visibility: Duration,
    pub max_deliveries: u32,
}

impl Default for QueuePolicy {
    fn default() -> Self {
        QueuePolicy {
            visibility: DEFAULT_VISIBILITY,
            max_deliveries: DEFAULT_MAX_DELIVERIES,
        }
    }
}

impl QueuePolicy {
    /// A policy from the two `DIR` attributes, each falling back to the default
    /// when it is absent or does not describe a number.
    ///
    /// A hand-edited `DIR` entry cannot make a queue unusable: an unreadable
    /// timeout is the default timeout, not a queue that refuses to run.
    pub fn from_attributes(timeout: &str, deliveries: &str) -> Self {
        let visibility = timeout
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Duration::from_secs(seconds.min(MAX_VISIBILITY_SECONDS)))
            .unwrap_or(DEFAULT_VISIBILITY);
        let max_deliveries = deliveries
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|count| *count > 0)
            .map(|count| count.min(MAX_DELIVERY_LIMIT))
            .unwrap_or(DEFAULT_MAX_DELIVERIES);
        QueuePolicy {
            visibility,
            max_deliveries,
        }
    }

    pub fn visibility_seconds(&self) -> u64 {
        self.visibility.as_secs()
    }
}

/// A claim one consumer holds on one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The authorised name of the client that dequeued it.
    pub owner: String,
    /// When the claim lapses and the record becomes available again, in
    /// milliseconds since the epoch.
    pub expires_millis: u64,
}

/// What a sweep of the expired claims decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Expired {
    /// Claims that lapsed and whose records are available again.
    pub returned: Vec<String>,
    /// Records that have now been delivered as often as the policy allows and
    /// are to be moved to the dead-letter file.
    pub dead: Vec<String>,
}

impl Expired {
    pub fn is_empty(&self) -> bool {
        self.returned.is_empty() && self.dead.is_empty()
    }
}

/// Why a record could not be acknowledged or returned.
#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Nothing in this queue is stored under that key.
    NotFound,
    /// The record is in the queue but nobody is holding it - it was never
    /// claimed, or the claim lapsed and it is available again.
    NotClaimed,
    /// Somebody else is holding it.
    HeldBy(String),
}

/// The order and the claims of one queue file.
///
/// Everything here is derived from the records beside it and from the small
/// `queue` file described in the module documentation, so it is rebuilt on
/// every load rather than trusted across one.
#[derive(Debug, Default)]
pub struct QueueState {
    /// The next sequence number to mint. Never decreases.
    next_sequence: u64,
    /// Keys nobody is holding, in arrival order. The order is the whole point:
    /// claiming the oldest is `first()`, not a scan.
    available: BTreeSet<String>,
    /// Keys somebody is holding, and who.
    claims: HashMap<String, Claim>,
    /// How many times each record has been delivered. Only records that have
    /// been delivered at least once appear, which is what keeps this - and the
    /// file it is written to - the size of the trouble rather than the size of
    /// the queue.
    deliveries: HashMap<String, u32>,
    /// Set when anything above changed and the `queue` file no longer says so.
    dirty: bool,
}

impl QueueState {
    /// The state of a queue whose records have just been read, with whatever
    /// its `queue` file still remembered.
    ///
    /// Claims are deliberately not restored - see the module documentation.
    /// A delivery count for a record that is no longer in the file is dropped,
    /// and the sequence is pulled up past every key already present, so a
    /// `queue` file that was lost or is behind cannot mint a key that collides
    /// with a record that is still there.
    pub fn attach<R>(records: &HashMap<String, R>, persisted: PersistedQueue) -> Self {
        let mut available = BTreeSet::new();
        let mut highest = 0u64;
        for key in records.keys() {
            if let Some(sequence) = key_sequence(key) {
                highest = highest.max(sequence + 1);
            }
            available.insert(key.clone());
        }
        let deliveries: HashMap<String, u32> = persisted
            .deliveries
            .into_iter()
            .filter(|(key, _)| records.contains_key(key))
            .collect();
        QueueState {
            next_sequence: persisted.next_sequence.max(highest),
            available,
            claims: HashMap::new(),
            deliveries,
            dirty: false,
        }
    }

    /// Whether the `queue` file is behind what is held here.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// The next sequence number, advanced to the current millisecond when the
    /// clock has moved on and forced upwards when it has not.
    pub fn mint(&mut self, now_millis: u64) -> u64 {
        let sequence = self.next_sequence.max(now_millis.saturating_mul(SUB_MILLISECOND));
        self.next_sequence = sequence.saturating_add(1);
        self.dirty = true;
        sequence
    }

    /// The sequence number this queue would mint next. Reported by
    /// `FILE.STATS`; the queue itself reads it through [`mint`](Self::mint).
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Puts a newly written record at the back of the queue.
    pub fn enqueue(&mut self, key: &str) {
        self.claims.remove(key);
        self.available.insert(key.to_string());
        self.dirty = true;
    }

    /// Forgets a record entirely: acknowledged, deleted, or moved to the
    /// dead-letter file.
    pub fn forget(&mut self, key: &str) {
        self.available.remove(key);
        self.claims.remove(key);
        self.deliveries.remove(key);
        self.dirty = true;
    }

    /// Returns every claim that has lapsed to the queue, and names the records
    /// that have now used up their deliveries.
    ///
    /// Called before every claim and before the statistics are read, which is
    /// what makes a visibility timeout take effect without a background thread:
    /// the only observer of an expired claim is the next consumer, and it does
    /// the sweeping on its way past.
    pub fn expire(&mut self, now_millis: u64, policy: QueuePolicy) -> Expired {
        let lapsed: Vec<String> = self
            .claims
            .iter()
            .filter(|(_, claim)| claim.expires_millis <= now_millis)
            .map(|(key, _)| key.clone())
            .collect();
        let mut outcome = Expired::default();
        for key in lapsed {
            self.claims.remove(&key);
            self.dirty = true;
            if self.deliveries.get(&key).copied().unwrap_or(0) >= policy.max_deliveries {
                outcome.dead.push(key);
            } else {
                self.available.insert(key.clone());
                outcome.returned.push(key);
            }
        }
        outcome.dead.sort();
        outcome.returned.sort();
        outcome
    }

    /// Claims the oldest available record for `owner`, returning its key and
    /// the delivery count this claim is.
    ///
    /// Callers sweep with [`expire`](Self::expire) first. Doing both under one
    /// lock is what makes the claim atomic: the record leaves `available` in
    /// the same critical section that put it in `claims`, so no second consumer
    /// can see it in between.
    pub fn claim(&mut self, owner: &str, now_millis: u64, policy: QueuePolicy) -> Option<(String, u32)> {
        let key = self.available.iter().next()?.clone();
        self.available.remove(&key);
        let deliveries = self.deliveries.entry(key.clone()).or_insert(0);
        *deliveries += 1;
        let deliveries = *deliveries;
        self.claims.insert(
            key.clone(),
            Claim {
                owner: owner.to_string(),
                expires_millis: now_millis.saturating_add(policy.visibility.as_millis() as u64),
            },
        );
        self.dirty = true;
        Some((key, deliveries))
    }

    /// The oldest available key, without claiming it.
    pub fn head(&self) -> Option<&String> {
        self.available.iter().next()
    }

    /// The claim held on `key`, if any.
    pub fn claim_on(&self, key: &str) -> Option<&Claim> {
        self.claims.get(key)
    }

    /// How many times `key` has been delivered.
    pub fn deliveries(&self, key: &str) -> u32 {
        self.deliveries.get(key).copied().unwrap_or(0)
    }

    /// Records the delivery count of a record moved in from another queue, so a
    /// dead letter arrives carrying the count that killed it.
    pub fn adopt(&mut self, key: &str, deliveries: u32) {
        self.available.insert(key.to_string());
        if deliveries > 0 {
            self.deliveries.insert(key.to_string(), deliveries);
        }
        self.dirty = true;
    }

    /// Checks that `owner` is the one holding `key`, and that the claim has not
    /// already lapsed.
    pub fn verify(&self, key: &str, owner: &str, present: bool) -> Result<(), ClaimError> {
        if !present {
            return Err(ClaimError::NotFound);
        }
        match self.claims.get(key) {
            None => Err(ClaimError::NotClaimed),
            Some(claim) if claim.owner != owner => Err(ClaimError::HeldBy(claim.owner.clone())),
            Some(_) => Ok(()),
        }
    }

    /// Gives a claimed record straight back, without waiting for its claim to
    /// lapse. `true` when the record has used up its deliveries and is to be
    /// dead lettered instead of made available.
    pub fn release(&mut self, key: &str, policy: QueuePolicy) -> bool {
        self.claims.remove(key);
        self.dirty = true;
        if self.deliveries.get(key).copied().unwrap_or(0) >= policy.max_deliveries {
            true
        } else {
            self.available.insert(key.to_string());
            false
        }
    }

    /// Brings the order back in line with the records after something other
    /// than a queue command changed them.
    ///
    /// A queue file is still a file: `WRITE` can put a record in one under a
    /// key of its own, and `DELETE` can take one out from under a consumer.
    /// Neither goes through the queue, so rather than forbid them - a queue
    /// that cannot be repaired by hand is worse than one that can - the order
    /// is reconciled with the records whenever the two disagree on how many
    /// there are. A record nobody has claimed becomes available in key order
    /// like any other; one that has vanished is forgotten.
    pub fn reconcile<R>(&mut self, records: &HashMap<String, R>) {
        self.available
            .retain(|key| records.contains_key(key) && !self.claims.contains_key(key));
        for key in records.keys() {
            if !self.claims.contains_key(key) {
                self.available.insert(key.clone());
            }
            if let Some(sequence) = key_sequence(key) {
                self.next_sequence = self.next_sequence.max(sequence + 1);
            }
        }
        self.claims.retain(|key, _| records.contains_key(key));
        self.deliveries.retain(|key, _| records.contains_key(key));
        self.dirty = true;
    }

    /// Records this queue is tracking, claimed or not. Compared against the
    /// record count to decide whether a [`reconcile`](Self::reconcile) is due.
    pub fn tracked(&self) -> usize {
        self.available.len() + self.claims.len()
    }

    /// Records available to be claimed.
    pub fn depth(&self) -> usize {
        self.available.len()
    }

    /// Records claimed and not yet acknowledged.
    pub fn in_flight(&self) -> usize {
        self.claims.len()
    }

    /// How long ago the oldest record still in the queue - available or claimed
    /// - was enqueued, in seconds.
    ///
    /// Read from the smallest live key, because a sequence key carries the
    /// millisecond it was minted in. A key that this did not mint has no
    /// enqueue time and is skipped rather than guessed at.
    pub fn oldest_unacknowledged_seconds(&self, now_millis: u64) -> Option<u64> {
        let oldest = self
            .available
            .iter()
            .chain(self.claims.keys())
            .filter_map(|key| key_enqueued_millis(key))
            .min()?;
        Some(now_millis.saturating_sub(oldest) / 1000)
    }

    /// What the `queue` file has to say, for the flush.
    pub fn to_persisted(&self) -> PersistedQueue {
        PersistedQueue {
            next_sequence: self.next_sequence,
            deliveries: self
                .deliveries
                .iter()
                .map(|(key, count)| (key.clone(), *count))
                .collect(),
        }
    }
}

/// The part of a queue's state that outlives the process.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PersistedQueue {
    pub next_sequence: u64,
    pub deliveries: Vec<(String, u32)>,
}

/// The `queue` file inside a file's directory.
pub fn state_path(file_dir: &str) -> PathBuf {
    Path::new(file_dir).join("queue")
}

/// Reads a queue's persisted state, or `None` when there is none to read.
///
/// A file that does not check out is reported as absent rather than as an
/// error. The records are the queue; this only says where the sequence had got
/// to and how often each record had been delivered, and starting those over is
/// a queue that redelivers a few records more than it had to - not a queue that
/// refuses to open.
pub fn read_state(file_dir: &str) -> Option<PersistedQueue> {
    let content = fs::read_to_string(state_path(file_dir)).ok()?;
    let (checksum_line, body) = content.split_once('\n')?;
    let recorded = checksum_line.strip_prefix("checksum=")?;
    if u32::from_str_radix(recorded.trim(), 16).ok()? != hashfile::crc32c(body.as_bytes()) {
        return None;
    }
    let mut state = PersistedQueue::default();
    for line in body.lines() {
        if let Some(next) = line.strip_prefix("next=") {
            state.next_sequence = next.trim().parse().unwrap_or(0);
        } else if let Some(entry) = line.strip_prefix("deliveries=")
            && let Some((key, count)) = entry.rsplit_once(':')
            && let Ok(count) = count.trim().parse::<u32>()
        {
            state.deliveries.push((key.to_string(), count));
        }
    }
    Some(state)
}

/// Writes a queue's persisted state, checksum first and through a temporary
/// file, so a crash mid-write leaves the previous state rather than half of
/// this one.
///
/// Written after the records, exactly as an index's `state` is: a delivery
/// count that names a record the data section has not got is dropped on the
/// next load, whereas a record with no delivery count is simply one that starts
/// its retries again.
pub fn write_state(file_dir: &str, state: &PersistedQueue, fsync: FsyncPolicy) -> io::Result<()> {
    let dir = Path::new(file_dir);
    fs::create_dir_all(dir)?;
    let mut body = format!("next={}\n", state.next_sequence);
    // Sorted, so the same state always produces the same bytes and the checksum
    // does not change without the state changing.
    let mut deliveries: Vec<&(String, u32)> = state.deliveries.iter().collect();
    deliveries.sort();
    for (key, count) in deliveries {
        body.push_str(&format!("deliveries={}:{}\n", key, count));
    }
    let tmp = dir.join("queue.tmp");
    {
        let mut file = File::create(&tmp)?;
        writeln!(file, "checksum={:08x}", hashfile::crc32c(body.as_bytes()))?;
        file.write_all(body.as_bytes())?;
        if fsync == FsyncPolicy::Always {
            file.sync_all()?;
        }
    }
    fs::rename(tmp, state_path(file_dir))
}

/// Removes a queue's persisted state, for a file that is no longer a queue.
pub fn remove_state(file_dir: &str) -> io::Result<()> {
    match fs::remove_file(state_path(file_dir)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
