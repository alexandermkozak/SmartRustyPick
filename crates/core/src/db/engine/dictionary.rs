//! What a dictionary entry says about a field, and what the display of that
//! field is therefore made of: its attribute number, heading, width,
//! justification and conversion code.
//!
//! These sit together because they are one subject, and because none of them is
//! governed by the locking discipline the rest of the engine is written around.
//! Every one of them takes a [`Table`] the caller has already resolved - or an
//! account and file name it resolves to one, once, up front - and reads the
//! dictionary map inside it. Not one takes a second lock while holding a table
//! handle, so nothing here can participate in a lock cycle.
//!
//! # Conversion codes
//!
//! A conversion code is attribute 8 of a dictionary entry: a short string
//! naming the transformation between how a field is *stored* and how it is
//! *shown*. `MD2` on a price field means the record holds `12345` and a reader
//! is shown `123.45`; the same code run backwards is what turns a written
//! `123.45` back into `12345`. Output conversion (OCONV) is
//! [`Database::apply_conversion`], input conversion (ICONV) is
//! [`Database::apply_iconv`].
//!
//! Of the codes `docs/data_structures.md` documents under Dictionary Items -
//! `MDn`, `MRn`, `D4-`, `D2/` - only `MDn` is implemented here. The rest, and
//! anything else an entry carries in attribute 8, pass the value through
//! unchanged: a code we do not recognise is a dictionary written for a
//! conversion nobody has taught us yet, not a corrupt record, so the stored
//! text is shown as it stands rather than the read failing. The attribute
//! positions themselves are the `DICT_*_IDX` constants in
//! [`crate::db::models`]; that document is what they mean to someone writing a
//! dictionary, and this module is what they do at run time.
//!
//! # The default width
//!
//! [`DEFAULT_FIELD_WIDTH`] is one rule applied at two moments, which is why it
//! is one constant and not two that happen to agree. `LIST` falls back to it
//! when an entry carries no width, and `SET.DICT` stores it when the caller
//! names none - and those have to be the same number, because otherwise "no
//! width given" would mean one width for an entry written by hand and another
//! for an entry created over the protocol. The server's `DEFAULT_DICT_WIDTH` is
//! therefore derived from this constant rather than declared beside it, so
//! changing this changes both.

use super::Database;
use crate::db::models::*;
use std::collections::HashMap;

/// The display width a field without a dictionary width is rendered at, and the
/// width `SET.DICT` stores for an entry whose caller named none. See
/// [The default width](self#the-default-width) for why those are one constant.
pub const DEFAULT_FIELD_WIDTH: usize = 10;

impl Table {
    /// The 0-based index of a dictionary field, or `None` when the field is
    /// unknown. Reading it off the table directly spares the caller a lookup in
    /// `loaded_tables`, which costs two string allocations per call.
    pub fn field_index(&self, field_name: &str) -> Option<usize> {
        if field_name == "ID" {
            return Some(0);
        }
        let rec = self.dictionary.get(field_name)?;
        let idx_str = rec.fields.get(DICT_FIELD_IDX)?.values.first()?.first_text()?;
        match idx_str.parse::<usize>() {
            // Pick attribute 1 is 0-indexed 0 in our internal fields vector
            Ok(idx) if idx > 0 => Some(idx - 1),
            _ => None,
        }
    }

    /// The Pick MDn conversion code of a dictionary field, if it has one.
    pub fn conversion_code(&self, field_name: &str) -> Option<String> {
        let rec = self.dictionary.get(field_name)?;
        Self::conversion_code_from_dict_record(rec).map(str::to_string)
    }

    /// Same as [`conversion_code`], but for a caller that already has the
    /// dictionary record in hand, sparing a second lookup in `dictionary`.
    pub(crate) fn conversion_code_from_dict_record(dict_rec: &Record) -> Option<&str> {
        // Pick MDn conversion is in Field 8
        let code = dict_rec.fields.get(DICT_CONV_IDX)?.values.first()?.sub_values.first()?;
        // A conversion code that is not text is not a conversion code.
        match std::str::from_utf8(code) {
            Ok(code) if !code.is_empty() => Some(code),
            _ => None,
        }
    }

    /// The 0-based index and conversion code of a dictionary field in a single
    /// dictionary lookup, instead of one lookup per property.
    pub fn field_index_and_conversion(&self, field_name: &str) -> Option<(usize, Option<String>)> {
        if field_name == "ID" {
            return Some((0, None));
        }
        let rec = self.dictionary.get(field_name)?;
        let idx_str = rec.fields.get(DICT_FIELD_IDX)?.values.first()?.first_text()?;
        let idx = match idx_str.parse::<usize>() {
            // Pick attribute 1 is 0-indexed 0 in our internal fields vector
            Ok(idx) if idx > 0 => idx - 1,
            _ => return None,
        };
        Some((idx, Self::conversion_code_from_dict_record(rec).map(str::to_string)))
    }
}

