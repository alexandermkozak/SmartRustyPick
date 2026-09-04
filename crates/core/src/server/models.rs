use crate::db::{DbError, QueryNode, SortSpec, ValuePosition};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io;

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Request {
    pub command: String,
    pub account: Option<String>,
    pub target_account: Option<String>,
    pub file: Option<String>,
    pub key: Option<String>,
    pub data: Option<serde_json::Value>,
    pub structured_data: Option<serde_json::Value>,
    pub is_dict: Option<bool>,
    pub query_node: Option<QueryNode>,
    pub query_string: Option<String>,
    pub sort_specs: Option<Vec<SortSpec>>,
    /// QUERY and SELECT: multivalued fields to explode, so each matching value
    /// becomes its own result row. The criterion comes from `query_node` or
    /// `query_string` as usual; a `BY.EXP` clause inside `query_string` fills
    /// this in on its own.
    pub explode: Option<Vec<String>>,
    pub list_name: Option<String>,
    pub batch_size: Option<usize>,
    pub thumbprint: Option<String>,
    pub name: Option<String>,
    pub accounts_list: Option<Vec<String>>,
    pub is_admin: Option<bool>,
    /// `CREATE.FILE`: create the file with per-file durable writes enabled.
    /// `SET.FILE`: turn per-file durable writes on or off for a file that
    /// already exists. Required there, since an absent flag must not be read as
    /// a request to demote.
    pub durable: Option<bool>,
    /// The dictionary field an index command names, alongside `file`.
    pub field: Option<String>,
    /// `CREATE.INDEX` and `SET.INDEX.EXCLUDE`: the values the index is to skip.
    /// An empty list on `SET.INDEX.EXCLUDE` clears the exclusions; an absent
    /// one there means the same, since replacing the set is the whole command.
    pub values: Option<Vec<String>>,
    /// `INDEX.STATS`: how many of the commonest values to return. Capped by the
    /// server, so one request cannot ask it to sort and send every distinct
    /// value an index holds.
    pub limit: Option<usize>,
}

/// The machine-readable classification of an error response.
///
/// `status` says only that something failed and `message` says it in English,
/// which leaves a client that wants to tell "there is no such file" from "the
/// disk is full" nothing to work with but prose. This is the interface: the
/// wording of a message may change, a code may not. Every code is listed in
/// `docs/protocol.md`, and `protocol_doc_tests.rs` fails when one is added
/// without being written up there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// A required field of the request was absent or empty.
    MissingField,
    /// A field was present but does not describe what it has to.
    InvalidData,
    /// A `query_string` that is not a query. Distinct from an absent one, which
    /// selects every record.
    InvalidQuery,
    /// The request line was not JSON.
    InvalidJson,
    /// No command of that name.
    UnknownCommand,
    /// The command is admin only and the client is not an admin.
    AdminRequired,
    /// The account is not in the client's allowed list.
    AccessDenied,
    /// The client's authorization was revoked while it was connected.
    Deauthorized,
    /// The command works inside an account and the request named none, with no
    /// single allowed account to fall back on.
    AccountNotSpecified,
    AccountNotFound,
    AccountExists,
    /// `SYSTEM` holds the registry and the authorizations; it cannot be dropped.
    AccountProtected,
    FileNotFound,
    FileExists,
    RecordNotFound,
    /// `GET.NEXT` for a list name no `SELECT` has filled.
    SelectListNotFound,
    /// No client is authorized under that name.
    ClientNotFound,
    IndexNotFound,
    IndexExists,
    /// The field cannot carry an index, and the message says why.
    InvalidField,
    /// Understood and refused: the database will not do this.
    InvalidRequest,
    /// What is on disk does not decode. The file needs repair, not a retry.
    CorruptData,
    /// The server may not touch a file or directory it needs.
    PermissionDenied,
    /// Any other I/O failure - a full disk, a short write, a broken pipe.
    IoError,
    /// The server cannot answer this command as it is currently configured.
    Unavailable,
}

