use anyhow::Result;
use sqlx::{postgres::PgRow, Column as SqlxColumn, PgPool, Row, TypeInfo};

use crate::engine::models::{Column, ForeignKeyInfo, IndexInfo, QueryResult, SchemaInfo};

use super::utils::{fetch_rows_and_result, group_tables_by_schema, split_qualified, TxConnection};

/// Connect to PostgreSQL
pub async fn connect(conn_str: &str) -> Result<PgPool> {
    let pool = PgPool::connect(conn_str).await?;
    Ok(pool)
}

/// Convert fetched rows into a `QueryResult`.
///
/// When the statement returned rows (SELECT, `... RETURNING`), `rows_affected`
/// is the number of rows returned; otherwise it is `affected`, the count
/// reported by the database (UPDATE/DELETE/INSERT).
fn rows_to_result(rows: &[PgRow], affected: u64) -> QueryResult {
    if rows.is_empty() {
        return QueryResult {
            rows_affected: affected,
            ..QueryResult::default()
        };
    }

    let columns: Vec<Column> = rows[0]
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_info().name().to_string(),
            nullable: true,
            is_primary_key: false,
        })
        .collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| (0..columns.len()).map(|i| get_value(row, i)).collect())
        .collect();

    QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        ..QueryResult::default()
    }
}

/// Execute one statement exactly once, collecting its rows and affected-row
/// count from the same stream.
async fn run_statement<'c, E>(
    executor: E,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let (rows, done, truncated) = fetch_rows_and_result(executor, query, max_rows, true).await?;
    Ok(QueryResult {
        truncated,
        ..rows_to_result(&rows, done.rows_affected())
    })
}

/// Execute a query on PostgreSQL, keeping at most `max_rows` rows (all when
/// `None`); `truncated` is set when more rows were available.
pub async fn execute_query_limited(
    pool: &PgPool,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    run_statement(pool, query, max_rows).await
}

/// Execute a sequence of statements as one transaction on a single dedicated
/// connection. Statements include the user's `BEGIN`/`COMMIT`/`ROLLBACK`.
/// On any error the transaction is rolled back and the error is returned.
pub async fn execute_transaction(pool: &PgPool, statements: &[String]) -> Result<QueryResult> {
    // Closed instead of pooled if the block does not run to completion.
    let mut conn = TxConnection::new(pool.acquire().await?);
    // Prefer the last statement that returned rows; if none did, fall back to
    // the last one that changed rows, so trailing `COMMIT`/`ROLLBACK` don't
    // hide the DML count.
    let mut last_rows: Option<QueryResult> = None;
    let mut last_changed: Option<QueryResult> = None;

    for stmt in statements {
        match run_statement(&mut *conn, stmt, None).await {
            Ok(result) => {
                if !result.rows.is_empty() {
                    last_rows = Some(result);
                } else if result.rows_affected > 0 {
                    last_changed = Some(result);
                }
            }
            Err(e) => {
                // Roll back on the same connection before bubbling up. If
                // that fails the transaction may still be open: the guard
                // closes the connection instead of pooling it.
                if sqlx::query("ROLLBACK").execute(&mut *conn).await.is_ok() {
                    conn.release();
                }
                return Err(e);
            }
        }
    }

    conn.release();
    Ok(last_rows.or(last_changed).unwrap_or_default())
}

/// Get tables grouped by schema
pub async fn get_tables_by_schema(pool: &PgPool) -> Result<Vec<SchemaInfo>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_schema, table_name FROM information_schema.tables 
         WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
         ORDER BY table_schema, table_name",
    )
    .fetch_all(pool)
    .await?;
    Ok(group_tables_by_schema(rows))
}

/// Insert a new row into a PostgreSQL table
pub async fn insert_row(
    pool: &PgPool,
    table_name: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
) -> Result<u64> {
    let (columns_part, values_part) =
        super::utils::build_insert_parts(columns, values, system_columns, '"', '"');

    if columns_part.is_empty() {
        return Err(anyhow::anyhow!("No columns to insert"));
    }

    let query = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name, columns_part, values_part
    );

    tracing::debug!("PostgreSQL INSERT query: {}", query);
    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// `(schema, table)` of a possibly qualified/quoted name. `None` schema:
/// the queries use `current_schema()`.
fn split_table(table_name: &str) -> (Option<String>, String) {
    split_qualified(table_name)
}

