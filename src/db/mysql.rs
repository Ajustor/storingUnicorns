use anyhow::Result;
use sqlx::{mysql::MySqlRow, Column as SqlxColumn, MySqlPool, Row, TypeInfo};

use crate::models::{Column, QueryResult, SchemaInfo};

use super::utils::{build_update_clauses, group_tables_by_schema};

/// Connect to MySQL
pub async fn connect(conn_str: &str) -> Result<MySqlPool> {
    let pool = MySqlPool::connect(conn_str).await?;
    Ok(pool)
}

/// Execute a query on MySQL
pub async fn execute_query(pool: &MySqlPool, query: &str) -> Result<QueryResult> {
    let rows: Vec<MySqlRow> = sqlx::query(query).fetch_all(pool).await?;

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

/// Get tables grouped by schema
pub async fn get_tables_by_schema(pool: &MySqlPool) -> Result<Vec<SchemaInfo>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.tables 
         WHERE TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
         ORDER BY TABLE_SCHEMA, TABLE_NAME",
    )
    .fetch_all(pool)
    .await?;
    Ok(group_tables_by_schema(rows))
}

/// Update a row in MySQL
pub async fn update_row(
    pool: &MySqlPool,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, where_clause) =
        build_update_clauses(columns, original_values, new_values, '`', '`');

    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    );

    tracing::debug!("MySQL UPDATE query: {}", query);
    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
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

/// Get column nullability information for a table
pub async fn get_column_nullability(
    pool: &MySqlPool,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    // Parse schema.table or just table
    let (schema, table) = if table_name.contains('.') {
        let parts: Vec<&str> = table_name.split('.').collect();
        (parts[0].trim_matches('`'), parts[1].trim_matches('`'))
    } else {
        ("", table_name.trim_matches('`'))
    };

    let rows: Vec<(String, String)> = if schema.is_empty() {
        sqlx::query_as(
            "SELECT column_name, is_nullable 
             FROM information_schema.columns 
             WHERE table_name = ?",
        )
        .bind(table)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT column_name, is_nullable 
             FROM information_schema.columns 
             WHERE table_schema = ? AND table_name = ?",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|(name, nullable)| (name, nullable == "YES"))
        .collect())
}

/// Get primary key columns for a table
pub async fn get_primary_keys(pool: &MySqlPool, table_name: &str) -> Result<Vec<String>> {
    // Parse schema.table or just table
    let (schema, table) = if table_name.contains('.') {
        let parts: Vec<&str> = table_name.split('.').collect();
        (parts[0].trim_matches('`'), parts[1].trim_matches('`'))
    } else {
        ("", table_name.trim_matches('`'))
    };

    let rows: Vec<(String,)> = if schema.is_empty() {
        sqlx::query_as(
            "SELECT column_name 
             FROM information_schema.key_column_usage 
             WHERE table_name = ? AND constraint_name = 'PRIMARY'
             ORDER BY ordinal_position",
        )
        .bind(table)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT column_name 
             FROM information_schema.key_column_usage 
             WHERE table_schema = ? AND table_name = ? AND constraint_name = 'PRIMARY'
             ORDER BY ordinal_position",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(pool)
        .await?
    };

    Ok(rows.into_iter().map(|(name,)| name).collect())
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

fn get_value(row: &MySqlRow, index: usize) -> String {
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .unwrap_or_else(|_| "NULL".to_string())
}
