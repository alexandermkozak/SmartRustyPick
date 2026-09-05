//! The queue commands: enqueue, claim, acknowledge, return and peek.
//!
//! The storage side of a queue - the sequence keys, the claim book and what is
//! persisted - is [`crate::db::queue`]. This is what the engine does with it:
//! how a command reaches the right file, what it takes the lock for, and where
//! a record goes when it has failed once too often.
//!
//! # Why a claim is atomic
//!
//! Everything that decides who gets a record happens inside one write guard on
//! that file: the expired claims are swept back, the oldest available key is
//! taken out of the order, and the claim is recorded, without the guard being
//! released in between. A second consumer arriving at any point either has not
//! got the lock yet or is looking at a queue the record has already left, so
//! two consumers cannot come away with the same key. That is the whole of the
//! concurrency argument, and it is why these operations are written as one
//! critical section each rather than as a read followed by a write.
//!
//! # Dead letters cross two files
//!
//! Moving a record to `<name>.DEAD` touches two files, and a thread here holds
//! one table lock at a time (see the locking rules on [`Database`]). So the
//! move is done in two steps with the dead letters carried between them in
//! hand, and the dead-letter file is written and flushed *before* the record
//! leaves the queue. A crash in between therefore duplicates a dead letter
//! rather than losing a record - and because the copy keeps its original
//! sequence key, the retry that follows overwrites it rather than adding a
//! second one.

use super::{Database, TableHandle};
use crate::db::error::{DbError, DbResult};
use crate::db::models::*;
use crate::db::queue::{self, ClaimError, QueuePolicy, QueueState};
use std::time::Duration;

/// One record handed to a consumer, with what the queue knows about it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueDelivery {
    pub key: String,
    pub record: Record,
    /// Deliveries including this one.
    pub deliveries: u32,
    /// When the record was enqueued, in milliseconds since the epoch, read from
    /// its sequence key.
    pub enqueued_millis: Option<u64>,
    /// When this claim lapses, in milliseconds since the epoch. `None` for a
    /// `PEEK`, which claims nothing.
    pub expires_millis: Option<u64>,
    /// Who is holding it. `None` for a `PEEK`.
    pub owner: Option<String>,
}

/// What [`Database::queue_file`] has to do before the order can be used.
enum Readiness {
    /// Attached and matching the records.
    Ready,
    /// Attached, but the records changed underneath it through `WRITE` or
    /// `DELETE`.
    Reconcile,
    /// Loaded without an order, or only just made a queue.
    Unattached,
}

/// A record on its way to the dead-letter file.
struct DeadLetter {
    key: String,
    record: Record,
    deliveries: u32,
}

impl Database {
    /// True when the account's `DIR` marks this file a queue.
    pub fn is_table_queue_for_account(&self, account: &str, name: &str) -> bool {
        self.file_attributes_for_account(account, name).queue.is_some()
    }

    /// The claim policy of a queue file, or the defaults for a file that is not
    /// one. Callers that need to know *whether* it is a queue ask
    /// [`is_table_queue_for_account`](Self::is_table_queue_for_account).
    pub fn queue_policy_for_account(&self, account: &str, name: &str) -> QueuePolicy {
        self.file_attributes_for_account(account, name)
            .queue
            .unwrap_or_default()
    }

    /// Attaches or drops a file's queue state after its `DIR` entry changed.
    ///
    /// A file that stops being a queue loses its bookkeeping with it: the
    /// `queue` file beside the records is removed, so nothing is left on disk
    /// describing an order the file no longer has. That happens whether or not
    /// the table is in memory - the entry has already changed, and a file left
    /// on disk that only the next promotion would read is exactly the kind of
    /// state that goes stale unnoticed. A file that is not loaded picks the rest
    /// of the change up when it next is.
    pub(crate) fn reattach_queue(&self, account: &str, name: &str, queue: bool) -> DbResult<()> {
        if !queue {
            queue::remove_state(&self.file_dir(account, name))?;
        }
        let Some(handle) = self.get_table_read_only_for_account(account, name) else {
            return Ok(());
        };
        let mut table = handle.write();
        match (queue, table.queue.is_some()) {
            (true, false) => {
                let persisted = queue::read_state(&self.file_dir(account, name)).unwrap_or_default();
                table.queue = Some(QueueState::attach(&table.records, persisted));
            }
            (false, true) => table.queue = None,
            _ => {}
        }
        Ok(())
    }

