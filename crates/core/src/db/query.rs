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
    pub fn parse_query(&self, table_name: &str, parts: &[&str]) -> Option<QueryNode> {
        self.parse_query_read_only(table_name, parts)
    }

    /// Parsing needs nothing from the database; this variant exists so a caller
    /// holding only a shared reference can build a query too.
    pub fn parse_query_read_only(&self, table_name: &str, parts: &[&str]) -> Option<QueryNode> {
        self.parse_query_consuming(table_name, parts).0
    }

    /// Parses `WITH <field> <op> <value> [AND|OR <field> <op> <value> ...]` and
    /// reports how many tokens it consumed.
    ///
    /// The count is what lets `LIST USERS WITH NAME = "Bob" NAME EMAIL` work:
    /// the criteria run out, and everything after them is a column list rather
    /// than a malformed condition.
    pub fn parse_query_consuming(&self, _table_name: &str, parts: &[&str]) -> (Option<QueryNode>, usize) {
        if parts.is_empty() { return (None, 0); }
        let mut i = if parts[0].to_uppercase() == "WITH" { 1 } else { 0 };

        // A condition is three tokens; anything shorter is not a clause.
        if i + 3 > parts.len() { return (None, 0); }

        let mut current_node = QueryNode::Condition(QueryCondition {
            field_name: parts[i].to_string(),
            op: parts[i + 1].to_string(),
            value: unquote(parts[i + 2]),
        });
        i += 3;

        while i < parts.len() {
            let logical_op = match parts[i].to_uppercase().as_str() {
                "AND" => LogicalOp::And,
                "OR" => LogicalOp::Or,
                // Not a logical operator: the clause ends here, and whatever
                // follows belongs to the caller.
                _ => break,
            };
            if i + 4 > parts.len() {
                // A trailing AND/OR with no condition after it. Leave the token
                // to the caller rather than silently swallowing it.
                break;
            }
            let next_condition = QueryNode::Condition(QueryCondition {
                field_name: parts[i + 1].to_string(),
                op: parts[i + 2].to_string(),
                value: unquote(parts[i + 3]),
            });
            current_node = QueryNode::Logical {
                op: logical_op,
                left: Box::new(current_node),
                right: Box::new(next_condition),
            };
            i += 4;
        }

        (Some(current_node), i)
    }

    /// `AND`s a condition absorbed from a `BY.EXP` clause onto whatever the
    /// `WITH` clause parsed, so the compact spelling filters exactly as the
    /// explicit one does.
    pub fn and_condition(node: Option<QueryNode>, condition: Option<QueryCondition>) -> Option<QueryNode> {
        let condition = QueryNode::Condition(condition?);
        Some(match node {
            Some(existing) => QueryNode::Logical {
                op: LogicalOp::And,
                left: Box::new(existing),
                right: Box::new(condition),
            },
            None => condition,
        })
    }

    /// Splits a clause list into the non-clause tokens, the sort specs and the
    /// explode specs.
    ///
    /// Sort operators are `BY` (ascending) and `BY.DSND` (descending), each
    /// followed by a field name. `BY.EXP` is the explode operator: it names a
    /// multivalued field whose values become one output row each. Where
    /// `BY.EXP <field>` is followed by a recognised comparison operator, the
    /// `<op> <value>` after it is absorbed as a selection criterion, so the
    /// compact `BY.EXP ACCOUNTS = "TEST"` and the explicit
    /// `BY.EXP ACCOUNTS WITH ACCOUNTS = "TEST"` both work. The operator set is
    /// closed, so `BY.EXP ACCOUNTS NAME` still reads `NAME` as a column.
    ///
    /// Any number of operators may be present, anywhere in the clause, and they
    /// are applied from left to right. Tokens that are not part of one keep
    /// their relative order, so sort, explode and column specifiers may be
    /// freely interleaved.
    pub fn parse_clause_specs<'a>(parts: &[&'a str]) -> (Vec<&'a str>, Vec<SortSpec>, Vec<ExplodeSpec>) {
        let mut remaining = Vec::new();
        let mut specs = Vec::new();
        let mut explodes = Vec::new();
        let mut i = 0;
        while i < parts.len() {
            let upper = parts[i].to_uppercase();
            let descending = match upper.as_str() {
                "BY" => false,
                "BY.DSND" => true,
                "BY.EXP" => {
                    if i + 1 >= parts.len() {
                        // Trailing operator without a field name: keep the token
                        // so it is not lost.
                        remaining.push(parts[i]);
                        i += 1;
                        continue;
                    }
                    let field_name = parts[i + 1].to_string();
                    i += 2;
                    // Absorb a trailing `<op> <value>` when the next token is
                    // unambiguously an operator.
                    let condition = if i + 1 < parts.len() && is_comparison_op(parts[i]) {
                        let cond = QueryCondition {
                            field_name: field_name.clone(),
                            op: parts[i].to_string(),
                            value: unquote(parts[i + 1]),
                        };
                        i += 2;
                        Some(cond)
                    } else {
                        None
                    };
                    explodes.push(ExplodeSpec { field_name, condition });
                    continue;
                }
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
        (remaining, specs, explodes)
    }

    pub fn sort_results(&self, table_name: &str, results: &mut Vec<(String, Record)>, specs: &[SortSpec]) {
        self.sort_results_for_account(&self.current_account(), table_name, results, specs);
    }

    pub fn sort_results_for_account(&self, account: &str, table_name: &str, results: &mut Vec<(String, Record)>, specs: &[SortSpec]) {
        if specs.is_empty() { return; }
        let handle = match self.get_table_mut_for_account(account, table_name) {
            Ok(handle) => handle,
            Err(_) => return,
        };
        Self::sort_results_in(&handle.read(), results, specs);
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
        Self::sort_key_at(id, record, resolved, None, None)
    }

    /// Same, for one row of an exploded result. A spec naming the exploded
    /// field (`explode_idx`) sorts on that row's own value rather than on the
    /// whole joined field, so `BY.EXP ACCOUNTS BY ACCOUNTS` orders the rows the
    /// way the reader expects.
    fn sort_key_at(
        id: &str,
        record: &Record,
        resolved: &[(Option<usize>, bool)],
        explode_idx: Option<usize>,
        position: Option<ValuePosition>,
    ) -> Vec<SortValue> {
        resolved
            .iter()
            .map(|(idx, _)| match idx {
                None => SortValue::new(id),
                // An unknown field compares equal, so `sorted_order` skips it
                // entirely; there is nothing to resolve.
                Some(i) if *i == usize::MAX => SortValue::default(),
                Some(i) if Some(*i) == explode_idx => {
                    SortValue::new(&record.get_value_display_string(*i, position))
                }
                Some(i) => SortValue::new(&record.get_field_display_string(*i)),
            })
            .collect()
    }

    /// Sorts the rows of an exploded result set. Rows arrive in key order and,
    /// within a key, in value-position order; a `BY` / `BY.DSND` spec reorders
    /// them, resolving the exploded column per row.
    pub fn sort_entries_in<T: std::borrow::Borrow<Record>>(
        table: &Table,
        rows: &mut Vec<(SelectEntry, T)>,
        specs: &[SortSpec],
        explode_idx: Option<usize>,
    ) {
        if specs.is_empty() { return; }

        let resolved = Self::resolve_sort_fields(table, specs);

        let sort_keys: Vec<Vec<SortValue>> = rows
            .iter()
            .map(|(entry, record)| {
                Self::sort_key_at(&entry.key, record.borrow(), &resolved, explode_idx, entry.position)
            })
            .collect();

        let order = Self::sorted_order(&sort_keys, &resolved, |i| rows[i].0.key.as_str());

        let mut taken: Vec<Option<(SelectEntry, T)>> = rows.drain(..).map(Some).collect();
        rows.extend(order.into_iter().map(|i| taken[i].take().unwrap()));
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

    pub fn sort_keys(&self, table_name: &str, is_dict: bool, keys: Vec<String>, specs: &[SortSpec]) -> Vec<String> {
        self.sort_keys_for_account(&self.current_account(), table_name, is_dict, keys, specs)
    }

    pub fn sort_keys_for_account(&self, account: &str, table_name: &str, is_dict: bool, keys: Vec<String>, specs: &[SortSpec]) -> Vec<String> {
        if specs.is_empty() { return keys; }
        match self.get_table_mut_for_account(account, table_name) {
            Ok(handle) => Self::sort_keys_in(&handle.read(), is_dict, keys, specs),
            Err(_) => keys,
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

    pub fn query(&self, table_name: &str, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        self.query_for_account(&self.current_account(), table_name, use_dict_section, query, keys_to_filter)
    }

    pub fn query_for_account(&self, account: &str, table_name: &str, use_dict_section: bool, query: &QueryNode, keys_to_filter: Option<&[String]>) -> Vec<(String, Record)> {
        let handle = match self.get_table_mut_for_account(account, table_name) {
            Ok(handle) => handle,
            // Return empty results if table not found
            Err(_) => return Vec::new(),
        };
        Self::query_in(&handle.read(), use_dict_section, query, keys_to_filter)
            .into_iter()
            .map(|(key, record)| (key, record.clone()))
            .collect()
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


    /// Runs `query` (if any) and expands each surviving record into one entry
    /// per exploded position.
    ///
    /// Record inclusion is decided exactly as it always was, by evaluating the
    /// whole node. The positions are collected separately, so a record kept by
    /// an `AND` over two fields still explodes only on the field `BY.EXP` named.
    /// With no explode spec this degenerates to [`query_in`](Self::query_in)
    /// with a `None` position on every entry, which is what an ordinary
    /// selection has always produced.
    ///
    /// Returns borrowed records, so a caller holding the database lock does not
    /// pay for a clone of every match.
    pub fn query_exploded_in<'a>(
        table: &'a Table,
        use_dict_section: bool,
        query: Option<&QueryNode>,
        explode: Option<&ExplodeSpec>,
        keys_to_filter: Option<&[String]>,
    ) -> Vec<(SelectEntry, &'a Record)> {
        let matches: Vec<(String, &Record)> = match query {
            Some(q) => Self::query_in(table, use_dict_section, q, keys_to_filter),
            None => Self::all_in(table, use_dict_section, keys_to_filter),
        };

        let resolved = explode.and_then(|spec| {
            let (index, _) = table.field_index_and_conversion(&spec.field_name)?;
            let mut conditions = Vec::new();
            if let Some(q) = query {
                Self::collect_conditions_for(q, &spec.field_name, &mut conditions);
            }
            Some((index, conditions))
        });

        let Some((index, conditions)) = resolved else {
            return matches
                .into_iter()
                .map(|(key, record)| (SelectEntry::new(key), record))
                .collect();
        };

        // Resolve the exploded field's conversion once, rather than per record.
        let mut field_map = HashMap::new();
        if let Some(q) = query {
            Self::collect_field_indices(table, q, &mut field_map);
        }

        let mut results = Vec::new();
        let mut positions = Vec::new();
        for (key, record) in matches {
            positions.clear();
            Self::collect_positions(record, index, &conditions, &field_map, &mut positions);
            if positions.is_empty() {
                // Either the field is absent, or the record was kept by a
                // condition on some other field. Keep it as one unexploded row:
                // inclusion is the query's decision, not the explode clause's.
                results.push((SelectEntry::new(key), record));
            } else {
                for position in &positions {
                    results.push((SelectEntry::at(key.clone(), *position), record));
                }
            }
        }
        results
    }

    /// The field index an exploded list's positions refer to, so a `BY` spec
    /// naming that field can sort on the row's own value rather than on the
    /// whole joined field.
    pub fn explode_field_index(table: &Table, explode: Option<&ExplodeSpec>) -> Option<usize> {
        let spec = explode?;
        table.field_index(&spec.field_name)
    }

    /// Every key of the section, filtered to `keys_to_filter` when given, in the
    /// same key order [`query_in`](Self::query_in) produces.
    fn all_in<'a>(table: &'a Table, use_dict_section: bool, keys_to_filter: Option<&[String]>) -> Vec<(String, &'a Record)> {
        let source_map = if use_dict_section { &table.dictionary } else { &table.records };
        match keys_to_filter {
            Some(filter_keys) => filter_keys
                .iter()
                .filter_map(|key| source_map.get_key_value(key).map(|(k, r)| (k.clone(), r)))
                .collect(),
            None => {
                let mut all: Vec<(String, &Record)> = source_map.iter().map(|(k, r)| (k.clone(), r)).collect();
                all.sort_by(|a, b| a.0.cmp(&b.0));
                all
            }
        }
    }

    /// Gathers every condition in the node that names `field_name`, regardless
    /// of how the node combines them. The positions they match are unioned, so
    /// `WITH ACCOUNTS = "TEST" OR ACCOUNTS = "DEV"` explodes on both.
    fn collect_conditions_for<'q>(node: &'q QueryNode, field_name: &str, out: &mut Vec<&'q QueryCondition>) {
        match node {
            QueryNode::Condition(cond) => {
                if cond.field_name == field_name {
                    out.push(cond);
                }
            }
            QueryNode::Logical { left, right, .. } => {
                Self::collect_conditions_for(left, field_name, out);
                Self::collect_conditions_for(right, field_name, out);
            }
        }
    }

    /// The positions of `field_idx` that satisfy any of `conditions`, in field
    /// order and without duplicates.
    ///
    /// With no conditions every value is a position, which is how a bare
    /// `BY.EXP <field>` explodes a record into one row per value. Unlike
    /// [`evaluate_node_static_with_id`](Self::evaluate_node_static_with_id)
    /// this deliberately does not short-circuit: every match is a row.
    pub(crate) fn collect_positions(
        record: &Record,
        field_idx: usize,
        conditions: &[&QueryCondition],
        field_map: &HashMap<String, FieldQueryInfo>,
        out: &mut Vec<ValuePosition>,
    ) {
        let Some(field) = record.fields.get(field_idx) else { return };

        if conditions.is_empty() {
            out.extend((0..field.values.len()).map(ValuePosition::value));
            return;
        }

        // Input conversion of each search value is resolved once for the field.
        let search_vals: Vec<String> = conditions
            .iter()
            .map(|cond| match field_map.get(&cond.field_name).and_then(|i| i.conversion.as_ref()) {
                Some(code) => Self::apply_iconv(&cond.value, code),
                None => cond.value.clone(),
            })
            .collect();

        for (v_idx, value) in field.values.iter().enumerate() {
            // A value with one sub-value is an ordinary value, not a sub-valued
            // one, so it reports as a plain value position.
            if value.sub_values.len() <= 1 {
                let text = value.sub_values.first().map(String::as_str).unwrap_or("");
                if Self::any_condition_matches(text, conditions, &search_vals) {
                    push_unique(out, ValuePosition::value(v_idx));
                }
                continue;
            }
            for (sv_idx, sub) in value.sub_values.iter().enumerate() {
                if Self::any_condition_matches(sub, conditions, &search_vals) {
                    push_unique(out, ValuePosition::sub_value(v_idx, sv_idx));
                }
            }
        }
    }

    fn any_condition_matches(text: &str, conditions: &[&QueryCondition], search_vals: &[String]) -> bool {
        conditions
            .iter()
            .zip(search_vals)
            .any(|(cond, search)| Self::compare_values(text, &cond.op, search))
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

/// The comparison operators [`compare_values`](Database::compare_values)
/// understands. `BY.EXP <field>` only absorbs a criterion when the token after
/// the field name is one of these, which is what keeps a bare column name after
/// an explode readable as a column.
pub fn is_comparison_op(token: &str) -> bool {
    matches!(
        token.to_uppercase().as_str(),
        "=" | "EQ" | "!=" | "#" | "<>" | "NE" | "<" | "LT" | ">" | "GT" | "<=" | "LE" | ">=" | "GE"
    )
}

/// Strips one layer of surrounding double quotes from a clause value.
pub fn unquote(raw: &str) -> String {
    let value = if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    value.trim().to_string()
}

fn push_unique(out: &mut Vec<ValuePosition>, pos: ValuePosition) {
    if !out.contains(&pos) {
        out.push(pos);
    }
}
