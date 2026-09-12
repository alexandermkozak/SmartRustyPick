//! The on-disk format version of a storage directory, and what a build does
//! when it meets one it did not write.
//!
//! # The question this answers
//!
//! The server is deployed as a container image with `db_storage/` on a mounted
//! volume. The volume outlives the container by design, so replacing the image
//! with a newer tag is the *normal* way to upgrade - and without a stamp on the
//! directory, every one of those upgrades was an untested assumption. Nothing
//! recorded which build wrote the data, and nothing checked.
//!
//! The failure that costs is not a server that refuses to start. It is a newer
//! binary opening an older directory, misreading a structure whose layout
//! changed, and either serving wrong answers or writing something the older
//! binary can no longer read - by which time the rollback is broken too. A
//! database that will not start is a bad morning; one that starts and is subtly
//! wrong is a bad quarter. Everything here exists to turn the second into the
//! first.
//!
//! # The policy
//!
//! A build declares two numbers: [`CURRENT`], the version it writes, and
//! [`OLDEST_SUPPORTED`], the oldest it can open. Opening a directory then has
//! four outcomes, and [`Opened`] is which one happened:
//!
//! - **at `CURRENT`** - nothing to do.
//! - **between `OLDEST_SUPPORTED` and `CURRENT`** - migrated in place, up one
//!   version at a time, and restamped. This is deliberate: an upgrade should be
//!   "pull the new tag and start it", not a runbook.
//! - **below `OLDEST_SUPPORTED`, or above `CURRENT`** - refused, naming the
//!   version found and the range supported. A directory above `CURRENT` is the
//!   rollback case, and it is refused rather than opened, because a build that
//!   does not know the format cannot know what it would be misreading.
//! - **no stamp at all** - adopted as [`OLDEST_SUPPORTED`] and recorded, then
//!   migrated forward like any other old directory.
//!
//! # Why adopting an unstamped directory is sound, and only once
//!
//! Stamping did not always exist, so every directory written before it has no
//! stamp. There has only ever been one lineage of this format, so such a
//! directory is version 1 - not a guess so much as the only thing it can be.
//! It is the single assumption in this module, it is made once per directory,
//! and it is recorded, so it is never made again.
//!
//! A **damaged** stamp is the opposite case and is refused. The version it
//! carried cannot be recovered, and treating "I cannot read this" as "there was
//! never one" is how a directory gets adopted as version 1 by a build that
//! should have refused it. [`crate::db::statefile`] keeps the two apart for
//! exactly this.
//!
//! # Downgrade
//!
//! Migrating restamps the directory, so the previous image will refuse it on
//! sight. That is the correct outcome and not a limitation: the older build
//! genuinely cannot read what the newer one wrote. It has to be *said*, though,
//! because the operator reaching for the previous tag needs to know before they
//! try - see `docs/deployment.md`.
//!
//! One transition is not covered, and cannot be: a build from before this
//! module existed checks nothing, so it will open a directory of any version
//! quite happily. The protection starts with the first build that has it.

use crate::db::error::{DbError, DbResult};
use crate::db::hashfile::FsyncPolicy;
use crate::db::statefile;
use std::fs;
use std::path::{Path, PathBuf};

/// The format version this build writes.
///
/// Bump it when the on-disk layout changes in a way an older build would
/// misread, and add the [`STEPS`] entry that reaches it. Not for a change an
/// older build tolerates - a new `DIR` attribute it ignores, a new file beside
/// the records it never opens - because a version that rises without the format
/// really changing makes every upgrade a migration and teaches operators that
/// the number means nothing.
pub const CURRENT: u32 = 1;

/// The oldest version this build can open and bring forward.
///
/// Dropping support for a version means raising this *and* deleting its step,
/// which is a deliberate act: it strands any directory still on it, and those
/// operators need a release note, not a surprise at start-up.
pub const OLDEST_SUPPORTED: u32 = 1;

/// The stamp, inside the storage directory.
///
/// A leading dot for the reason [`crate::db::transaction::LOG_DIR`] has one:
/// the storage directory's other entries are account directories, and an
/// account cannot be named `.format`.
pub const STAMP: &str = ".format";

/// One migration step: brings a storage directory from version `from` to
/// `from + 1`, leaving the stamp to the caller.
type Step = fn(&str) -> DbResult<()>;

