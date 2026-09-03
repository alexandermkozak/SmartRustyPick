//! What the engine says went wrong.
//!
//! Every failure the database can report is one of the [`DbError`] variants, so
//! a caller decides what to do by matching on the kind rather than by reading
//! the message. That distinction used to be lost: an account that does not
//! exist, a file that already does and a disk that is full all arrived as
//! `io::Error::new(ErrorKind::Other, "some prose")`, which left the request
//! handler with nothing to classify and the tests asserting on wording.
//!
//! Genuine I/O keeps its own variant and its `io::Error` intact, so the kind a
//! caller may want to retry on - permission denied, a short write - is still
//! there to look at. The protocol layer maps these variants onto the stable
//! error codes `docs/protocol.md` documents; the mapping lives with the wire
//! types in `server::models`, so the engine owes the protocol nothing but this
//! vocabulary.

use std::fmt;
use std::io;

/// The result of anything the engine can be asked to do.
pub type DbResult<T> = std::result::Result<T, DbError>;

/// A failure the database reports.
///
/// The variants carry what the message used to spell out - which account,
/// which file, which field - so a caller can build its own wording, and a test
/// can assert on the kind without pinning the prose.
#[derive(Debug)]
pub enum DbError {
    /// A command that works inside an account was given none.
    NoAccount,
    /// The named account is not in the registry.
    AccountNotFound(String),
    /// An account of that name is already registered.
    AccountExists(String),
    /// `SYSTEM` holds the account registry and the authorized clients, so it
    /// cannot be dropped.
    AccountProtected(String),
    /// The account has no file of that name.
    FileNotFound { account: String, file: String },
    /// The account already has a file of that name.
    FileExists { account: String, file: String },
    /// The file already carries an index on that field.
    IndexExists { file: String, field: String },
    /// The file carries no index on that field.
    IndexNotFound { file: String, field: String },
    /// The field cannot be indexed, and `reason` says why: it is not in the
    /// file's dictionary, it is `ID`, or its name cannot become a directory.
    InvalidField { field: String, reason: String },
    /// The request was understood and refused: it asks for something the
    /// database will not do, such as setting the `DIR` file's durability.
    InvalidRequest(String),
    /// A real I/O failure, with the `io::Error` it came from.
    Io(io::Error),
}

impl DbError {
    /// The `io::ErrorKind` this variant stands for, for the boundaries that
    /// still speak `io::Result` - `main` and the async server plumbing.
    fn io_kind(&self) -> io::ErrorKind {
        match self {
            DbError::NoAccount | DbError::InvalidField { .. } | DbError::InvalidRequest(_) => {
                io::ErrorKind::InvalidInput
            }
            DbError::AccountNotFound(_) | DbError::FileNotFound { .. } | DbError::IndexNotFound { .. } => {
                io::ErrorKind::NotFound
            }
            DbError::AccountExists(_) | DbError::FileExists { .. } | DbError::IndexExists { .. } => {
                io::ErrorKind::AlreadyExists
            }
            DbError::AccountProtected(_) => io::ErrorKind::PermissionDenied,
            DbError::Io(e) => e.kind(),
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NoAccount => write!(f, "Not logged into an account"),
            DbError::AccountNotFound(name) => write!(f, "Account '{}' not found", name),
            DbError::AccountExists(name) => write!(f, "Account '{}' already exists", name),
            DbError::AccountProtected(name) => write!(f, "Cannot delete {} account", name),
            DbError::FileNotFound { account, file } => {
                write!(f, "Table '{}' not found in account '{}'", file, account)
            }
            DbError::FileExists { account, file } => {
                write!(f, "Table '{}' already exists in account '{}'", file, account)
            }
            DbError::IndexExists { file, field } => {
                write!(f, "'{}' is already indexed on file '{}'", field, file)
            }
            DbError::IndexNotFound { file, field } => {
                write!(f, "'{}' is not indexed on file '{}'", field, file)
            }
            DbError::InvalidField { field, reason } => write!(f, "'{}' cannot be indexed: {}", field, reason),
            DbError::InvalidRequest(detail) => write!(f, "{}", detail),
            DbError::Io(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for DbError {
    fn from(e: io::Error) -> Self {
        DbError::Io(e)
    }
}

/// Back to `io::Error` for the boundaries that still speak it, keeping the kind
/// the variant stands for so a caller matching on `ErrorKind` sees the same
/// classification it would have seen before.
impl From<DbError> for io::Error {
    fn from(e: DbError) -> Self {
        match e {
            DbError::Io(inner) => inner,
            other => io::Error::new(other.io_kind(), other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variant_says_which_file_it_is_about_without_being_read_as_prose() {
        let missing = DbError::FileNotFound {
            account: "SALES".to_string(),
            file: "USERS".to_string(),
        };
        // The point of the variants: the caller reaches for the account and the
        // file it names rather than parsing them back out of the message.
        let DbError::FileNotFound { account, file } = &missing else {
            panic!("unexpected variant: {missing:?}");
        };
        assert_eq!((account.as_str(), file.as_str()), ("SALES", "USERS"));
        assert_eq!(missing.to_string(), "Table 'USERS' not found in account 'SALES'");
    }

    #[test]
    fn an_io_error_survives_the_round_trip_unchanged() {
        let original = io::Error::new(io::ErrorKind::PermissionDenied, "no");
        let back: io::Error = DbError::from(original).into();
        assert_eq!(back.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(back.to_string(), "no");
    }

    #[test]
    fn a_semantic_error_carries_its_kind_across_the_io_boundary() {
        let back: io::Error = DbError::AccountExists("SALES".to_string()).into();
        assert_eq!(back.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(back.to_string(), "Account 'SALES' already exists");
    }
}
