use std::collections::BTreeMap;

use futures_util::TryStreamExt;
use sqlx::pool::PoolConnection;
use sqlx::{Database, Either, Executor, IntoArguments};

use crate::engine::models::{Column, ForeignKeyInfo, IndexInfo, SchemaInfo};
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
/// Only the rows of the first statement that returns rows are kept (like
/// SQL Server's first result set); affected-row counts are summed over all.
///
/// `prepared` selects the prepared-statement protocol; otherwise the text
/// (simple query) protocol is used, which MySQL needs for statements it does
/// not support as prepared statements (`START TRANSACTION`, `LOCK TABLES`…)
/// and for several statements in one string. Prepared statements are not
/// cached: a cached statement keeps the column names it was prepared with,
/// which go stale after an `ALTER TABLE … RENAME COLUMN`.
pub async fn fetch_rows_and_result<'c, DB, E>(
    executor: E,
    query: &str,
    max_rows: Option<usize>,
    prepared: bool,
) -> sqlx::Result<(Vec<DB::Row>, DB::QueryResult, bool)>
where
    DB: Database + sqlx::database::HasStatementCache,
    E: Executor<'c, Database = DB>,
    for<'q> DB::Arguments<'q>: IntoArguments<'q, DB>,
{
    let mut stream = if prepared {
        executor.fetch_many(sqlx::query(query).persistent(false))
    } else {
        executor.fetch_many(query)
    };
    let mut rows = Vec::new();
    let mut done = DB::QueryResult::default();
    let mut truncated = false;
    let mut single_statement: Option<bool> = None;
    // Set once the first statement that returned rows has finished: rows of
    // later statements (possibly another shape) are discarded.
    let mut first_result_done = false;
    while let Some(item) = stream.try_next().await? {
        match item {
            Either::Left(result) => {
                first_result_done |= !rows.is_empty();
                done.extend(Some(result));
            }
            Either::Right(_) if first_result_done => {}
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

/// A pooled connection dedicated to a transaction block run with raw
/// `BEGIN`/`COMMIT` statements.
///
/// sqlx returns a dropped `PoolConnection` to the pool after a mere ping: if
/// the block is interrupted (task aborted by a GUI cancel, error whose
/// `ROLLBACK` failed…) its transaction would stay open on a pooled connection,
/// the next query borrowing it would run inside that transaction (never
/// committed) and the locks it holds would block every other connection.
/// Dropping this guard therefore *closes* the connection, which makes the
/// database roll the transaction back. Only `release`, called once the block
/// has run to its `COMMIT`/`ROLLBACK`, gives the connection back to the pool.
pub struct TxConnection<DB: Database> {
    conn: Option<PoolConnection<DB>>,
}

impl<DB: Database> TxConnection<DB> {
    pub fn new(conn: PoolConnection<DB>) -> Self {
        Self { conn: Some(conn) }
    }

    /// The block completed: return the connection to the pool.
    pub fn release(mut self) {
        drop(self.conn.take());
    }
}

impl<DB: Database> std::ops::Deref for TxConnection<DB> {
    type Target = DB::Connection;

    fn deref(&self) -> &DB::Connection {
        self.conn.as_deref().expect("connection already released")
    }
}

impl<DB: Database> std::ops::DerefMut for TxConnection<DB> {
    fn deref_mut(&mut self) -> &mut DB::Connection {
        self.conn
            .as_deref_mut()
            .expect("connection already released")
    }
}

impl<DB: Database> Drop for TxConnection<DB> {
    fn drop(&mut self) {
        if let Some(mut conn) = self.conn.take() {
            // Closed (in a spawned task) instead of returned to the pool.
            conn.close_on_drop();
        }
    }
}

/// A possibly quoted, qualified table name as shown to the user:
/// `"main"."users"` → `main.users`.
pub fn display_qualified(name: &str) -> String {
    match split_qualified(name) {
        (Some(schema), table) => format!("{schema}.{table}"),
        (None, table) => table,
    }
}

/// Split a table name, bare (`t`), qualified (`s.t`, `db.s.t`) and/or quoted
/// with any dialect's quotes (`"s"."t"`, `` `s`.`t` ``, `[s].[t]`), into
/// `(schema, table)`, both unquoted. Doubled quotes inside a quoted part are
/// unescaped and dots inside quotes are kept. With more than two parts the
/// last two are used; an empty schema counts as none.
pub fn split_qualified(name: &str) -> (Option<String>, String) {
    let mut parts: Vec<String> = Vec::new();
    let mut chars = name.trim().chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let mut part = String::new();
        let close = match chars.peek() {
            Some('"') => Some('"'),
            Some('`') => Some('`'),
            Some('[') => Some(']'),
            _ => None,
        };
        if let Some(close) = close {
            chars.next();
            while let Some(c) = chars.next() {
                if c != close {
                    part.push(c);
                } else if chars.next_if_eq(&close).is_some() {
                    part.push(close);
                } else {
                    break;
                }
            }
            // Anything between the closing quote and the next dot is ignored.
            while chars.next_if(|&c| c != '.').is_some() {}
        } else {
            while let Some(c) = chars.next_if(|&c| c != '.') {
                part.push(c);
            }
            part.truncate(part.trim_end().len());
        }
        parts.push(part);
        if chars.next().is_none() {
            break;
        }
    }
    let table = parts.pop().unwrap_or_default();
    let schema = parts.pop().filter(|s| !s.is_empty());
    (schema, table)
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

/// Build indexes from one row per indexed column, `(index, column, unique,
/// primary)`, ordered so that the columns of an index are consecutive and
/// in key order.
pub fn group_indexes(
    rows: impl IntoIterator<Item = (String, String, bool, bool)>,
) -> Vec<IndexInfo> {
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for (name, column, unique, primary) in rows {
        match indexes.last_mut() {
            Some(last) if last.name == name => last.columns.push(column),
            _ => indexes.push(IndexInfo {
                name,
                columns: vec![column],
                unique,
                primary,
            }),
        }
    }
    indexes
}

/// Build foreign keys from one row per column pair, `(constraint, column,
/// referenced table, referenced column)`, ordered so that the columns of a
/// constraint are consecutive and in key order.
pub fn group_foreign_keys(
    rows: impl IntoIterator<Item = (String, String, String, String)>,
) -> Vec<ForeignKeyInfo> {
    let mut keys: Vec<ForeignKeyInfo> = Vec::new();
    for (name, column, ref_table, ref_column) in rows {
        match keys.last_mut() {
            Some(last) if last.name == name => {
                last.columns.push(column);
                last.ref_columns.push(ref_column);
            }
            _ => keys.push(ForeignKeyInfo {
                name,
                columns: vec![column],
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }
    keys
}

/// SQL literal for `value` in column `col`. For numeric types a number is
/// left unquoted and an empty value is NULL; anything else is a quoted
/// string, so a non-numeric value typed in a numeric column is rejected by
/// the database instead of being run as SQL.
fn sql_literal(col: &Column, value: &str) -> String {
    let value = if is_bit_type(&col.type_name) {
        map_bit_value(value)
    } else {
        value.to_string()
    };
    if is_numeric_type(&col.type_name) {
        let number = value.trim();
        if number.is_empty() {
            return "NULL".to_string();
        }
        let plain = number
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'));
        if plain && number.parse::<f64>().is_ok() {
            return number.to_string();
        }
    }
    format!("'{}'", value.replace('\'', "''"))
}

/// `"col" = <literal>`, or `"col" IS NULL` for a NULL value.
fn where_part(col: &Column, value: &str, quote_start: char, quote_end: char) -> String {
    let literal = if value == "NULL" {
        "NULL".to_string()
    } else {
        sql_literal(col, value)
    };
    if literal == "NULL" {
        format!("{quote_start}{}{quote_end} IS NULL", col.name)
    } else {
        format!("{quote_start}{}{quote_end} = {literal}", col.name)
    }
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
            let literal = if new == "NULL" {
                "NULL".to_string()
            } else {
                sql_literal(col, new)
            };
            format!("{quote_start}{}{quote_end} = {literal}", col.name)
        })
        .collect();

    (
        set_parts.join(", "),
        build_where_clause(columns, original_values, quote_start, quote_end),
    )
}

/// Check if a SQL type is numeric (should not be quoted)
fn is_numeric_type(type_name: &str) -> bool {
    let type_lower = type_name.to_lowercase();
    // Arrays (`integer[]`) and types whose name merely contains a numeric
    // word take a quoted literal.
    if type_lower.contains('[') || type_lower.contains("interval") || type_lower.contains("point") {
        return false;
    }
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
            val_parts.push(sql_literal(col, val));
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
        .map(|(col, val)| where_part(col, val, quote_start, quote_end))
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
    fn groups_consecutive_index_and_foreign_key_rows() {
        let s = |v: &str| v.to_string();
        let idx = group_indexes([
            (s("pk"), s("a"), true, true),
            (s("pk"), s("b"), true, true),
            (s("ix"), s("c"), false, false),
        ]);
        assert_eq!(idx.len(), 2);
        assert_eq!(idx[0].columns, ["a", "b"]);
        assert!(idx[0].primary && !idx[1].unique);
        let fks = group_foreign_keys([
            (s("fk1"), s("x"), s("p"), s("px")),
            (s("fk1"), s("y"), s("p"), s("py")),
            (s("fk2"), s("z"), s("q"), s("id")),
        ]);
        assert_eq!(fks.len(), 2);
        assert_eq!(fks[0].columns, ["x", "y"]);
        assert_eq!(fks[0].ref_columns, ["px", "py"]);
        assert_eq!(fks[1].ref_table, "q");
    }

    #[test]
    fn display_qualified_drops_quotes() {
        assert_eq!(display_qualified("\"main\".\"users\""), "main.users");
        assert_eq!(display_qualified("[dbo].[t]"), "dbo.t");
        assert_eq!(display_qualified("`t`"), "t");
        assert_eq!(display_qualified("s.t"), "s.t");
    }

    #[test]
    fn split_qualified_handles_every_quote_style() {
        let split = |n: &str| {
            let (schema, table) = split_qualified(n);
            (schema.as_deref().map(str::to_string), table)
        };
        let some = |s: &str| Some(s.to_string());
        assert_eq!(split("t"), (None, "t".into()));
        assert_eq!(split("  t "), (None, "t".into()));
        assert_eq!(split("s.t"), (some("s"), "t".into()));
        assert_eq!(split("\"main\".\"t\""), (some("main"), "t".into()));
        assert_eq!(split("`db`.`t`"), (some("db"), "t".into()));
        assert_eq!(split("[dbo].[t]"), (some("dbo"), "t".into()));
        assert_eq!(split("\"t\""), (None, "t".into()));
        assert_eq!(split("[My Table]"), (None, "My Table".into()));
        assert_eq!(split("\"a.b\".\"c.d\""), (some("a.b"), "c.d".into()));
        assert_eq!(
            split("\"we\"\"ird\".\"x\"\"y\""),
            (some("we\"ird"), "x\"y".into())
        );
        assert_eq!(split("`a``b`.`c`"), (some("a`b"), "c".into()));
        assert_eq!(split("[a]]b].[c]"), (some("a]b"), "c".into()));
        assert_eq!(split("db.[dbo].t"), (some("dbo"), "t".into()));
        assert_eq!(split("public.\"Mixed\""), (some("public"), "Mixed".into()));
        assert_eq!(split("\"\".t"), (None, "t".into()));
    }

    #[test]
    fn literals_per_column_type() {
        let col = |ty: &str| Column {
            name: "c".into(),
            type_name: ty.into(),
            nullable: true,
            is_primary_key: true,
        };
        assert_eq!(sql_literal(&col("int"), " 42 "), "42");
        assert_eq!(sql_literal(&col("numeric(10,2)"), "-1.5e3"), "-1.5e3");
        assert_eq!(sql_literal(&col("int"), ""), "NULL");
        assert_eq!(
            sql_literal(&col("int"), "1; DROP TABLE t"),
            "'1; DROP TABLE t'"
        );
        assert_eq!(sql_literal(&col("bit"), "true"), "1");
        assert_eq!(sql_literal(&col("text"), "it's"), "'it''s'");
        assert_eq!(sql_literal(&col("text"), ""), "''");
        let cols = [col("int"), col("text")];
        let (set, filter) = build_update_clauses(
            &cols,
            &["1".into(), "a".into()],
            &["".into(), "NULL".into()],
            '"',
            '"',
        );
        assert_eq!(set, "\"c\" = NULL, \"c\" = NULL");
        assert_eq!(filter, "\"c\" = 1 AND \"c\" = 'a'");
        assert_eq!(
            build_where_clause(&cols, &["NULL".into(), "x".into()], '[', ']'),
            "[c] IS NULL AND [c] = 'x'"
        );
    }

    #[test]
    fn numeric_detection_ignores_arrays_and_lookalikes() {
        assert!(is_numeric_type("integer"));
        assert!(is_numeric_type("numeric(10,2)"));
        assert!(!is_numeric_type("integer[]"));
        assert!(!is_numeric_type("interval"));
        assert!(!is_numeric_type("point"));
        assert!(!is_numeric_type("character varying(50)"));
    }

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