/// Every migration, by the version it starts from.
///
/// Empty, because there has only ever been one format version. When there are
/// two, this is where the step between them goes - and
/// [`every_supported_version_has_a_way_forward`] fails if [`CURRENT`] is raised
/// without one, so the ladder cannot quietly acquire a missing rung.
///
/// [`every_supported_version_has_a_way_forward`]: #
const STEPS: &[(u32, Step)] = &[];

/// What opening a storage directory found, and what was done about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// The directory did not exist, or held nothing. Created and stamped.
    Created,
    /// Already at the version this build writes.
    Current,
    /// Held data but carried no stamp. Adopted as [`OLDEST_SUPPORTED`] and
    /// recorded - see the module documentation for why that is sound.
    Adopted,
    /// Brought forward from `from` to [`CURRENT`] and restamped.
    Migrated { from: u32 },
}

impl Opened {
    /// True when the directory was changed in a way an older build cannot
    /// undo. What `docs/deployment.md` warns about before a rollback, and what
    /// the start-up line reports rather than leaving to a log nobody reads.
    pub fn changed_the_format(&self) -> bool {
        matches!(self, Opened::Migrated { .. })
    }

    /// One line for an operator watching the server start. `None` when there is
    /// nothing worth saying - an ordinary start on a current directory should
    /// not print anything at all.
    pub fn describe(&self, storage_dir: &str) -> Option<String> {
        match self {
            Opened::Current => None,
            Opened::Created => Some(format!("{} created at storage format {}.", storage_dir, CURRENT)),
            Opened::Adopted => Some(format!(
                "{} holds data but carried no format stamp; adopting it as storage format {} and recording it.",
                storage_dir, OLDEST_SUPPORTED
            )),
            Opened::Migrated { from } => Some(format!(
                "{} migrated from storage format {} to {}. An older build can no longer open it.",
                storage_dir, from, CURRENT
            )),
        }
    }
}

/// The stamp file's path inside `storage_dir`.
pub fn stamp_path(storage_dir: &str) -> PathBuf {
    Path::new(storage_dir).join(STAMP)
}

/// The version `storage_dir` is stamped with, for a caller that wants to look
/// without opening - the refusal message, and the tests.
///
/// `Ok(None)` means there is no stamp; an unreadable one is an error, because
/// the version it held cannot be recovered and must not be guessed at.
pub fn version_of(storage_dir: &str) -> DbResult<Option<u32>> {
    match statefile::read(&stamp_path(storage_dir)) {
        statefile::State::Missing => Ok(None),
        statefile::State::Unreadable => Err(DbError::IncompatibleStorage {
            storage_dir: storage_dir.to_string(),
            detail: format!(
                "its {} stamp is unreadable, so the format it was written in cannot be determined. Restore the \
                 directory from a backup, or remove {} to have it adopted as format {} - only if you know it was \
                 written by a build that used that format",
                STAMP,
                stamp_path(storage_dir).display(),
                OLDEST_SUPPORTED
            ),
        }),
        statefile::State::Body(body) => match statefile::field(&body, "version").and_then(|v| v.parse::<u32>().ok()) {
            Some(version) => Ok(Some(version)),
            None => Err(DbError::IncompatibleStorage {
                storage_dir: storage_dir.to_string(),
                detail: format!("its {} stamp does not name a version", STAMP),
            }),
        },
    }
}