    /// The policy a file is actually run under, which is its own except on a
    /// dead-letter file.
    ///
    /// A dead-letter file is the end of the line. A record that fails there has
    /// already failed everywhere else, and moving it on to `<name>.DEAD.DEAD`
    /// would bury it one level deeper each time an operator looked at it -
    /// which is the opposite of what a dead-letter file is for, since draining
    /// one is how you find out what went wrong. So its records are never dead
    /// lettered again: a `NACK` or a lapsed claim puts one back on the file it
    /// is already on, with its delivery count still rising.
    fn effective_policy(name: &str, policy: QueuePolicy) -> QueuePolicy {
        if queue::is_dead_letter_name(name) {
            QueuePolicy {
                max_deliveries: u32::MAX,
                ..policy
            }
        } else {
            policy
        }
    }

    /// The queue file a command names, refusing a file that is not one.
    ///
    /// A queue command against an ordinary file is a client error worth naming:
    /// silently treating the file as a queue would mint sequence keys into
    /// somebody's data, and silently succeeding without ordering would be
    /// worse still.
    fn queue_file(&self, account: &str, name: &str) -> DbResult<(TableHandle, QueuePolicy)> {
        let attributes = self.file_attributes_for_account(account, name);
        let Some(policy) = attributes.queue else {
            return Err(DbError::InvalidRequest(format!(
                "{} is not a queue file. Create it with CREATE.FILE {} QUEUE, or use SET.FILE to convert it",
                name, name
            )));
        };
        let handle = self.get_table_mut_for_account(account, name)?;
        // A file that became a queue while it was already in memory, or one
        // loaded before its DIR entry was read, has no ordering attached yet.
        // A file that grew or lost records through `WRITE` and `DELETE` has one
        // that no longer matches them.
        match Self::queue_readiness(&handle) {
            Readiness::Ready => {}
            Readiness::Reconcile => handle.write().reconcile_queue(),
            Readiness::Unattached => {
                let persisted = queue::read_state(&self.file_dir(account, name)).unwrap_or_default();
                let mut table = handle.write();
                // Re-checked under the exclusive guard: another connection may
                // have attached it since the read above.
                if table.queue.is_none() {
                    table.queue = Some(QueueState::attach(&table.records, persisted));
                }
            }
        }
        Ok((handle, policy))
    }

    /// Whether a loaded queue file's order can be used as it stands.
    fn queue_readiness(handle: &TableHandle) -> Readiness {
        let table = handle.read();
        match table.queue.as_ref() {
            None => Readiness::Unattached,
            Some(state) if state.tracked() != table.records.len() => Readiness::Reconcile,
            Some(_) => Readiness::Ready,
        }
    }

    /// Appends a record to a queue and returns the key the engine minted for it.
    pub fn enqueue(&self, account: &str, name: &str, record: Record) -> DbResult<String> {
        let (handle, _) = self.queue_file(account, name)?;
        let key = {
            let mut table = handle.write();
            let sequence = table
                .queue
                .as_mut()
                .expect("queue_file attaches the queue state")
                .mint(queue::now_millis());
            let key = queue::format_key(sequence);
            table.insert_record(&key, record);
            table
                .queue
                .as_mut()
                .expect("queue_file attaches the queue state")
                .enqueue(&key);
            key
        };
        self.note_write_for(account, name)?;
        Ok(key)
    }

    /// Claims the oldest unclaimed record for `owner`, or `None` when there is
    /// nothing to claim.
    ///
    /// `visibility` overrides the queue's own timeout for this one claim, for a
    /// consumer that knows this particular job is a slow one.
    pub fn dequeue(
        &self,
        account: &str,
        name: &str,
        owner: &str,
        visibility: Option<Duration>,
    ) -> DbResult<Option<QueueDelivery>> {
        let (handle, policy) = self.queue_file(account, name)?;
        let policy = QueuePolicy {
            visibility: visibility.unwrap_or(policy.visibility),
            ..Self::effective_policy(name, policy)
        };
        let now = queue::now_millis();
        let (delivery, dead) = {
            let mut table = handle.write();
            let dead = Self::take_expired(&mut table, now, policy);
            let table = &mut *table;
            let state = table.queue.as_mut().expect("queue_file attaches the queue state");
            let claimed = state.claim(owner, now, policy);
            let delivery = claimed.map(|(key, deliveries)| {
                let expires = state.claim_on(&key).map(|claim| claim.expires_millis);
                QueueDelivery {
                    record: table.records.get(&key).cloned().unwrap_or_default(),
                    enqueued_millis: queue::key_enqueued_millis(&key),
                    expires_millis: expires,
                    owner: Some(owner.to_string()),
                    deliveries,
                    key,
                }
            });
            (delivery, dead)
        };
        self.bury(account, name, dead)?;
        if delivery.is_some() {
            // A claim changes no record, but it does change the delivery count
            // the `queue` file carries, and losing that to a restart is a
            // record that starts its retries over.
            self.note_write_for(account, name)?;
        }
        Ok(delivery)
    }

