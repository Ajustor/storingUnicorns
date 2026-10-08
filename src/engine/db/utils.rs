use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use sqlx::{Database, Either, Executor, IntoArguments};

use crate::engine::models::{Column, SchemaInfo};
use crate::engine::sql::statements::split_statements;

/// Execute `query` exactly once and collect both its rows and its
/// `DB::QueryResult` (affected-row counts summed via `Extend`) from the same
/// `fetch_many` stream. `fetch_all` would discard the affected-row counts,
/// and `execute` would discard the rows.
///
/// With `max_rows`, at most that many rows are kept; the third element is
/// true when at least one more row was available. For a single statement the
/// stream is dropped as soon as the extra row arrives, which stops the fetch.
/// When `query` holds several statements, the rest of the stream is still
/// drained (rows discarded) so that the following statements run: some
/// drivers (SQLite) execute them lazily, while the stream is being read.
pub async fn fetch_rows_and_result<'c, DB, E>(
    executor: E,
    query: &str,
    max_rows: Option<usize>,
) -> sqlx::Result<(Vec<DB::Row>, DB::QueryResult, bool)>
where
    DB: Database,
    E: Executor<'c, Database = DB>,
    for<'q> DB::Arguments<'q>: IntoArguments<'q, DB>,
{
    let mut stream = executor.fetch_many(sqlx::query(query));
    let mut rows = Vec::new();
    let mut done = DB::QueryResult::default();
    let mut truncated = false;
    let mut single_statement: Option<bool> = None;
    while let Some(item) = stream.try_next().await? {
        match item {
            Either::Left(result) => done.extend(Some(result)),
            Either::Right(row) => {
                if max_rows.is_some_and(|max| rows.len() >= max) {
                    truncated = true;
                    let single =
                        *single_statement.get_or_insert_with(|| split_statements(query).len() <= 1);
                    if single {
                        break;
                    }
                } else {
                    rows.push(row);
                }
            }
        }
    }
    Ok((rows, done, truncated))
}

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

/// `query` without its leading whitespace and `--` / `/* */` comments.
pub(crate) fn skip_leading_comments(query: &str) -> &str {
    let mut rest = query;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            return rest;
        }
    }
}

/// First keyword of `query` (after whitespace and comments), upper-cased.
pub(crate) fn leading_keyword(query: &str) -> String {
    skip_leading_comments(query)
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// Whether `query` starts (after whitespace and comments) with a
/// row-modifying keyword. `WITH ... DELETE/UPDATE` is deliberately not
/// recognised: a CTE is far more often a SELECT, and an empty SELECT would
/// otherwise report a stale count.
pub(crate) fn is_dml(query: &str) -> bool {
    matches!(
        leading_keyword(query).as_str(),
        "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "MERGE"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_dml_skips_leading_comments() {
        assert!(is_dml("  -- note\n/* c */ delete from t"));
        assert!(is_dml("INSERT INTO t VALUES (1)"));
        assert!(is_dml("update t set a = 1"));
        assert!(is_dml("REPLACE INTO t VALUES (1)"));
        assert!(!is_dml("SELECT 1"));
        assert!(!is_dml("COMMIT"));
        assert!(!is_dml("-- DELETE FROM t\nSELECT 1"));
        assert!(!is_dml("/* unterminated DELETE"));
        assert!(!is_dml(""));
    }

    #[test]
    fn is_dml_recognises_merge() {
        assert!(is_dml(
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE;"
        ));
        assert!(is_dml(
            "/* upsert */ merge t using s on 1 = 1 when matched then delete;"
        ));
    }

    #[test]
    fn leading_keyword_is_uppercased() {
        assert_eq!(leading_keyword("\n -- x\n begin tran"), "BEGIN");
        assert_eq!(leading_keyword("@x"), "");
    }
}
