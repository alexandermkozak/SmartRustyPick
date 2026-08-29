use crate::db::engine::Database;
use crate::db::models::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct FieldQueryInfo {
    pub index: usize,
    pub conversion: Option<String>,
}

/// One record's value for one sort spec, resolved once so that comparing is
/// only comparing.
///
/// A sort does O(n log n) comparisons over n values. Deriving anything inside
/// the comparator therefore pays for it n log n times, when the input it is
/// derived from only ever changes n times: at 10,000 records that is roughly a
/// 14x multiplier on the same work. Both derivations the ordering needs - the
/// numeric reading of the text and its lowercase form - are moved here, in
/// front of the sort.
#[derive(Clone, Debug, Default)]
pub(crate) struct SortValue {
    /// The trimmed text, which is what the ordering is defined over.
    text: String,
    /// `Some` when `text` parses as a number. Two values compare numerically
    /// only when both do, so that a numeric column containing one stray label
    /// still orders sensibly rather than half-numerically.
    number: Option<f64>,
    /// `text` lowercased, or `None` when lowercasing leaves it unchanged -
    /// which is the common case (digits, keys, already-lowercase text), and
    /// worth not allocating a second string for.
    lower: Option<String>,
}

impl SortValue {
    pub(crate) fn new(raw: &str) -> Self {
        let text = raw.trim();
        let number = text.parse::<f64>().ok();
        // Collecting the lowercase form is equivalent to comparing
        // `char::to_lowercase` iterators lazily, because UTF-8 orders its bytes
        // the same way Unicode orders its code points.
        let lower: String = text.chars().flat_map(char::to_lowercase).collect();
        let lower = if lower == text { None } else { Some(lower) };
        SortValue { text: text.to_string(), number, lower }
    }

    /// The form the case-insensitive comparison is made over.
    fn folded(&self) -> &str {
        self.lower.as_deref().unwrap_or(&self.text)
    }

    pub(crate) fn compare(&self, other: &Self) -> std::cmp::Ordering {
        if let (Some(l), Some(r)) = (self.number, other.number) {
            return l.partial_cmp(&r).unwrap_or(std::cmp::Ordering::Equal);
        }
        // Text comparison is case-insensitive so that "Ztest" and "ztest" sort together instead of
        // all uppercase values coming before all lowercase ones. Ties fall back to the raw byte
        // comparison to keep the ordering deterministic.
        let ci = self.folded().cmp(other.folded());
        if ci != std::cmp::Ordering::Equal {
            return ci;
        }
        self.text.cmp(&other.text)
    }
}

impl Database {
    pub fn parse_query(&mut self, table_name: &str, parts: &[&str]) -> Option<QueryNode> {
        self.parse_query_read_only(table_name, parts)
    }

    /// Parsing needs nothing from the database; this variant exists so a caller
    /// holding only a shared reference can build a query too.
    pub fn parse_query_read_only(&self, _table_name: &str, parts: &[&str]) -> Option<QueryNode> {
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
        let _ = self.get_table_mut_for_account(account, table_name);
        let table = match self.get_table_read_only_for_account(account, table_name) {
            Some(t) => t,
            None => return,
        };
        Self::sort_results_in(table, results, specs);
    }

    /// Same as [`sort_results_for_account`], but for a caller that has already
    /// resolved the table, so no lookup is repeated here. Unknown fields compare
    /// equal, so they simply leave the order untouched.
    ///
    /// Generic over `T: Borrow<Record>` so it works both for owned results and
    /// for the borrowed records returned by [`query_in`](Self::query_in),
    /// without cloning.
    pub fn sort_results_in<T: std::borrow::Borrow<Record>>(table: &Table, results: &mut Vec<(String, T)>, specs: &[SortSpec]) {
        if specs.is_empty() { return; }

        let resolved = Self::resolve_sort_fields(table, specs);

        // Pre-calculate the sort values once per record instead of on every comparison.
        let sort_keys: Vec<Vec<SortValue>> = results
            .iter()
            .map(|(id, record)| Self::sort_key_for(id, record.borrow(), &resolved))
            .collect();

        let order = Self::sorted_order(&sort_keys, &resolved, |i| results[i].0.as_str());

        let mut taken: Vec<Option<(String, T)>> = results.drain(..).map(Some).collect();
        results.extend(order.into_iter().map(|i| taken[i].take().unwrap()));
    }

    /// Resolves each sort spec to a field index. `None` means the record ID, `Some(usize::MAX)`
    /// marks an unknown field, which compares equal so the ordering stays stable.
    fn resolve_sort_fields(table: &Table, specs: &[SortSpec]) -> Vec<(Option<usize>, bool)> {
        let mut resolved: Vec<(Option<usize>, bool)> = Vec::with_capacity(specs.len());
        for spec in specs {
            if spec.field_name == "ID" {
                resolved.push((None, spec.descending));
            } else {
                match table.field_index(&spec.field_name) {
                    Some(i) => resolved.push((Some(i), spec.descending)),
                    None => resolved.push((Some(usize::MAX), spec.descending)),
                }
            }
        }
        resolved
    }