    /// Consumes a claimed record: it leaves the queue for good.
    pub fn ack(&self, account: &str, name: &str, key: &str, owner: &str) -> DbResult<()> {
        let (handle, _) = self.queue_file(account, name)?;
        {
            let mut table = handle.write();
            let present = table.records.contains_key(key);
            Self::claim_check(&table, key, owner, present, name)?;
            table.remove_record(key);
            table
                .queue
                .as_mut()
                .expect("queue_file attaches the queue state")
                .forget(key);
        }
        self.note_write_for(account, name)?;
        Ok(())
    }

    /// Gives a claimed record back at once, without waiting for its claim to
    /// lapse. A record that has used up its deliveries is dead lettered here
    /// rather than made available again.
    pub fn nack(&self, account: &str, name: &str, key: &str, owner: &str) -> DbResult<()> {
        let (handle, policy) = self.queue_file(account, name)?;
        let policy = Self::effective_policy(name, policy);
        let dead = {
            let mut table = handle.write();
            let present = table.records.contains_key(key);
            Self::claim_check(&table, key, owner, present, name)?;
            let table = &mut *table;
            let state = table.queue.as_mut().expect("queue_file attaches the queue state");
            if state.release(key, policy) {
                let deliveries = state.deliveries(key);
                state.forget(key);
                let record = table.records.remove(key).unwrap_or_default();
                table.mark_dirty(key);
                vec![DeadLetter {
                    key: key.to_string(),
                    record,
                    deliveries,
                }]
            } else {
                Vec::new()
            }
        };
        self.bury(account, name, dead)?;
        self.note_write_for(account, name)?;
        Ok(())
    }

    /// Reads a record without claiming it: the head of the queue, or the one
    /// under `key`.
    ///
    /// Expired claims are swept on the way past, so a peek at a queue whose
    /// consumers have died shows the record that is about to be redelivered
    /// rather than the one behind it.
    pub fn peek(&self, account: &str, name: &str, key: Option<&str>) -> DbResult<Option<QueueDelivery>> {
        let (handle, policy) = self.queue_file(account, name)?;
        let policy = Self::effective_policy(name, policy);
        let now = queue::now_millis();
        let (delivery, dead) = {
            let mut table = handle.write();
            let dead = Self::take_expired(&mut table, now, policy);
            let table = &mut *table;
            let state = table.queue.as_ref().expect("queue_file attaches the queue state");
            let wanted = match key {
                Some(key) => Some(key.to_string()),
                None => state.head().cloned(),
            };
            let delivery = wanted.and_then(|key| {
                let record = table.records.get(&key)?.clone();
                let claim = state.claim_on(&key);
                Some(QueueDelivery {
                    deliveries: state.deliveries(&key),
                    enqueued_millis: queue::key_enqueued_millis(&key),
                    expires_millis: claim.map(|claim| claim.expires_millis),
                    owner: claim.map(|claim| claim.owner.clone()),
                    record,
                    key,
                })
            });
            (delivery, dead)
        };
        self.bury(account, name, dead)?;
        Ok(delivery)
    }

    /// What a queue is doing, for `FILE.STATS` and the dashboard.
    ///
    /// Sweeping first is what keeps the numbers honest: an in-flight count that
    /// still counts claims which lapsed ten minutes ago describes a queue that
    /// is busy when it is in fact stalled.
    pub fn queue_statistics(&self, account: &str, name: &str) -> DbResult<QueueStats> {
        let (handle, policy) = self.queue_file(account, name)?;
        let now = queue::now_millis();
        let (stats, dead) = {
            let mut table = handle.write();
            // Swept under the effective policy, but reported under the file's
            // own: a dead-letter file's entry says what its records were given
            // before they got there, and that is what an operator is reading.
            let dead = Self::take_expired(&mut table, now, Self::effective_policy(name, policy));
            let state = table.queue.as_ref().expect("queue_file attaches the queue state");
            let stats = QueueStats {
                depth: state.depth() as u64,
                in_flight: state.in_flight() as u64,
                oldest_unacknowledged_seconds: state.oldest_unacknowledged_seconds(now),
                dead_letters: 0,
                next_sequence: state.next_sequence(),
                visibility_timeout_seconds: policy.visibility_seconds(),
                max_deliveries: policy.max_deliveries,
                dead_letter: queue::is_dead_letter_name(name),
            };
            (stats, dead)
        };
        self.bury(account, name, dead)?;
        let dead_name = queue::dead_letter_name(name);
        let dead_letters = if queue::is_dead_letter_name(name) || !self.account_has_table(account, &dead_name) {
            0
        } else {
            self.get_table_read_only_for_account(account, &dead_name)
                .map(|handle| handle.read().records.len() as u64)
                .unwrap_or_else(|| self.file_record_count(&self.account_storage_dir(account), account, &dead_name))
        };
        Ok(QueueStats { dead_letters, ..stats })
    }