/// Checks `storage_dir`, migrating and stamping it as the policy requires, and
/// says what happened.
///
/// Called before anything else reads or writes a byte of it. An `Err` here is a
/// server that does not start, which is the whole point: every outcome this
/// refuses is one where carrying on would mean reading a layout this build does
/// not understand.
pub fn open(storage_dir: &str) -> DbResult<Opened> {
    let existed = Path::new(storage_dir).exists();
    if !existed {
        fs::create_dir_all(storage_dir)?;
    }

    let found = match version_of(storage_dir)? {
        Some(version) => version,
        None => {
            // Nothing in it is a directory nobody has used yet, whether this
            // call made it or a deployment script did. Anything in it is a
            // directory from before stamping existed.
            if is_empty(storage_dir) {
                stamp(storage_dir, CURRENT)?;
                return Ok(Opened::Created);
            }
            stamp(storage_dir, OLDEST_SUPPORTED)?;
            let adopted = migrate(storage_dir, OLDEST_SUPPORTED)?;
            return Ok(match adopted {
                Opened::Current => Opened::Adopted,
                other => other,
            });
        }
    };

    if found == CURRENT {
        return Ok(Opened::Current);
    }
    if found > CURRENT {
        return Err(DbError::IncompatibleStorage {
            storage_dir: storage_dir.to_string(),
            detail: format!(
                "it is at storage format {}, and this build writes {} (and opens {} upwards). It was written by a \
                 newer build: run that one against it, or restore this directory from a backup taken before the \
                 upgrade",
                found, CURRENT, OLDEST_SUPPORTED
            ),
        });
    }
    if found < OLDEST_SUPPORTED {
        return Err(DbError::IncompatibleStorage {
            storage_dir: storage_dir.to_string(),
            detail: format!(
                "it is at storage format {}, and this build opens {} upwards. Bring it forward with a build that \
                 still supports {} before upgrading to this one",
                found, OLDEST_SUPPORTED, found
            ),
        });
    }
    migrate(storage_dir, found)
}

/// Runs every step from `from` up to [`CURRENT`], stamping after each one.
///
/// Stamped **after** each step rather than once at the end, so a crash halfway
/// up a two-version climb leaves a directory that says where it got to. The
/// next start resumes from there instead of running the first step again over
/// data it has already converted.
fn migrate(storage_dir: &str, from: u32) -> DbResult<Opened> {
    let mut at = from;
    while at < CURRENT {
        let step = STEPS.iter().find(|(version, _)| *version == at).map(|(_, step)| step);
        let Some(step) = step else {
            // Unreachable unless CURRENT was raised without its step, which the
            // test below exists to catch before a release. Refusing here means
            // that mistake is a server that will not start rather than one that
            // silently stamps a directory it never converted.
            return Err(DbError::IncompatibleStorage {
                storage_dir: storage_dir.to_string(),
                detail: format!(
                    "it is at storage format {} and this build writes {}, but it carries no migration from {}. \
                     This is a bug in the build, not in the directory",
                    from, CURRENT, at
                ),
            });
        };
        step(storage_dir)?;
        at += 1;
        stamp(storage_dir, at)?;
    }
    Ok(if at == from {
        Opened::Current
    } else {
        Opened::Migrated { from }
    })
}

/// Records the version, durably: the stamp is what every later start trusts, so
/// it is the one small file worth an `fsync` whatever the server's policy is.
fn stamp(storage_dir: &str, version: u32) -> DbResult<()> {
    statefile::write(
        &stamp_path(storage_dir),
        &format!("version={}\n", version),
        FsyncPolicy::Always,
    )?;
    Ok(())
}

