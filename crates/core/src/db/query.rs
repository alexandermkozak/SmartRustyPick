use crate::db::engine::Database;
use crate::db::models::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct FieldQueryInfo {
    pub index: usize,
    pub conversion: Option<String>,
}

impl Database {
    pub fn parse_query(&mut self, _table_name: &str, parts: &[&str]) -> Option<QueryNode> {
        // Simple parser for WITH <field> <op> <value> [AND/OR <field> <op> <value> ...]
        if parts.is_empty() { return None; }
        let mut start_idx = 0;
        if parts[0].to_uppercase() == "WITH" {
            start_idx = 1;
        }

        let mut i = start_idx;
        let mut current_node: Option<QueryNode> = None;

        while i < parts.len() {
            if i + 2 >= parts.len() { break; }

            let field_name = parts[i];
            let op = parts[i + 1];
            let mut value = parts[i + 2].to_string();
            if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                value = value[1..value.len() - 1].to_string();
            }
            let value = value.trim().to_string();

            let condition = QueryNode::Condition(QueryCondition {
                field_name: field_name.to_string(),
                op: op.to_string(),
                value,
            });

            match current_node {
                None => {
                    current_node = Some(condition);
                    i += 3;
                }
                Some(_) => {
                    // This shouldn't happen without a logical op
                    return None;
                }
            }

            // Check for logical operator
            while i < parts.len() {
                let logical_op_str = parts[i].to_uppercase();
                let logical_op = match logical_op_str.as_str() {
                    "AND" => LogicalOp::And,
                    "OR" => LogicalOp::Or,
                    _ => break, // End of query or unknown
                };
                i += 1;

                // Parse next condition
                if i + 2 >= parts.len() { break; }
                let field_name = parts[i];
                let op = parts[i + 1];
                let mut value = parts[i + 2].to_string();
                if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                    value = value[1..value.len() - 1].to_string();
                }
                let value = value.trim().to_string();
                let next_condition = QueryNode::Condition(QueryCondition {
                    field_name: field_name.to_string(),
                    op: op.to_string(),
                    value,
                });

                current_node = Some(QueryNode::Logical {
                    op: logical_op,
                    left: Box::new(current_node.unwrap()),
                    right: Box::new(next_condition),
                });
                i += 3;
            }
        }

        current_node
    }

    pub fn query(&mut self, table_name: &str, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        let account = self.current_account.clone();
        self.query_for_account(&account, table_name, use_dict_section, query, keys_to_filter)
    }

    pub fn query_for_account(&mut self, account: &str, table_name: &str, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        // Pre-calculate field indices and conversions to avoid repeated mutable borrows of self
        let mut field_map = HashMap::new();
        self.collect_field_indices_for_account(account, table_name, query, &mut field_map);

        let mut results = Vec::new();

        // Use a block to limit the borrow of `table`
        {
            let table = match self.get_table_mut_for_account(account, table_name) {
                Ok(t) => t,
                Err(_) => return results, // Return empty results if table not found
            };
            let source_map = if use_dict_section {
                &table.dictionary
            } else {
                &table.records
            };

            if let Some(filter_keys) = keys_to_filter {
                for key in filter_keys {
                    if let Some(record) = source_map.get(key) {
                        if Self::evaluate_node_static_with_id(key, record, query, &field_map) {
                            results.push((key.clone(), record.clone()));
                        }
                    }
                }
            } else {
                // Optimize: Filter before sorting.
                // Avoid cloning the entire table by using an iterator.
                results = source_map.iter()
                    .filter(|(key, record)| Self::evaluate_node_static_with_id(key, record, query, &field_map))
                    .map(|(key, record)| (key.clone(), record.clone()))
                    .collect();
                results.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }

        results
    }


    pub(crate) fn collect_field_indices_for_account(&mut self, account: &str, table_name: &str, node: &QueryNode, map: &mut HashMap<String, FieldQueryInfo>) {
        match node {
            QueryNode::Condition(cond) => {
                if cond.field_name == "ID" { return; }
                if !map.contains_key(&cond.field_name) {
                    if let Some(idx) = self.get_field_index_for_account(account, table_name, &cond.field_name) {
                        let conversion = self.get_conversion_code_read_only_for_account(account, table_name, &cond.field_name);
                        map.insert(cond.field_name.clone(), FieldQueryInfo { index: idx, conversion });
                    }
                }
            }
            QueryNode::Logical { left, right, .. } => {
                self.collect_field_indices_for_account(account, table_name, left, map);
                self.collect_field_indices_for_account(account, table_name, right, map);
            }
        }
    }

    pub(crate) fn evaluate_node_static_with_id(key: &str, record: &Record, node: &QueryNode, field_map: &HashMap<String, FieldQueryInfo>) -> bool {
        match node {
            QueryNode::Condition(cond) => {
                if cond.field_name == "ID" {
                    return Self::compare_values(key, &cond.op, &cond.value);
                }
                let info = match field_map.get(&cond.field_name) {
                    Some(info) => info,
                    None => return false,
                };

                let search_val = if let Some(code) = &info.conversion {
                    Self::apply_iconv(&cond.value, code)
                } else {
                    cond.value.clone()
                };

                if let Some(field) = record.fields.get(info.index) {
                    if field.values.is_empty() {
                        return Self::compare_values("", &cond.op, &search_val);
                    }
                    for v in &field.values {
                        if v.sub_values.is_empty() {
                            if Self::compare_values("", &cond.op, &search_val) { return true; }
                        }
                        if v.sub_values.iter().any(|sv| Self::compare_values(sv, &cond.op, &search_val)) {
                            return true;
                        }
                    }
                } else {
                    return Self::compare_values("", &cond.op, &search_val);
                }
                false
            }
            QueryNode::Logical { op, left, right } => {
                match op {
                    LogicalOp::And => Self::evaluate_node_static_with_id(key, record, left, field_map) && Self::evaluate_node_static_with_id(key, record, right, field_map),
                    LogicalOp::Or => Self::evaluate_node_static_with_id(key, record, left, field_map) || Self::evaluate_node_static_with_id(key, record, right, field_map),
                }
            }
        }
    }

    pub(crate) fn compare_values(record_val: &str, op: &str, search_val: &str) -> bool {
        let record_val = record_val.trim();
        let op_upper = op.to_uppercase();
        match op_upper.as_str() {
            "=" | "EQ" => {
                let len = search_val.len();
                if len >= 2 && search_val.starts_with('[') && search_val.ends_with(']') {
                    record_val.contains(&search_val[1..len - 1])
                } else if len >= 1 && search_val.ends_with(']') {
                    record_val.starts_with(&search_val[..len - 1])
                } else if len >= 1 && search_val.starts_with('[') {
                    record_val.ends_with(&search_val[1..])
                } else {
                    record_val == search_val
                }
            }
            "!=" | "#" | "<>" | "NE" => {
                let len = search_val.len();
                let matches = if len >= 2 && search_val.starts_with('[') && search_val.ends_with(']') {
                    record_val.contains(&search_val[1..len - 1])
                } else if len >= 1 && search_val.ends_with(']') {
                    record_val.starts_with(&search_val[..len - 1])
                } else if len >= 1 && search_val.starts_with('[') {
                    record_val.ends_with(&search_val[1..])
                } else {
                    record_val == search_val
                };
                !matches
            }
            "<" | "LT" => record_val < search_val,
            ">" | "GT" => record_val > search_val,
            "<=" | "LE" => record_val <= search_val,
            ">=" | "GE" => record_val >= search_val,
            _ => false,
        }
    }
}
