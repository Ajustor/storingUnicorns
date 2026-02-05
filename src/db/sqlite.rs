use anyhow::Result;
use sqlx::{sqlite::SqliteRow, Column as SqlxColumn, Row, SqlitePool, TypeInfo};

use crate::models::{Column, QueryResult, SchemaInfo};

use super::utils::build_update_clauses;

/// Connect to SQLite
pub async fn connect(conn_str: &str) -> Result<SqlitePool> {
    let pool = SqlitePool::connect(conn_str).await?;
    Ok(pool)
}

/// Execute a query on SQLite
pub async fn execute_query(pool: &SqlitePool, query: &str) -> Result<QueryResult> {
    let rows: Vec<SqliteRow> = sqlx::query(query).fetch_all(pool).await?;

    if rows.is_empty() {
        return Ok(QueryResult::default());
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

    Ok(QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        execution_time_ms: 0,
    })
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
        .unwrap_or_else(|_| "NULL".to_string())
}
