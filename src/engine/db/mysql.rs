use anyhow::Result;
use sqlx::{mysql::MySqlRow, Column as SqlxColumn, MySqlPool, Row, TypeInfo};

use crate::engine::models::{Column, ForeignKeyInfo, IndexInfo, QueryResult, SchemaInfo};

use super::utils::{
    fetch_rows_and_result, group_foreign_keys, group_indexes, group_tables_by_schema,
    split_qualified, TxConnection,
};

/// Connect to MySQL
pub async fn connect(conn_str: &str) -> Result<MySqlPool> {
    let pool = MySqlPool::connect(conn_str).await?;
    Ok(pool)
}

/// Convert fetched rows into a `QueryResult`.
///
/// When the statement returned rows (SELECT, `... RETURNING`), `rows_affected`
/// is the number of rows returned; otherwise it is `affected`, the count
/// reported by the database (UPDATE/DELETE/INSERT).
fn rows_to_result(rows: &[MySqlRow], affected: u64) -> QueryResult {
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
/// count from the same stream. User SQL goes through the text protocol: the
/// prepared-statement protocol rejects `START TRANSACTION`, `LOCK TABLES`,
/// several statements in one string, etc.
async fn run_statement<'c, E>(
    executor: E,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    let (rows, done, truncated) = fetch_rows_and_result(executor, query, max_rows, false).await?;
    Ok(QueryResult {
        truncated,
        ..rows_to_result(&rows, done.rows_affected())
    })
}

/// Execute a query on MySQL, keeping at most `max_rows` rows (all when
/// `None`); `truncated` is set when more rows were available.
pub async fn execute_query_limited(
    pool: &MySqlPool,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    run_statement(pool, query, max_rows).await
}

/// Execute a sequence of statements as one transaction on a single dedicated
/// connection. Statements include the user's `BEGIN`/`COMMIT`/`ROLLBACK`.
/// On any error the transaction is rolled back and the error is returned.
pub async fn execute_transaction(pool: &MySqlPool, statements: &[String]) -> Result<QueryResult> {
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
pub async fn get_tables_by_schema(pool: &MySqlPool) -> Result<Vec<SchemaInfo>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT CAST(TABLE_SCHEMA AS CHAR), CAST(TABLE_NAME AS CHAR)
         FROM information_schema.tables
         WHERE TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
         ORDER BY TABLE_SCHEMA, TABLE_NAME",
    )
    .fetch_all(pool)
    .await?;
    Ok(group_tables_by_schema(rows))
}

/// Insert a new row into a MySQL table
pub async fn insert_row(
    pool: &MySqlPool,
    table_name: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
) -> Result<u64> {
    let (columns_part, values_part) =
        super::utils::build_insert_parts(columns, values, system_columns, '`', '`');

    if columns_part.is_empty() {
        return Err(anyhow::anyhow!("No columns to insert"));
    }

    let query = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name, columns_part, values_part
    );

    tracing::debug!("MySQL INSERT query: {}", query);
    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// `(schema, table)` of a possibly qualified/quoted name; the schema is
/// empty when not given, which the queries turn into `DATABASE()`.
fn split_table(table_name: &str) -> (String, String) {
    let (schema, table) = split_qualified(table_name);
    (schema.unwrap_or_default(), table)
}

/// Columns of a table as `(name, type, nullable, primary key)`, in order
/// (current database when the name is not qualified). `information_schema`
/// columns are cast to CHAR: MySQL 8 reports them with a binary collation,
/// which would not decode as text.
async fn column_rows(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<Vec<(String, String, bool, bool)>> {
    let (schema, table) = split_table(table_name);
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR),
                CAST(IS_NULLABLE AS CHAR), CAST(COLUMN_KEY AS CHAR)
         FROM information_schema.COLUMNS
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
         ORDER BY ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, ty, nullable, key)| (name, ty, nullable == "YES", key == "PRI"))
        .collect())
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, _, nullable, _)| (name, nullable))
        .collect())
}

/// Get primary key columns for a table, in key order
pub async fn get_primary_keys(pool: &MySqlPool, table_name: &str) -> Result<Vec<String>> {
    let (schema, table) = split_table(table_name);
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT CAST(COLUMN_NAME AS CHAR)
         FROM information_schema.KEY_COLUMN_USAGE
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
           AND CONSTRAINT_NAME = 'PRIMARY'
         ORDER BY ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(name,)| name).collect())
}

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(pool: &MySqlPool, table_name: &str) -> Result<Vec<String>> {
    Ok(column_rows(pool, table_name)
        .await?
        .into_iter()
        .map(|(name, ..)| name)
        .collect())
}

/// Get full column details for a table (for schema modification). The type
/// is the full `COLUMN_TYPE` (`varchar(50)`, `int unsigned`).
pub async fn get_table_column_details(
    pool: &MySqlPool,
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

/// Get the indexes of a table from `information_schema.STATISTICS`
/// (current database when the name is not qualified).
pub async fn get_indexes(pool: &MySqlPool, table_name: &str) -> Result<Vec<IndexInfo>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT CAST(INDEX_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), NON_UNIQUE
         FROM information_schema.STATISTICS
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
         ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(group_indexes(rows.into_iter().map(
        |(name, column, non_unique)| {
            let primary = name == "PRIMARY";
            // NULL column: a functional key part.
            let column = column.unwrap_or_else(|| "<expression>".to_string());
            (name, column, non_unique == 0, primary)
        },
    )))
}

/// Get the foreign keys of a table from `information_schema.KEY_COLUMN_USAGE`
/// (current database when the name is not qualified). The referenced table
/// is qualified with its schema when it lives in another one.
pub async fn get_foreign_keys(pool: &MySqlPool, table_name: &str) -> Result<Vec<ForeignKeyInfo>> {
    let (schema, table) = split_table(table_name);

    let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT CAST(CONSTRAINT_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR),
                CAST(TABLE_SCHEMA AS CHAR), CAST(REFERENCED_TABLE_SCHEMA AS CHAR),
                CAST(REFERENCED_TABLE_NAME AS CHAR), CAST(REFERENCED_COLUMN_NAME AS CHAR)
         FROM information_schema.KEY_COLUMN_USAGE
         WHERE TABLE_SCHEMA = COALESCE(NULLIF(?, ''), DATABASE()) AND TABLE_NAME = ?
           AND REFERENCED_TABLE_NAME IS NOT NULL
         ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(group_foreign_keys(rows.into_iter().map(
        |(name, column, own_schema, ref_schema, ref_table, ref_column)| {
            let ref_table = if ref_schema == own_schema {
                ref_table
            } else {
                format!("{ref_schema}.{ref_table}")
            };
            (name, column, ref_table, ref_column)
        },
    )))
}

/// Test the connection
pub async fn test(pool: &MySqlPool) -> Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

/// Close the connection
pub async fn close(pool: MySqlPool) {
    pool.close().await;
}

/// Text of a value. Rows come from the text protocol (`run_statement`), so
/// every non-NULL value already is its textual form, exactly as MySQL prints
/// it (DATETIME without offset, DECIMAL scale kept, JSON as stored, BIGINT
/// UNSIGNED in full); BLOB-like values are shown lossily.
fn get_value(row: &MySqlRow, index: usize) -> String {
    use sqlx::ValueRef;
    match row.try_get_raw(index) {
        Ok(raw) if !raw.is_null() => {}
        _ => return "NULL".to_string(),
    }
    row.try_get_unchecked::<&[u8], _>(index)
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_else(|_| "NULL".to_string())
}