    /// Sweeps the claims that have lapsed, taking out of the table the records
    /// that have now been delivered as often as the policy allows.
    ///
    /// The caller is holding the table's guard, so the records come back in
    /// hand rather than being written anywhere: putting them in the dead-letter
    /// file is a second file's business, and this thread may hold only one.
    fn take_expired(table: &mut Table, now: u64, policy: QueuePolicy) -> Vec<DeadLetter> {
        let expired = match table.queue.as_mut() {
            Some(state) => state.expire(now, policy),
            None => return Vec::new(),
        };
        if expired.dead.is_empty() {
            return Vec::new();
        }
        let mut dead = Vec::with_capacity(expired.dead.len());
        for key in expired.dead {
            let deliveries = table
                .queue
                .as_ref()
                .map(|state| state.deliveries(&key))
                .unwrap_or_default();
            if let Some(state) = table.queue.as_mut() {
                state.forget(&key);
            }
            let record = table.records.remove(&key).unwrap_or_default();
            table.mark_dirty(&key);
            dead.push(DeadLetter {
                key,
                record,
                deliveries,
            });
        }
        dead
    }

    /// Writes dead letters to `<name>.DEAD`, creating it if this is the first.
    ///
    /// Called with no table guard held: the records were taken out of the queue
    /// under its own guard and are carried here, so that the two files are
    /// never locked at once. The dead-letter file is flushed before the queue
    /// is - see the note at the top of this module on what a crash in between
    /// does.
    fn bury(&self, account: &str, queue_name: &str, dead: Vec<DeadLetter>) -> DbResult<()> {
        if dead.is_empty() {
            return Ok(());
        }
        let name = queue::dead_letter_name(queue_name);
        if !self.account_has_table(account, &name) {
            // The dead-letter file inherits the durability a queue has by
            // default: a record that reached it has already failed everywhere
            // else, and losing it to a buffer would be the last chance gone.
            match self.create_table_with(
                account,
                &name,
                FileAttributes {
                    durable: true,
                    queue: Some(self.queue_policy_for_account(account, queue_name)),
                },
            ) {
                Ok(()) => {}
                Err(DbError::FileExists { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        let (handle, _) = self.queue_file(account, &name)?;
        {
            let mut table = handle.write();
            for letter in dead {
                table.insert_record(&letter.key, letter.record);
                table
                    .queue
                    .as_mut()
                    .expect("queue_file attaches the queue state")
                    .adopt(&letter.key, letter.deliveries);
            }
        }
        self.note_write_for(account, &name)
    }

    /// Turns a failed ownership check into the error the caller sends back.
    fn claim_check(table: &Table, key: &str, owner: &str, present: bool, name: &str) -> DbResult<()> {
        let state = table.queue.as_ref().expect("queue_file attaches the queue state");
        match state.verify(key, owner, present) {
            Ok(()) => Ok(()),
            Err(ClaimError::NotFound) => Err(DbError::InvalidRequest(format!("No record {} in queue {}", key, name))),
            Err(ClaimError::NotClaimed) => Err(DbError::InvalidRequest(format!(
                "Record {} in queue {} is not claimed: the claim lapsed, or it was never dequeued",
                key, name
            ))),
            Err(ClaimError::HeldBy(holder)) => Err(DbError::InvalidRequest(format!(
                "Record {} in queue {} is claimed by {}",
                key, name, holder
            ))),
        }
    }
}

/// Writes a queue's persisted state during a flush, and marks it clean.
///
/// Kept here rather than inline in the flush so that everything the `queue`
/// file means sits in one place.
pub(super) fn persist(
    file_dir: &str,
    state: &mut QueueState,
    fsync: crate::db::hashfile::FsyncPolicy,
) -> std::io::Result<()> {
    queue::write_state(file_dir, &state.to_persisted(), fsync)?;
    state.clear_dirty();
    Ok(())
}