    /// Builds the pre-calculated sort values of a single record, one per sort spec.
    fn sort_key_for(id: &str, record: &Record, resolved: &[(Option<usize>, bool)]) -> Vec<SortValue> {
        resolved
            .iter()
            .map(|(idx, _)| match idx {
                None => SortValue::new(id),
                // An unknown field compares equal, so `sorted_order` skips it
                // entirely; there is nothing to resolve.
                Some(i) if *i == usize::MAX => SortValue::default(),
                Some(i) => SortValue::new(&record.get_field_display_string(*i)),
            })
            .collect()
    }

    /// Sorts indices `0..sort_keys.len()` by the pre-calculated values, falling back to the ID.
    fn sorted_order<'a, F: Fn(usize) -> &'a str>(sort_keys: &[Vec<SortValue>], resolved: &[(Option<usize>, bool)], id_of: F) -> Vec<usize> {
        let mut order: Vec<usize> = (0..sort_keys.len()).collect();
        order.sort_by(|&a, &b| {
            for (n, (idx, descending)) in resolved.iter().enumerate() {
                if matches!(idx, Some(i) if *i == usize::MAX) { continue; }
                let mut ord = sort_keys[a][n].compare(&sort_keys[b][n]);
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
        if self.get_table_mut_for_account(account, table_name).is_err() { return keys; }
        match self.get_table_read_only_for_account(account, table_name) {
            Some(table) => Self::sort_keys_in(table, is_dict, keys, specs),
            None => keys,
        }
    }

    /// Same as [`sort_keys_for_account`], but for a caller that has already
    /// resolved the table.
    pub fn sort_keys_in(table: &Table, is_dict: bool, keys: Vec<String>, specs: &[SortSpec]) -> Vec<String> {
        if specs.is_empty() { return keys; }

        let resolved = Self::resolve_sort_fields(table, specs);

        // Look the records up by reference and pre-calculate their sort values; no record is cloned.
        let sort_keys: Vec<Vec<SortValue>> = {
            let map = if is_dict { &table.dictionary } else { &table.records };
            keys.iter()
                .map(|k| match map.get(k) {
                    Some(r) => Self::sort_key_for(k, r, &resolved),
                    None => vec![SortValue::default(); resolved.len()],
                })
                .collect()
        };

        let order = Self::sorted_order(&sort_keys, &resolved, |i| keys[i].as_str());

        let mut taken: Vec<Option<String>> = keys.into_iter().map(Some).collect();
        order.into_iter().map(|i| taken[i].take().unwrap()).collect()
    }

    pub fn query(&mut self, table_name: &str, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        let account = self.current_account.clone();
        self.query_for_account(&account, table_name, use_dict_section, query, keys_to_filter)
    }

    pub fn query_for_account(&mut self, account: &str, table_name: &str, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        if self.get_table_mut_for_account(account, table_name).is_err() {
            return Vec::new(); // Return empty results if table not found
        }
        match self.get_table_read_only_for_account(account, table_name) {
            Some(table) => Self::query_in(table, use_dict_section, query, keys_to_filter)
                .into_iter()
                .map(|(key, record)| (key, record.clone()))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Same as [`query_for_account`], but for a caller that has already resolved
    /// the table. Returns borrowed records, so a caller that already holds the
    /// database lock (e.g. serializing results immediately) does not pay for a
    /// clone of every matching record.
    pub fn query_in<'a>(table: &'a Table, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, &'a Record)> {
        // Pre-calculate field indices and conversions to avoid repeated lookups per record
        let mut field_map = HashMap::new();
        Self::collect_field_indices(table, query, &mut field_map);

        let mut results = Vec::new();

        let source_map = if use_dict_section {
            &table.dictionary
        } else {
            &table.records
        };

        if let Some(filter_keys) = keys_to_filter {
            for key in filter_keys {
                if let Some(record) = source_map.get(key) {
                    if Self::evaluate_node_static_with_id(key, record, query, &field_map) {
                        results.push((key.clone(), record));
                    }
                }
            }
        } else {
            // Optimize: Filter before sorting.
            // Avoid cloning the entire table by using an iterator.
            results = source_map.iter()
                .filter(|(key, record)| Self::evaluate_node_static_with_id(key, record, query, &field_map))
                .map(|(key, record)| (key.clone(), record))
                .collect();
            results.sort_by(|a, b| a.0.cmp(&b.0));
        }

        results
    }


    pub(crate) fn collect_field_indices(table: &Table, node: &QueryNode, map: &mut HashMap<String, FieldQueryInfo>) {
        match node {
            QueryNode::Condition(cond) => {
                if cond.field_name == "ID" { return; }
                if !map.contains_key(&cond.field_name) {
                    if let Some((idx, conversion)) = table.field_index_and_conversion(&cond.field_name) {
                        map.insert(cond.field_name.clone(), FieldQueryInfo { index: idx, conversion });
                    }
                }
            }
            QueryNode::Logical { left, right, .. } => {
                Self::collect_field_indices(table, left, map);
                Self::collect_field_indices(table, right, map);
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
