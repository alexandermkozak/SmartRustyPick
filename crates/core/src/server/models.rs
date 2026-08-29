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
    /// CREATE.FILE only: create the file with per-file durable writes enabled.
    pub durable: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Response {
    pub status: String,
    pub message: Option<String>,
    pub record: Option<serde_json::Value>,
    pub results: Option<Vec<(String, serde_json::Value)>>,
    pub keys: Option<Vec<String>>,
    pub count: Option<usize>,
    /// For an exploded result, the position within the exploded field that put
    /// each row in `results` there. Index-aligned with `results`, and `None`
    /// for an ordinary, unexploded one.
    pub positions: Option<Vec<Option<ValuePosition>>>,
}
