use anyhow::Result;
use sqlx::{sqlite::SqliteRow, Column as SqlxColumn, Row, SqlitePool, TypeInfo};

use crate::models::{Column, QueryResult, SchemaInfo};

use super::utils::build_update_clauses;

/// Connect to SQLite
pub async fn connect(conn_str: &str) -> Result<SqlitePool> {
    let pool = SqlitePool::connect(conn_str).await?;
    Ok(pool)
}

/// Convert fetched rows into a `QueryResult`.
fn rows_to_result(rows: &[SqliteRow]) -> QueryResult {
    if rows.is_empty() {
        return QueryResult::default();
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
        execution_time_ms: 0,
    }
}

/// Execute a query on SQLite
pub async fn execute_query(pool: &SqlitePool, query: &str) -> Result<QueryResult> {
    let rows: Vec<SqliteRow> = sqlx::query(query).fetch_all(pool).await?;
    Ok(rows_to_result(&rows))
}

/// Execute a sequence of statements as one transaction on a single dedicated
/// connection. The statements include the user's `BEGIN`/`COMMIT`/`ROLLBACK`.
/// On any error the transaction is rolled back and the error is returned.
/// Returns the last statement's result set (or an empty result).
pub async fn execute_transaction(pool: &SqlitePool, statements: &[String]) -> Result<QueryResult> {
    let mut conn = pool.acquire().await?;
    let mut last = QueryResult::default();

    for stmt in statements {
        match sqlx::query(stmt).fetch_all(&mut *conn).await {
            Ok(rows) => {
                if !rows.is_empty() {
                    last = rows_to_result(&rows);
                }
            }
            Err(e) => {
                // Best-effort rollback on the same connection before bubbling up.
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                return Err(e.into());
            }
        }
    }

    Ok(last)
}

/// Get tables grouped by schema (SQLite uses "main" as default schema)
pub async fn get_tables_by_schema(pool: &SqlitePool) -> Result<Vec<SchemaInfo>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master 
         WHERE type='table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    Ok(vec![SchemaInfo {
        name: "main".to_string(),
        tables: rows.into_iter().map(|r| r.0).collect(),
        expanded: false,
    }])
}

/// Update a row in SQLite
pub async fn update_row(
    pool: &SqlitePool,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, where_clause) =
        build_update_clauses(columns, original_values, new_values, '"', '"');

    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    );

    tracing::debug!("SQLite UPDATE query: {}", query);
    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Insert a new row into a SQLite table
pub async fn insert_row(
    pool: &SqlitePool,
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

    tracing::debug!("SQLite INSERT query: {}", query);
    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    // SQLite uses PRAGMA table_info to get column info
    let table = table_name.trim_matches('"').replace("main.", "");
    let query = format!("PRAGMA table_info('{}')", table);

    let rows: Vec<SqliteRow> = sqlx::query(&query).fetch_all(pool).await?;

    let mut result = std::collections::HashMap::new();
    for row in rows {
        let name: String = row.try_get(1).unwrap_or_default();
        let notnull: i32 = row.try_get(3).unwrap_or(0);
        result.insert(name, notnull == 0); // notnull=0 means nullable=true
    }

    Ok(result)
}

/// Get primary key columns for a table
pub async fn get_primary_keys(pool: &SqlitePool, table_name: &str) -> Result<Vec<String>> {
    // SQLite uses PRAGMA table_info - pk column (index 5) indicates primary key
    let table = table_name.trim_matches('"').replace("main.", "");
    let query = format!("PRAGMA table_info('{}')", table);

    let rows: Vec<SqliteRow> = sqlx::query(&query).fetch_all(pool).await?;

    let mut primary_keys = Vec::new();
    for row in rows {
        let name: String = row.try_get(1).unwrap_or_default();
        let pk: i32 = row.try_get(5).unwrap_or(0);
        if pk > 0 {
            primary_keys.push(name);
        }
    }

    Ok(primary_keys)
}

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(pool: &SqlitePool, table_name: &str) -> Result<Vec<String>> {
    let table = table_name.trim_matches('"').replace("main.", "");
    let query = format!("PRAGMA table_info('{}')", table);

    let rows: Vec<SqliteRow> = sqlx::query(&query).fetch_all(pool).await?;

    let mut columns = Vec::new();
    for row in rows {
        let name: String = row.try_get(1).unwrap_or_default();
        columns.push(name);
    }

    Ok(columns)
}

/// Get full column details for a table (for schema modification)
pub async fn get_table_column_details(
    pool: &SqlitePool,
    table_name: &str,
) -> Result<Vec<crate::models::Column>> {
    let table = table_name.trim_matches('"').replace("main.", "");
    let query = format!("PRAGMA table_info('{}')", table);

    let rows: Vec<SqliteRow> = sqlx::query(&query).fetch_all(pool).await?;

    let mut columns = Vec::new();
    for row in rows {
        let name: String = row.try_get(1).unwrap_or_default();
        let type_name: String = row.try_get(2).unwrap_or_default();
        let notnull: i32 = row.try_get(3).unwrap_or(0);
        let pk: i32 = row.try_get(5).unwrap_or(0);

        columns.push(crate::models::Column {
            name,
            type_name,
            nullable: notnull == 0,
            is_primary_key: pk > 0,
        });
    }

    Ok(columns)
}

/// Test the connection
pub async fn test(pool: &SqlitePool) -> Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

/// Close the connection
pub async fn close(pool: SqlitePool) {
    pool.close().await;
}

fn get_value(row: &SqliteRow, index: usize) -> String {
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .or_else(|_| {
            row.try_get::<chrono::DateTime<chrono::Utc>, _>(index)
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
            row.try_get::<Vec<u8>, _>(index)
                .map(|v| String::from_utf8_lossy(&v).into_owned())
        })
        .unwrap_or_else(|_| "NULL".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// A shared in-memory pool: max_connections(1) ensures every acquisition
    /// reuses the same connection (and therefore the same in-memory database).
    async fn mem_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (a INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn count(pool: &SqlitePool) -> i64 {
        let rows: Vec<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM t")
            .fetch_all(pool)
            .await
            .unwrap();
        rows[0].0
    }

    #[tokio::test]
    async fn transaction_commit_persists_rows() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (1)".to_string(),
            "INSERT INTO t VALUES (2)".to_string(),
            "COMMIT".to_string(),
        ];
        execute_transaction(&pool, &stmts).await.unwrap();
        assert_eq!(count(&pool).await, 2);
    }

    #[tokio::test]
    async fn transaction_rolls_back_on_error() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (1)".to_string(),
            "INSERT INTO nonexistent_table VALUES (2)".to_string(), // fails
            "COMMIT".to_string(),
        ];
        let result = execute_transaction(&pool, &stmts).await;
        assert!(result.is_err(), "expected the transaction to fail");
        assert_eq!(count(&pool).await, 0, "failed transaction must roll back");
    }

    #[tokio::test]
    async fn transaction_returns_last_select_result() {
        let pool = mem_pool().await;
        let stmts = [
            "BEGIN".to_string(),
            "INSERT INTO t VALUES (42)".to_string(),
            "SELECT a FROM t".to_string(),
            "COMMIT".to_string(),
        ];
        let result = execute_transaction(&pool, &stmts).await.unwrap();
        assert_eq!(result.rows, vec![vec!["42".to_string()]]);
    }
}
