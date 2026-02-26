use std::collections::BTreeMap;

use crate::models::{Column, SchemaInfo};

/// Group tables by schema name
pub fn group_tables_by_schema(rows: Vec<(String, String)>) -> Vec<SchemaInfo> {
    let mut schema_map: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (schema, table) in rows {
        schema_map.entry(schema).or_default().push(table);
    }

    schema_map
        .into_iter()
        .map(|(name, tables)| SchemaInfo {
            name,
            tables,
            expanded: false,
        })
        .collect()
}

/// Build UPDATE query SET clause and WHERE clause
/// WHERE clause uses only primary key columns if available, otherwise falls back to all columns
pub fn build_update_clauses(
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
    quote_start: char,
    quote_end: char,
) -> (String, String) {
    // Build SET clause only for changed values
    let set_parts: Vec<String> = columns
        .iter()
        .zip(original_values.iter().zip(new_values.iter()))
        .filter(|(_, (orig, new))| orig != new)
        .map(|(col, (_, new))| {
            if new == "NULL" {
                format!("{}{}{} = NULL", quote_start, col.name, quote_end)
            } else {
                let escaped_value = new.replace('\'', "''");
                let escaped_value = if is_bit_type(&col.type_name) {
                    map_bit_value(&escaped_value)
                } else {
                    escaped_value
                };
                if is_numeric_type(&col.type_name) {
                    format!(
                        "{}{}{} = {}",
                        quote_start, col.name, quote_end, escaped_value
                    )
                } else {
                    format!(
                        "{}{}{} = '{}'",
                        quote_start, col.name, quote_end, escaped_value
                    )
                }
            }
        })
        .collect();

    // Check if we have primary key columns
    let has_primary_keys = columns.iter().any(|c| c.is_primary_key);

    // Build WHERE clause using primary keys only (if available) or all columns as fallback
    let where_parts: Vec<String> = columns
        .iter()
        .zip(original_values.iter())
        .filter(|(col, _)| !has_primary_keys || col.is_primary_key)
        .map(|(col, val)| {
            if val == "NULL" {
                format!("{}{}{} IS NULL", quote_start, col.name, quote_end)
            } else {
                let escaped_value = val.replace('\'', "''");
                let escaped_value = if is_bit_type(&col.type_name) {
                    map_bit_value(&escaped_value)
                } else {
                    escaped_value
                };
                if is_numeric_type(&col.type_name) {
                    format!(
                        "{}{}{} = {}",
                        quote_start, col.name, quote_end, escaped_value
                    )
                } else {
                    format!(
                        "{}{}{} = '{}'",
                        quote_start, col.name, quote_end, escaped_value
                    )
                }
            }
        })
        .collect();

    (set_parts.join(", "), where_parts.join(" AND "))
}

/// Check if a SQL type is numeric (should not be quoted)
fn is_numeric_type(type_name: &str) -> bool {
    let type_lower = type_name.to_lowercase();
    type_lower.contains("int")
        || type_lower.contains("serial")
        || type_lower.contains("float")
        || type_lower.contains("double")
        || type_lower.contains("decimal")
        || type_lower.contains("numeric")
        || type_lower.contains("real")
        || type_lower.contains("money")
        || type_lower.contains("number")
        || type_lower == "bit"
}

/// Check if a SQL type is a BIT type
fn is_bit_type(type_name: &str) -> bool {
    type_name.to_lowercase() == "bit"
}

/// Map boolean string values ("true"/"false") to bit values ("1"/"0")
fn map_bit_value(value: &str) -> String {
    match value.to_lowercase().as_str() {
        "true" => "1".to_string(),
        "false" => "0".to_string(),
        _ => value.to_string(),
    }
}

/// Build INSERT query column list and values list
/// Returns (columns_part, values_part) excluding system-generated columns
pub fn build_insert_parts(
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
    quote_start: char,
    quote_end: char,
) -> (String, String) {
    let mut col_parts: Vec<String> = Vec::new();
    let mut val_parts: Vec<String> = Vec::new();

    for (idx, (col, val)) in columns.iter().zip(values.iter()).enumerate() {
        // Skip system-generated columns
        if system_columns.contains(&idx) {
            continue;
        }

        col_parts.push(format!("{}{}{}", quote_start, col.name, quote_end));

        if val == "NULL" || val.is_empty() {
            val_parts.push("NULL".to_string());
        } else {
            let escaped_value = val.replace('\'', "''");
            let escaped_value = if is_bit_type(&col.type_name) {
                map_bit_value(&escaped_value)
            } else {
                escaped_value
            };
            if is_numeric_type(&col.type_name) {
                val_parts.push(escaped_value);
            } else {
                val_parts.push(format!("'{}'", escaped_value));
            }
        }
    }

    (col_parts.join(", "), val_parts.join(", "))
}

/// Build a complete UPDATE query string
pub fn build_update_query(
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
    quote_start: char,
    quote_end: char,
) -> Option<String> {
    let (set_clause, where_clause) =
        build_update_clauses(columns, original_values, new_values, quote_start, quote_end);

    if set_clause.is_empty() {
        return None;
    }

    Some(format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    ))
}

/// Build a complete INSERT query string
pub fn build_insert_query(
    table_name: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
    quote_start: char,
    quote_end: char,
) -> Option<String> {
    let (columns_part, values_part) =
        build_insert_parts(columns, values, system_columns, quote_start, quote_end);

    if columns_part.is_empty() {
        return None;
    }

    Some(format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name, columns_part, values_part
    ))
}

/// Build a WHERE clause for identifying a specific row (for DELETE).
/// Uses primary key columns if available, otherwise all columns.
pub fn build_where_clause(
    columns: &[Column],
    values: &[String],
    quote_start: char,
    quote_end: char,
) -> String {
    let has_primary_keys = columns.iter().any(|c| c.is_primary_key);

    let where_parts: Vec<String> = columns
        .iter()
        .zip(values.iter())
        .filter(|(col, _)| !has_primary_keys || col.is_primary_key)
        .map(|(col, val)| {
            if val == "NULL" {
                format!("{}{}{} IS NULL", quote_start, col.name, quote_end)
            } else {
                let escaped_value = val.replace('\'', "''");
                let escaped_value = if is_bit_type(&col.type_name) {
                    map_bit_value(&escaped_value)
                } else {
                    escaped_value
                };
                if is_numeric_type(&col.type_name) {
                    format!(
                        "{}{}{} = {}",
                        quote_start, col.name, quote_end, escaped_value
                    )
                } else {
                    format!(
                        "{}{}{} = '{}'",
                        quote_start, col.name, quote_end, escaped_value
                    )
                }
            }
        })
        .collect();

    where_parts.join(" AND ")
}

/// Build a complete DELETE query string for a single row
pub fn build_delete_query(
    table_name: &str,
    columns: &[Column],
    values: &[String],
    quote_start: char,
    quote_end: char,
) -> String {
    let where_clause = build_where_clause(columns, values, quote_start, quote_end);
    format!("DELETE FROM {} WHERE {}", table_name, where_clause)
}
