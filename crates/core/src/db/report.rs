//! The formatted `LIST` report.
//!
//! This lived inline in the CLI, which meant the only renderer in the system
//! was one no test could reach - the CLI crate has no tests, and the functions
//! that built the report printed rather than returned. Building the lines here
//! and letting the caller print them keeps the layout unchanged while making it
//! testable, and puts one renderer where both the CLI and the server can reach
//! it.

use crate::db::engine::Database;
use crate::db::models::{Record, SelectEntry, Table};

/// The `ID` column is not dictionary-driven; it is always first and always this
/// wide.
const ID_WIDTH: usize = 10;

struct FieldFormat {
    name: String,
    header: String,
    width: usize,
    justify: String,
}

impl FieldFormat {
    /// Pads or truncates a cell to the column's width, honouring its
    /// justification.
    fn cell(&self, text: &str) -> String {
        if self.justify == "R" {
            format!("{:>width$.width$}", text, width = self.width)
        } else {
            format!("{:<width$.width$}", text, width = self.width)
        }
    }
}

/// Expands `*` to every dictionary field, keeping the caller's order otherwise.
fn expand_columns(table: &Table, columns: &[String]) -> Vec<String> {
    let mut expanded = Vec::with_capacity(columns.len());
    for name in columns {
        if name == "*" {
            expanded.extend(Database::all_dict_fields_in(table));
        } else {
            expanded.push(name.clone());
        }
    }
    expanded
}

/// Renders the header, the separator rule and one line per row.
///
/// Each row carries the position that put it in the result. That position
/// belongs to `explode_field` alone: only that column narrows to the single
/// value that matched, while every other column repeats the record's whole
/// field. Applying it everywhere would blank out every single-valued column on
/// any row past the first.
///
/// The table is passed in rather than looked up, because the caller is holding
/// its lock already - a row's records are borrowed out of it. Resolving the
/// dictionary from the table also means one lookup per column instead of one
/// per column per row.
pub fn render_list<T: std::borrow::Borrow<Record>>(
    table: &Table,
    columns: &[String],
    explode_field: Option<&str>,
    rows: &[(SelectEntry, T)],
) -> Vec<String> {
    let mut formats = vec![FieldFormat {
        name: "ID".to_string(),
        header: "ID".to_string(),
        width: ID_WIDTH,
        justify: "L".to_string(),
    }];
    for name in expand_columns(table, columns) {
        formats.push(FieldFormat {
            header: Database::field_header_in(table, &name),
            width: Database::field_width_in(table, &name),
            justify: Database::field_justification_in(table, &name),
            name,
        });
    }

    let mut lines = Vec::with_capacity(rows.len() + 2);

    let mut header_line = String::new();
    let mut separator_line = String::new();
    for (i, fmt) in formats.iter().enumerate() {
        if i > 0 {
            header_line.push(' ');
            separator_line.push(' ');
        }
        header_line.push_str(&fmt.cell(&fmt.header));
        separator_line.push_str(&"-".repeat(fmt.width));
    }
    lines.push(header_line);
    lines.push(separator_line);

    for (entry, record) in rows {
        let mut row_line = String::new();
        for (i, fmt) in formats.iter().enumerate() {
            if i > 0 {
                row_line.push(' ');
            }
            let value = if fmt.name == "ID" {
                entry.key.clone()
            } else {
                let position = (Some(fmt.name.as_str()) == explode_field)
                    .then_some(entry.position)
                    .flatten();
                Database::format_record_field_at_in(table, record.borrow(), &fmt.name, position)
            };
            row_line.push_str(&fmt.cell(&value));
        }
        lines.push(row_line);
    }

    lines
}