impl ErrorCode {
    /// Every code, in the order `docs/protocol.md` lists them. Kept for the
    /// documentation test rather than for the wire.
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::MissingField,
        ErrorCode::InvalidData,
        ErrorCode::InvalidQuery,
        ErrorCode::InvalidJson,
        ErrorCode::UnknownCommand,
        ErrorCode::AdminRequired,
        ErrorCode::AccessDenied,
        ErrorCode::Deauthorized,
        ErrorCode::AccountNotSpecified,
        ErrorCode::AccountNotFound,
        ErrorCode::AccountExists,
        ErrorCode::AccountProtected,
        ErrorCode::FileNotFound,
        ErrorCode::FileExists,
        ErrorCode::RecordNotFound,
        ErrorCode::SelectListNotFound,
        ErrorCode::ClientNotFound,
        ErrorCode::IndexNotFound,
        ErrorCode::IndexExists,
        ErrorCode::InvalidField,
        ErrorCode::InvalidRequest,
        ErrorCode::CorruptData,
        ErrorCode::PermissionDenied,
        ErrorCode::IoError,
        ErrorCode::Unavailable,
    ];

    /// The string this code is sent as.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::MissingField => "MISSING_FIELD",
            ErrorCode::InvalidData => "INVALID_DATA",
            ErrorCode::InvalidQuery => "INVALID_QUERY",
            ErrorCode::InvalidJson => "INVALID_JSON",
            ErrorCode::UnknownCommand => "UNKNOWN_COMMAND",
            ErrorCode::AdminRequired => "ADMIN_REQUIRED",
            ErrorCode::AccessDenied => "ACCESS_DENIED",
            ErrorCode::Deauthorized => "DEAUTHORIZED",
            ErrorCode::AccountNotSpecified => "ACCOUNT_NOT_SPECIFIED",
            ErrorCode::AccountNotFound => "ACCOUNT_NOT_FOUND",
            ErrorCode::AccountExists => "ACCOUNT_EXISTS",
            ErrorCode::AccountProtected => "ACCOUNT_PROTECTED",
            ErrorCode::FileNotFound => "FILE_NOT_FOUND",
            ErrorCode::FileExists => "FILE_EXISTS",
            ErrorCode::RecordNotFound => "RECORD_NOT_FOUND",
            ErrorCode::SelectListNotFound => "SELECT_LIST_NOT_FOUND",
            ErrorCode::ClientNotFound => "CLIENT_NOT_FOUND",
            ErrorCode::IndexNotFound => "INDEX_NOT_FOUND",
            ErrorCode::IndexExists => "INDEX_EXISTS",
            ErrorCode::InvalidField => "INVALID_FIELD",
            ErrorCode::InvalidRequest => "INVALID_REQUEST",
            ErrorCode::CorruptData => "CORRUPT_DATA",
            ErrorCode::PermissionDenied => "PERMISSION_DENIED",
            ErrorCode::IoError => "IO_ERROR",
            ErrorCode::Unavailable => "UNAVAILABLE",
        }
    }

    /// The code a wire string names, or `None` for one this build does not know.
    pub fn from_wire(code: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.iter().copied().find(|known| known.as_str() == code)
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let code = String::deserialize(deserializer)?;
        ErrorCode::from_wire(&code).ok_or_else(|| serde::de::Error::custom(format!("unknown error code {}", code)))
    }
}

/// A code this build does not know reads back as unpopulated rather than
/// failing the whole response: a client compiled against an older server must
/// still be able to see the status and the message of an error it cannot
/// classify.
fn deserialize_code<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ErrorCode>, D::Error> {
    let code = Option::<String>::deserialize(deserializer)?;
    Ok(code.as_deref().and_then(ErrorCode::from_wire))
}

/// How an I/O failure is classified: the kinds worth telling apart from
/// "something went wrong with the disk" get their own code. `InvalidData` is
/// what the storage layer raises for bytes that do not decode, and
/// `InvalidInput` for an argument it will not take.
impl From<&io::Error> for ErrorCode {
    fn from(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            io::ErrorKind::InvalidData => ErrorCode::CorruptData,
            io::ErrorKind::InvalidInput => ErrorCode::InvalidData,
            _ => ErrorCode::IoError,
        }
    }
}

/// The engine's vocabulary onto the protocol's. Exhaustive on purpose: a new
/// [`DbError`] variant fails to compile here until it has been given a code.
impl From<&DbError> for ErrorCode {
    fn from(e: &DbError) -> Self {
        match e {
            DbError::NoAccount => ErrorCode::AccountNotSpecified,
            DbError::AccountNotFound(_) => ErrorCode::AccountNotFound,
            DbError::AccountExists(_) => ErrorCode::AccountExists,
            DbError::AccountProtected(_) => ErrorCode::AccountProtected,
            DbError::FileNotFound { .. } => ErrorCode::FileNotFound,
            DbError::FileExists { .. } => ErrorCode::FileExists,
            DbError::IndexExists { .. } => ErrorCode::IndexExists,
            DbError::IndexNotFound { .. } => ErrorCode::IndexNotFound,
            DbError::InvalidField { .. } => ErrorCode::InvalidField,
            DbError::InvalidRequest(_) => ErrorCode::InvalidRequest,
            DbError::Io(inner) => ErrorCode::from(inner),
        }
    }
}

/// Every field but `status` is skipped when empty, so a reply carries only what
/// the command it answers actually populated. That is the contract
/// `docs/protocol.md` states, and it keeps the six-field null tail off every
/// response on the wire. Clients must read an absent field as "not populated",
/// exactly as they read a null one.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Response {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The error's stable classification, set whenever `status` is `"ERROR"`.
    /// This is what a client branches on; `message` is for a person to read.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_code"
    )]
    pub code: Option<ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results: Option<Vec<(String, serde_json::Value)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// For an exploded result, the position within the exploded field that put
    /// each row in `results` there. Index-aligned with `results`, and `None`
    /// for an ordinary, unexploded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positions: Option<Vec<Option<ValuePosition>>>,
}