impl Database {
    pub fn get_conversion_code_read_only(&self, table_name: &str, field_name: &str) -> Option<String> {
        self.get_conversion_code_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_conversion_code_read_only_for_account(
        &self,
        account: &str,
        table_name: &str,
        field_name: &str,
    ) -> Option<String> {
        self.get_table_read_only_for_account(account, table_name)?
            .read()
            .conversion_code(field_name)
    }

    pub fn get_conversion_code(&self, table_name: &str, field_name: &str) -> Option<String> {
        self.get_conversion_code_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_header_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_header_in(&handle.read(), field_name),
            None => field_name.to_string(),
        }
    }

    /// The column heading of a field, read from a table the caller has already
    /// resolved.
    ///
    /// The `_in` variants exist for a caller that is holding the table's lock
    /// already - a report renderer walking every column of every row. Going
    /// back through the database for each one would take that same lock again,
    /// which is both wasteful and, with a writer waiting in between, a way to
    /// deadlock against ourselves.
    pub fn field_header_in(table: &Table, field_name: &str) -> String {
        if field_name == "ID" {
            return "ID".to_string();
        }
        if let Some(rec) = table.dictionary.get(field_name)
            && let Some(f2) = rec.fields.get(DICT_NAME_IDX)
            && let Some(v1) = f2.values.first()
            && let Some(header) = v1.first_text()
            && !header.is_empty()
        {
            return header.to_string();
        }
        field_name.to_string()
    }