/// True when the directory holds nothing at all. An unreadable directory counts
/// as not empty: refusing to adopt what cannot be inspected is the safe way
/// round, and the error surfaces on the next thing that touches it.
fn is_empty(storage_dir: &str) -> bool {
    match fs::read_dir(storage_dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn stamped(dir: &str) -> Option<u32> {
        version_of(dir).expect("the stamp must be readable")
    }

    #[test]
    fn a_new_directory_is_created_and_stamped_with_what_this_build_writes() {
        let guard = TempDir::new("format_new");
        let dir = format!("{}/fresh", guard.path());
        assert_eq!(open(&dir).unwrap(), Opened::Created);
        assert_eq!(stamped(&dir), Some(CURRENT));

        // And opening it again is an ordinary start: nothing to do, and nothing
        // to say about it.
        assert_eq!(open(&dir).unwrap(), Opened::Current);
        assert_eq!(open(&dir).unwrap().describe(&dir), None);
    }

    #[test]
    fn an_unstamped_directory_that_holds_data_is_adopted_and_recorded() {
        let guard = TempDir::new("format_adopt");
        let dir = guard.path();
        fs::create_dir_all(format!("{}/SYSTEM", dir)).unwrap();

        let opened = open(dir).unwrap();
        assert_eq!(opened, Opened::Adopted);
        assert_eq!(stamped(dir), Some(OLDEST_SUPPORTED));
        assert!(
            opened.describe(dir).unwrap().contains("no format stamp"),
            "the one assumption in the module is announced, not silent"
        );
        assert!(
            !opened.changed_the_format(),
            "adopting reads the data, it does not rewrite it"
        );

        // The assumption is made once. The second start is an ordinary one.
        assert_eq!(open(dir).unwrap(), Opened::Current);
    }

    #[test]
    fn a_directory_from_a_newer_build_is_refused_rather_than_opened() {
        let guard = TempDir::new("format_newer");
        let dir = guard.path();
        stamp(dir, CURRENT + 1).unwrap();

        let refused = open(dir).unwrap_err();
        let DbError::IncompatibleStorage { detail, .. } = &refused else {
            panic!(
                "a rollback onto a newer directory must have its own error: {:?}",
                refused
            );
        };
        // Both halves have to be in the message: which version is on disk, and
        // which ones this build has. Either alone leaves an operator guessing.
        assert!(detail.contains(&(CURRENT + 1).to_string()), "{}", detail);
        assert!(detail.contains(&CURRENT.to_string()), "{}", detail);
        // And the stamp is left exactly as it was: a build that will not open a
        // directory has no business writing to it.
        assert_eq!(stamped(dir), Some(CURRENT + 1));
    }

    #[test]
    fn a_directory_older_than_this_build_supports_is_refused() {
        if OLDEST_SUPPORTED == 0 {
            return;
        }
        let guard = TempDir::new("format_older");
        let dir = guard.path();
        stamp(dir, OLDEST_SUPPORTED - 1).unwrap();

        let refused = open(dir).unwrap_err();
        assert!(matches!(refused, DbError::IncompatibleStorage { .. }), "{:?}", refused);
        assert!(refused.to_string().contains(&OLDEST_SUPPORTED.to_string()));
    }

    /// The distinction the whole module rests on. A damaged stamp is not an
    /// absent one: adopting it as version 1 is how a directory of some other
    /// version gets opened by a build that should have refused it.
    #[test]
    fn a_damaged_stamp_is_refused_rather_than_adopted() {
        let guard = TempDir::new("format_damaged");
        let dir = guard.path();
        fs::create_dir_all(format!("{}/SYSTEM", dir)).unwrap();
        fs::write(stamp_path(dir), "checksum=00000000\nversion=1\n").unwrap();

        let refused = open(dir).unwrap_err();
        assert!(matches!(refused, DbError::IncompatibleStorage { .. }), "{:?}", refused);
        assert!(
            refused.to_string().contains("unreadable"),
            "the message has to say the version could not be read: {}",
            refused
        );

        // A stamp that checks out but says nothing useful is refused too.
        fs::remove_file(stamp_path(dir)).unwrap();
        statefile::write(&stamp_path(dir), "kind=storage\n", FsyncPolicy::Never).unwrap();
        assert!(matches!(open(dir).unwrap_err(), DbError::IncompatibleStorage { .. }));
    }

    /// The ladder cannot acquire a missing rung. Raise `CURRENT` without adding
    /// the step that reaches it and this fails, here, rather than at a customer
    /// site as a directory stamped with a version nothing converted it to.
    // Both lints fire only because there is one format version today: the
    // comparison is constant and the range is empty. That is exactly the state
    // this test exists to watch, and it stops being true the moment somebody
    // raises either constant - which is the moment the assertions start
    // earning their place.
    #[allow(clippy::assertions_on_constants, clippy::reversed_empty_ranges)]
    #[test]
    fn every_supported_version_has_a_way_forward() {
        assert!(
            OLDEST_SUPPORTED <= CURRENT,
            "this build opens {} upwards but writes {}",
            OLDEST_SUPPORTED,
            CURRENT
        );
        for version in OLDEST_SUPPORTED..CURRENT {
            assert!(
                STEPS.iter().any(|(from, _)| *from == version),
                "storage format {} has no migration to {}. Add it to STEPS, or raise OLDEST_SUPPORTED past it.",
                version,
                version + 1
            );
        }
        for (from, _) in STEPS {
            assert!(
                *from < CURRENT,
                "STEPS carries a migration from {}, which is not below CURRENT ({})",
                from,
                CURRENT
            );
        }
    }

    /// The stamp is a file, and the directory's other entries are account
    /// directories - so it has to be a name no account can take.
    #[test]
    fn the_stamp_cannot_collide_with_an_account_directory() {
        assert!(STAMP.starts_with('.'), "{} could be an account name", STAMP);
    }
}
