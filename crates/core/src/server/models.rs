use crate::db::QueryNode;
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
    pub list_name: Option<String>,
    pub batch_size: Option<usize>,
    pub thumbprint: Option<String>,
    pub name: Option<String>,
    pub accounts_list: Option<Vec<String>>,
    pub is_admin: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Response {
    pub status: String,
    pub message: Option<String>,
    pub record: Option<serde_json::Value>,
    pub results: Option<Vec<(String, serde_json::Value)>>,
    pub keys: Option<Vec<String>>,
    pub count: Option<usize>,
}