    pub fn get_field_width_read_only_for_account(&self, account: &str, table_name: &str, field_name: &str) -> usize {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_width_in(&handle.read(), field_name),
            None => DEFAULT_FIELD_WIDTH,
        }
    }

    /// The display width of a field. See [`field_header_in`](Self::field_header_in).
    pub fn field_width_in(table: &Table, field_name: &str) -> usize {
        if field_name == "ID" {
            return DEFAULT_FIELD_WIDTH;
        }
        if let Some(rec) = table.dictionary.get(field_name)
            && let Some(f4) = rec.fields.get(DICT_WIDTH_IDX)
            && let Some(v1) = f4.values.first()
            && let Some(width_str) = v1.first_text()
            && let Ok(width) = width_str.parse::<usize>()
        {
            return width;
        }
        DEFAULT_FIELD_WIDTH
    }

    pub fn get_field_justification_read_only_for_account(
        &self,
        account: &str,
        table_name: &str,
        field_name: &str,
    ) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::field_justification_in(&handle.read(), field_name),
            None => "L".to_string(),
        }
    }

    /// The justification of a field. See [`field_header_in`](Self::field_header_in).
    pub fn field_justification_in(table: &Table, field_name: &str) -> String {
        if field_name == "ID" {
            return "L".to_string();
        }
        if let Some(rec) = table.dictionary.get(field_name)
            && let Some(f3) = rec.fields.get(DICT_JUSTIFY_IDX)
            && let Some(v1) = f3.values.first()
            && let Some(just) = v1.first_text()
            && !just.is_empty()
        {
            return just.to_string();
        }
        "L".to_string()
    }

    pub fn get_all_dict_fields_read_only_for_account(&self, account: &str, table_name: &str) -> Vec<String> {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::all_dict_fields_in(&handle.read()),
            None => Vec::new(),
        }
    }

    /// Every dictionary field, in attribute order. See
    /// [`field_header_in`](Self::field_header_in).
    pub fn all_dict_fields_in(table: &Table) -> Vec<String> {
        let mut fields_map: HashMap<usize, String> = HashMap::new();
        let mut keys: Vec<_> = table.dictionary.keys().cloned().collect();
        keys.sort(); // Consistent order for "picking the first"

        for key in keys {
            if let Some(record) = table.dictionary.get(&key)
                && let Some(field_idx_str) = record
                    .fields
                    .get(DICT_FIELD_IDX)
                    .and_then(|f| f.values.first())
                    .and_then(|v| v.first_text())
                && let Ok(idx) = field_idx_str.parse::<usize>()
                && idx > 0
                && !fields_map.contains_key(&idx)
            {
                fields_map.insert(idx, key);
            }
        }

        let mut sorted_indices: Vec<_> = fields_map.keys().cloned().collect();
        sorted_indices.sort();

        sorted_indices
            .into_iter()
            .map(|idx| fields_map.get(&idx).unwrap().clone())
            .collect()
    }

    pub fn apply_conversion(val: &str, code: &str) -> String {
        if code.starts_with("MD")
            && code.len() > 2
            && let Ok(decimals) = code[2..].parse::<usize>()
        {
            let divisor = 10f64.powi(decimals as i32);
            if let Ok(num) = val.parse::<i64>() {
                let mut s = format!("{:.width$}", num as f64 / divisor, width = decimals);
                if decimals == 0 {
                    s = format!("{}", num);
                }
                return s;
            } else if let Ok(f) = val.parse::<f64>() {
                // Robustness: handle cases where data might already be stored with a decimal point
                let mut s = format!("{:.width$}", f / divisor, width = decimals);
                if decimals == 0 {
                    s = format!("{}", f.round() as i64);
                }
                return s;
            }
        }
        val.to_string()
    }

    pub fn apply_iconv(val: &str, code: &str) -> String {
        if code.starts_with("MD")
            && code.len() > 2
            && let Ok(decimals) = code[2..].parse::<usize>()
            && let Ok(f) = val.parse::<f64>()
        {
            let multiplier = 10f64.powi(decimals as i32);
            return format!("{:.0}", (f * multiplier).round());
        }
        val.to_string()
    }

    /// Applies an output conversion to each value and sub-value of a field
    /// rather than to the whole field.
    ///
    /// The field's display string joins its values with `]` and sub-values with
    /// `\\`, so handing that to [`apply_conversion`](Self::apply_conversion)
    /// gives it something like `"200]300"`, which parses as no number at all
    /// and comes back unconverted. Splitting on the marks first means an `MD2`
    /// column of a multivalued field converts the way a single-valued one does.
    fn convert_display_string(raw: &str, code: &str) -> String {
        if !raw.contains([']', '\\']) {
            return Self::apply_conversion(raw, code);
        }
        raw.split(']')
            .map(|value| {
                value
                    .split('\\')
                    .map(|sub| Self::apply_conversion(sub, code))
                    .collect::<Vec<_>>()
                    .join("\\")
            })
            .collect::<Vec<_>>()
            .join("]")
    }

    pub fn format_record_field(&self, table_name: &str, record: &Record, field_name: &str) -> String {
        self.format_record_field_for_account(&self.current_account(), table_name, record, field_name)
    }

    pub fn format_record_field_for_account(
        &self,
        account: &str,
        table_name: &str,
        record: &Record,
        field_name: &str,
    ) -> String {
        self.format_record_field_at_for_account(account, table_name, record, field_name, None)
    }

    /// Renders one column of one output row. `position` is the row's exploded
    /// position, so an exploded column shows only the value (or sub-value) that
    /// put the row there; `None` renders the whole field, which is what every
    /// unexploded row does.
    pub fn format_record_field_at(
        &self,
        table_name: &str,
        record: &Record,
        field_name: &str,
        position: Option<ValuePosition>,
    ) -> String {
        self.format_record_field_at_for_account(&self.current_account(), table_name, record, field_name, position)
    }

    pub fn format_record_field_at_for_account(
        &self,
        account: &str,
        table_name: &str,
        record: &Record,
        field_name: &str,
        position: Option<ValuePosition>,
    ) -> String {
        match self.get_table_read_only_for_account(account, table_name) {
            Some(handle) => Self::format_record_field_at_in(&handle.read(), record, field_name, position),
            None => String::new(),
        }
    }

    /// Renders one column of one row from a table the caller has already
    /// resolved, in a single dictionary lookup. See
    /// [`field_header_in`](Self::field_header_in).
    pub fn format_record_field_at_in(
        table: &Table,
        record: &Record,
        field_name: &str,
        position: Option<ValuePosition>,
    ) -> String {
        let (field_idx, conv) = match table.field_index_and_conversion(field_name) {
            Some(resolved) => resolved,
            None => return String::new(),
        };

        let raw_val = record.get_value_display_string(field_idx, position);
        match conv {
            Some(code) => Self::convert_display_string(&raw_val, &code),
            None => raw_val,
        }
    }

    pub fn get_field_index_read_only(&self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_read_only_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_index_read_only_for_account(
        &self,
        account: &str,
        table_name: &str,
        field_name: &str,
    ) -> Option<usize> {
        if field_name == "ID" {
            return Some(0);
        }
        self.get_table_read_only_for_account(account, table_name)?
            .read()
            .field_index(field_name)
    }

    pub fn get_field_index(&self, table_name: &str, field_name: &str) -> Option<usize> {
        self.get_field_index_for_account(&self.current_account(), table_name, field_name)
    }

    pub fn get_field_index_for_account(&self, account: &str, table_name: &str, field_name: &str) -> Option<usize> {
        if field_name == "ID" {
            return Some(0);
        }
        let _ = self.get_table_mut_for_account(account, table_name).ok();
        self.get_field_index_read_only_for_account(account, table_name, field_name)
    }
}