/// Columns of a table as `(name, type, nullable, primary key)`, in order.
/// The type comes from `format_type` (`character varying(50)`, `integer[]`).
async fn column_rows(pool: &PgPool, table_name: &str) -> Result<Vec<(String, String, bool, bool)>> {
    let (schema, table) = split_table(table_name);
    let rows = sqlx::query_as(
        "SELECT a.attname::text,
                format_type(a.atttypid, a.atttypmod),
                NOT a.attnotnull,
                EXISTS (SELECT 1 FROM pg_index i
                        WHERE i.indrelid = t.oid AND i.indisprimary
                          AND a.attnum = ANY(i.indkey))
         FROM pg_attribute a
         JOIN pg_class t ON t.oid = a.attrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         WHERE n.nspname = COALESCE($1, current_schema()) AND t.relname = $2
           AND a.attnum > 0 AND NOT a.attisdropped
         ORDER BY a.attnum",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    pool: &PgPool,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, _, nullable, _)| (name, nullable))
        .collect())
}

/// Get primary key columns for a table, in key order
pub async fn get_primary_keys(pool: &PgPool, table_name: &str) -> Result<Vec<String>> {
    let (schema, table) = split_table(table_name);
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT a.attname::text
         FROM pg_index i
         JOIN pg_class t ON t.oid = i.indrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         CROSS JOIN LATERAL unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
         JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
         WHERE i.indisprimary
           AND n.nspname = COALESCE($1, current_schema()) AND t.relname = $2
         ORDER BY k.ord",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(name,)| name).collect())
}

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(pool: &PgPool, table_name: &str) -> Result<Vec<String>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, ..)| name)
        .collect())
}

/// Get full column details for a table (for schema modification)
pub async fn get_table_column_details(
    pool: &PgPool,
    table_name: &str,
) -> Result<Vec<crate::engine::models::Column>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(
            |(name, type_name, nullable, is_primary_key)| crate::engine::models::Column {
                name,
                type_name,
                nullable,
                is_primary_key,
            },
        )
        .collect())
}

/// Get the indexes of a table from `pg_index`. Expression columns are
/// rendered with `pg_get_indexdef`; INCLUDE columns are left out.
pub async fn get_indexes(pool: &PgPool, table_name: &str) -> Result<Vec<IndexInfo>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, bool, bool, Vec<String>)> = sqlx::query_as(
        "SELECT ic.relname::text,
                i.indisunique,
                i.indisprimary,
                ARRAY(
                    SELECT COALESCE(a.attname::text,
                                    pg_get_indexdef(i.indexrelid, k.ord::int, true))
                    FROM unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
                    LEFT JOIN pg_attribute a
                           ON a.attrelid = i.indrelid AND a.attnum = k.attnum
                    WHERE k.ord <= i.indnkeyatts
                    ORDER BY k.ord
                )
         FROM pg_index i
         JOIN pg_class ic ON ic.oid = i.indexrelid
         JOIN pg_class t ON t.oid = i.indrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         WHERE n.nspname = COALESCE($1, current_schema()) AND t.relname = $2
         ORDER BY i.indisprimary DESC, ic.relname",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(name, unique, primary, columns)| IndexInfo {
            name,
            columns,
            unique,
            primary,
        })
        .collect())
}

/// Get the foreign keys of a table from `pg_constraint` (`conkey`/`confkey`
/// keep the column pairing of multi-column keys). The referenced table is
/// qualified with its schema when it lives in another schema.
pub async fn get_foreign_keys(pool: &PgPool, table_name: &str) -> Result<Vec<ForeignKeyInfo>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, Vec<String>, String, Vec<String>)> = sqlx::query_as(
        "SELECT c.conname::text,
                ARRAY(
                    SELECT a.attname::text
                    FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord)
                    JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum
                    ORDER BY k.ord
                ),
                CASE WHEN rn.nspname = n.nspname THEN rt.relname::text
                     ELSE rn.nspname::text || '.' || rt.relname::text END,
                ARRAY(
                    SELECT a.attname::text
                    FROM unnest(c.confkey) WITH ORDINALITY AS k(attnum, ord)
                    JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum
                    ORDER BY k.ord
                )
         FROM pg_constraint c
         JOIN pg_class t ON t.oid = c.conrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         JOIN pg_class rt ON rt.oid = c.confrelid
         JOIN pg_namespace rn ON rn.oid = rt.relnamespace
         WHERE c.contype = 'f' AND n.nspname = COALESCE($1, current_schema())
           AND t.relname = $2
         ORDER BY c.conname",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(name, columns, ref_table, ref_columns)| ForeignKeyInfo {
            name,
            columns,
            ref_table,
            ref_columns,
        })
        .collect())
}

/// Test the connection
pub async fn test(pool: &PgPool) -> Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

/// Close the connection
pub async fn close(pool: PgPool) {
    pool.close().await;
}

/// `{a,b}` rendering of an array (NULL elements as `NULL`).
fn array_text<T: ToString>(items: Vec<Option<T>>) -> String {
    let items: Vec<String> = items
        .into_iter()
        .map(|v| v.map_or_else(|| "NULL".to_string(), |v| v.to_string()))
        .collect();
    format!("{{{}}}", items.join(","))
}

