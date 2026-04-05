use crate::db::engine::Database;
use crate::db::models::*;
use std::collections::HashMap;

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
        // Pre-calculate field indices to avoid repeated mutable borrows of self
        let mut field_map = HashMap::new();
        self.collect_field_indices(table_name, query, &mut field_map);

        let mut results = Vec::new();

        // Use a block to limit the borrow of `table`
        {
            let table = self.get_table_mut(table_name);
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

    pub(crate) fn collect_field_indices(&mut self, table_name: &str, node: &QueryNode, map: &mut HashMap<String, usize>) {
        match node {
            QueryNode::Condition(cond) => {
                if cond.field_name == "ID" { return; }
                if !map.contains_key(&cond.field_name) {
                    if let Some(idx) = self.get_field_index(table_name, &cond.field_name) {
                        map.insert(cond.field_name.clone(), idx);
                    }
                }
            }
            QueryNode::Logical { left, right, .. } => {
                self.collect_field_indices(table_name, left, map);
                self.collect_field_indices(table_name, right, map);
            }
        }
    }

    pub(crate) fn evaluate_node_static_with_id(key: &str, record: &Record, node: &QueryNode, field_map: &HashMap<String, usize>) -> bool {
        match node {
            QueryNode::Condition(cond) => {
                if cond.field_name == "ID" {
                    return Self::compare_values(key, &cond.op, &cond.value);
                }
                let field_idx = match field_map.get(&cond.field_name) {
                    Some(idx) => *idx,
                    None => return false,
                };

                if let Some(field) = record.fields.get(field_idx) {
                    if field.values.is_empty() {
                        return Self::compare_values("", &cond.op, &cond.value);
                    }
                    for v in &field.values {
                        if v.sub_values.is_empty() {
                            if Self::compare_values("", &cond.op, &cond.value) { return true; }
                        }
                        if v.sub_values.iter().any(|sv| Self::compare_values(sv, &cond.op, &cond.value)) {
                            return true;
                        }
                    }
                } else {
                    return Self::compare_values("", &cond.op, &cond.value);
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
        match op {
            "=" => record_val.trim() == search_val.trim(),
            "!=" => record_val.trim() != search_val.trim(),
            "<" => record_val.trim() < search_val.trim(),
            ">" => record_val.trim() > search_val.trim(),
            "<=" => record_val.trim() <= search_val.trim(),
            ">=" => record_val.trim() >= search_val.trim(),
            "[" => record_val.trim().ends_with(search_val.trim()),
            "]" => record_val.trim().starts_with(search_val.trim()),
            "[]" => record_val.trim().contains(search_val.trim()),
            _ => false,
        }
    }
}
