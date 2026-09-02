use crate::db::{QueryNode, SortSpec, ValuePosition};
use serde::{Deserialize, Serialize};

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
