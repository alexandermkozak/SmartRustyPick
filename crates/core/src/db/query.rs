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

    /// Splits a clause list into the non-sort tokens and the parsed sort specs.
    /// Sort operators are `BY` (ascending) and `BY.DSND` (descending), each followed by a field name.
    /// Any number of them may be present, anywhere in the clause, and they are applied from left to
    /// right. Tokens that are not part of a sort operator keep their relative order, so sort and
    /// column specifiers may be freely interleaved.
    pub fn parse_sort_specs<'a>(parts: &[&'a str]) -> (Vec<&'a str>, Vec<SortSpec>) {
        let mut remaining = Vec::new();
        let mut specs = Vec::new();
        let mut i = 0;
        while i < parts.len() {
            let descending = match parts[i].to_uppercase().as_str() {
                "BY" => false,
                "BY.DSND" => true,
                _ => {
                    remaining.push(parts[i]);
                    i += 1;
                    continue;
                }
            };
            if i + 1 >= parts.len() {
                // Trailing sort operator without a field name: keep the token so it is not lost.
                remaining.push(parts[i]);
                i += 1;
                continue;
            }
            specs.push(SortSpec {
                field_name: parts[i + 1].to_string(),
                descending,
            });
            i += 2;
        }
        (remaining, specs)
    }

    pub fn sort_results(&mut self, table_name: &str, results: &mut Vec<(String, Record)>, specs: &[SortSpec]) {
        let account = self.current_account.clone();
        self.sort_results_for_account(&account, table_name, results, specs);
    }

    pub fn sort_results_for_account(&mut self, account: &str, table_name: &str, results: &mut Vec<(String, Record)>, specs: &[SortSpec]) {
        if specs.is_empty() { return; }

        let resolved = self.resolve_sort_fields(account, table_name, specs);

        // Pre-calculate the sort values once per record instead of on every comparison.
        let sort_keys: Vec<Vec<String>> = results
            .iter()
            .map(|(id, record)| Self::sort_key_for(id, record, &resolved))
            .collect();

        let order = Self::sorted_order(&sort_keys, &resolved, |i| results[i].0.as_str());

        let mut taken: Vec<Option<(String, Record)>> = results.drain(..).map(Some).collect();
        results.extend(order.into_iter().map(|i| taken[i].take().unwrap()));
    }

    /// Resolves each sort spec to a field index. `None` means the record ID, `Some(usize::MAX)`
    /// marks an unknown field, which compares equal so the ordering stays stable.
    fn resolve_sort_fields(&mut self, account: &str, table_name: &str, specs: &[SortSpec]) -> Vec<(Option<usize>, bool)> {
        let mut resolved: Vec<(Option<usize>, bool)> = Vec::with_capacity(specs.len());
        for spec in specs {
            if spec.field_name == "ID" {
                resolved.push((None, spec.descending));
            } else {
                match self.get_field_index_for_account(account, table_name, &spec.field_name) {
                    Some(i) => resolved.push((Some(i), spec.descending)),
                    None => resolved.push((Some(usize::MAX), spec.descending)),
                }
            }
        }
        resolved
    }

    /// Builds the pre-calculated sort values of a single record, one per sort spec.
    fn sort_key_for(id: &str, record: &Record, resolved: &[(Option<usize>, bool)]) -> Vec<String> {
        resolved
            .iter()
            .map(|(idx, _)| match idx {
                None => id.to_string(),
                Some(i) if *i == usize::MAX => String::new(),
                Some(i) => record.get_field_display_string(*i),
            })
            .collect()
    }

    /// Sorts indices `0..sort_keys.len()` by the pre-calculated values, falling back to the ID.
    fn sorted_order<'a, F: Fn(usize) -> &'a str>(sort_keys: &[Vec<String>], resolved: &[(Option<usize>, bool)], id_of: F) -> Vec<usize> {
        let mut order: Vec<usize> = (0..sort_keys.len()).collect();
        order.sort_by(|&a, &b| {
            for (n, (idx, descending)) in resolved.iter().enumerate() {
                if matches!(idx, Some(i) if *i == usize::MAX) { continue; }
                let mut ord = Self::compare_sort_values(&sort_keys[a][n], &sort_keys[b][n]);
                if *descending {
                    ord = ord.reverse();
                }
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            id_of(a).cmp(id_of(b))
        });
        order
    }

    pub fn sort_keys(&mut self, table_name: &str, is_dict: bool, keys: Vec<String>, specs: &[SortSpec]) -> Vec<String> {
        let account = self.current_account.clone();
        self.sort_keys_for_account(&account, table_name, is_dict, keys, specs)
    }

    pub fn sort_keys_for_account(&mut self, account: &str, table_name: &str, is_dict: bool, keys: Vec<String>, specs: &[SortSpec]) -> Vec<String> {
        if specs.is_empty() { return keys; }

        let resolved = self.resolve_sort_fields(account, table_name, specs);

        // Look the records up by reference and pre-calculate their sort values; no record is cloned.
        let sort_keys: Vec<Vec<String>> = {
            let table = match self.get_table_mut_for_account(account, table_name) {
                Ok(t) => t,
                Err(_) => return keys,
            };
            let map = if is_dict { &table.dictionary } else { &table.records };
            keys.iter()
                .map(|k| match map.get(k) {
                    Some(r) => Self::sort_key_for(k, r, &resolved),
                    None => vec![String::new(); resolved.len()],
                })
                .collect()
        };

        let order = Self::sorted_order(&sort_keys, &resolved, |i| keys[i].as_str());

        let mut taken: Vec<Option<String>> = keys.into_iter().map(Some).collect();
        order.into_iter().map(|i| taken[i].take().unwrap()).collect()
    }

    pub(crate) fn compare_sort_values(left: &str, right: &str) -> std::cmp::Ordering {
        let l = left.trim();
        let r = right.trim();
        if let (Ok(lf), Ok(rf)) = (l.parse::<f64>(), r.parse::<f64>()) {
            return lf.partial_cmp(&rf).unwrap_or(std::cmp::Ordering::Equal);
        }
        l.cmp(r)
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
