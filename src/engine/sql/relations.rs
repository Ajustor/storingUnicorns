//! Conditions matching the rows linked by a foreign key (stub, replaced
//! by the engine implementation).
#![allow(dead_code)]

use crate::engine::models::{is_null, DatabaseType, ForeignKeyInfo, IncomingForeignKey};
use crate::engine::sql::statements::{quote_chars, quote_ident};

fn numeric(type_name: &str) -> bool {
    let t = type_name.to_ascii_lowercase();
    ["int", "numeric", "decimal", "real", "double", "float"]
        .iter()
        .any(|k| t.contains(k))
        && !t.contains('[')
}

/// The `WHERE` condition (without the keyword) selecting the rows of
/// `target_columns` equal to `values` (one per column; NULL_CELL → `IS NULL`),
/// quoted for the engine, literals typed from `target_types` when known.
pub fn match_condition(
    target_columns: &[String],
    values: &[String],
    target_types: &[Option<String>],
    db: &DatabaseType,
) -> String {
    let quotes = quote_chars(db);
    target_columns
        .iter()
        .zip(values)
        .enumerate()
        .map(|(i, (col, value))| {
            let col = quote_ident(col, quotes);
            if is_null(value) {
                return format!("{col} IS NULL");
            }
            let typed = target_types.get(i).cloned().flatten();
            let number =
                typed.as_deref().is_some_and(numeric) && value.trim().parse::<f64>().is_ok();
            if number {
                format!("{col} = {}", value.trim())
            } else {
                format!("{col} = '{}'", value.replace('\'', "''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn linked(
    from: &[String],
    to: &[String],
    row: &dyn Fn(&str) -> Option<String>,
    db: &DatabaseType,
) -> Option<String> {
    let mut values = Vec::new();
    for c in from {
        let v = row(c)?;
        if is_null(&v) {
            return None;
        }
        values.push(v);
    }
    Some(match_condition(to, &values, &[], db))
}

/// For an outgoing FK and the current row (column name → value): the
/// referenced table and the condition selecting the referenced row.
/// None when a referencing value is NULL (no linked row).
pub fn follow(
    fk: &ForeignKeyInfo,
    row: &dyn Fn(&str) -> Option<String>,
    db: &DatabaseType,
) -> Option<(String, String)> {
    linked(&fk.columns, &fk.ref_columns, row, db).map(|c| (fk.ref_table.clone(), c))
}

/// For an incoming FK and the current row: the referencing table and the
/// condition selecting the rows that point at it.
pub fn referencing(
    fk: &IncomingForeignKey,
    row: &dyn Fn(&str) -> Option<String>,
    db: &DatabaseType,
) -> Option<(String, String)> {
    linked(&fk.ref_columns, &fk.columns, row, db).map(|c| (fk.table.clone(), c))
}