/// Postgres-style text of an interval (`1 year 2 mons 3 days 04:05:06`).
fn interval_text(i: &sqlx::postgres::types::PgInterval) -> String {
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n.abs() == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    let (years, months) = (i.months / 12, i.months % 12);
    if years != 0 {
        parts.push(plural(years.into(), "year"));
    }
    if months != 0 {
        parts.push(plural(months.into(), "mon"));
    }
    if i.days != 0 {
        parts.push(plural(i.days.into(), "day"));
    }
    if i.microseconds != 0 || parts.is_empty() {
        let sign = if i.microseconds < 0 { "-" } else { "" };
        let us = i.microseconds.unsigned_abs();
        let secs = us / 1_000_000;
        let mut time = format!(
            "{sign}{:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        );
        if !us.is_multiple_of(1_000_000) {
            time.push_str(format!(".{:06}", us % 1_000_000).trim_end_matches('0'));
        }
        parts.push(time);
    }
    parts.join(" ")
}

fn get_value(row: &PgRow, index: usize) -> String {
    use sqlx::ValueRef;
    let raw = match row.try_get_raw(index) {
        Ok(raw) if !raw.is_null() => raw,
        _ => return "NULL".to_string(),
    };
    let type_name = raw.type_info().name().to_string();
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i16, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f32, _>(index).map(|v| v.to_string()))
        .or_else(|_| {
            row.try_get::<rust_decimal::Decimal, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .or_else(|_| {
            row.try_get::<sqlx::types::Uuid, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<sqlx::types::JsonValue, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<chrono::DateTime<chrono::Utc>, _>(index)
                .map(|v| v.to_rfc3339())
        })
        .or_else(|_| {
            row.try_get::<chrono::DateTime<chrono::FixedOffset>, _>(index)
                .map(|v| v.to_rfc3339())
        })
        .or_else(|_| {
            row.try_get::<chrono::NaiveDateTime, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<chrono::NaiveDate, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<chrono::NaiveTime, _>(index)
                .map(|v| v.to_string())
        })
        .or_else(|_| {
            row.try_get::<sqlx::postgres::types::PgInterval, _>(index)
                .map(|v| interval_text(&v))
        })
        .or_else(|_| row.try_get::<Vec<Option<String>>, _>(index).map(array_text))
        .or_else(|_| row.try_get::<Vec<Option<i32>>, _>(index).map(array_text))
        .or_else(|_| row.try_get::<Vec<Option<i64>>, _>(index).map(array_text))
        .or_else(|_| row.try_get::<Vec<Option<i16>>, _>(index).map(array_text))
        .or_else(|_| row.try_get::<Vec<Option<f64>>, _>(index).map(array_text))
        .or_else(|_| row.try_get::<Vec<Option<bool>>, _>(index).map(array_text))
        .or_else(|_| {
            row.try_get::<Vec<Option<rust_decimal::Decimal>>, _>(index)
                .map(array_text)
        })
        .or_else(|_| {
            row.try_get::<Vec<Option<sqlx::types::Uuid>>, _>(index)
                .map(array_text)
        })
        .or_else(|_| {
            row.try_get::<Vec<u8>, _>(index)
                .map(|v| String::from_utf8_lossy(&v).into_owned())
        })
        // Not NULL but of a type without a decoder here (enum, domain...):
        // binary text-like payloads (enum labels) are shown as is.
        .or_else(|_| {
            row.try_get_unchecked::<&[u8], _>(index)
                .ok()
                .and_then(|b| std::str::from_utf8(b).ok())
                .filter(|s| !s.chars().any(char::is_control))
                .map(str::to_string)
                .ok_or(())
        })
        .unwrap_or_else(|_| format!("<{type_name}>"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::types::PgInterval;

    #[test]
    fn interval_text_matches_postgres_style() {
        let i = |months, days, microseconds| PgInterval {
            months,
            days,
            microseconds,
        };
        assert_eq!(interval_text(&i(0, 1, 0)), "1 day");
        assert_eq!(
            interval_text(&i(14, 3, 3_723_000_000)),
            "1 year 2 mons 3 days 01:02:03"
        );
        assert_eq!(interval_text(&i(0, 0, 0)), "00:00:00");
        assert_eq!(interval_text(&i(0, 0, -1_500_000)), "-00:00:01.5");
    }

    #[test]
    fn array_text_renders_nulls() {
        assert_eq!(array_text(vec![Some(1), None]), "{1,NULL}");
        assert_eq!(array_text::<i32>(vec![]), "{}");
    }
}
