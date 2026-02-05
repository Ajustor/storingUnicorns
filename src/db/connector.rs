use anyhow::Result;
use sqlx::{
    mysql::MySqlRow, postgres::PgRow, sqlite::SqliteRow, Column as SqlxColumn, MySqlPool, PgPool,
    Row, SqlitePool, TypeInfo,
};
use std::time::Instant;

use crate::models::{Column, ConnectionConfig, DatabaseType, QueryResult};

/// Unified database connection handle
pub enum DatabaseConnection {
    Postgres(PgPool),
    MySQL(MySqlPool),
    SQLite(SqlitePool),
}

impl DatabaseConnection {
    /// Connect to a database using the provided configuration
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        let conn_str = config.to_connection_string();

        match config.db_type {
            DatabaseType::Postgres => {
                let pool = PgPool::connect(&conn_str).await?;
                Ok(DatabaseConnection::Postgres(pool))
            }
            DatabaseType::MySQL => {
                let pool = MySqlPool::connect(&conn_str).await?;
                Ok(DatabaseConnection::MySQL(pool))
            }
            DatabaseType::SQLite => {
                let pool = SqlitePool::connect(&conn_str).await?;
                Ok(DatabaseConnection::SQLite(pool))
            }
        }
    }

    /// Execute a query and return results
    pub async fn execute_query(&self, query: &str) -> Result<QueryResult> {
        let start = Instant::now();

        let result = match self {
            DatabaseConnection::Postgres(pool) => execute_pg_query(pool, query).await?,
            DatabaseConnection::MySQL(pool) => execute_mysql_query(pool, query).await?,
            DatabaseConnection::SQLite(pool) => execute_sqlite_query(pool, query).await?,
        };

        Ok(QueryResult {
            execution_time_ms: start.elapsed().as_millis(),
            ..result
        })
    }

    /// Test the connection
    #[allow(dead_code)]
    pub async fn test(&self) -> Result<()> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
            DatabaseConnection::MySQL(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
            DatabaseConnection::SQLite(pool) => {
                sqlx::query("SELECT 1").execute(pool).await?;
            }
        }
        Ok(())
    }

    /// Get list of tables/schemas
    pub async fn get_tables(&self) -> Result<Vec<String>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                let rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT table_name FROM information_schema.tables 
                     WHERE table_schema = 'public' 
                     ORDER BY table_name",
                )
                .fetch_all(pool)
                .await?;
                Ok(rows.into_iter().map(|r| r.0).collect())
            }
            DatabaseConnection::MySQL(pool) => {
                let rows: Vec<(String,)> = sqlx::query_as("SHOW TABLES").fetch_all(pool).await?;
                Ok(rows.into_iter().map(|r| r.0).collect())
            }
            DatabaseConnection::SQLite(pool) => {
                let rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT name FROM sqlite_master 
                     WHERE type='table' AND name NOT LIKE 'sqlite_%'
                     ORDER BY name",
                )
                .fetch_all(pool)
                .await?;
                Ok(rows.into_iter().map(|r| r.0).collect())
            }
        }
    }

    /// Close the connection
    pub async fn close(self) {
        match self {
            DatabaseConnection::Postgres(pool) => pool.close().await,
            DatabaseConnection::MySQL(pool) => pool.close().await,
            DatabaseConnection::SQLite(pool) => pool.close().await,
        }
    }
}

async fn execute_pg_query(pool: &PgPool, query: &str) -> Result<QueryResult> {
    let rows: Vec<PgRow> = sqlx::query(query).fetch_all(pool).await?;

    if rows.is_empty() {
        return Ok(QueryResult::default());
    }

    let columns: Vec<Column> = rows[0]
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_info().name().to_string(),
        })
        .collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|i| get_pg_value(row, i))
                .collect()
        })
        .collect();

    Ok(QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        execution_time_ms: 0,
    })
}

async fn execute_mysql_query(pool: &MySqlPool, query: &str) -> Result<QueryResult> {
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
        })
        .collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|i| get_mysql_value(row, i))
                .collect()
        })
        .collect();

    Ok(QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        execution_time_ms: 0,
    })
}

async fn execute_sqlite_query(pool: &SqlitePool, query: &str) -> Result<QueryResult> {
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
        })
        .collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|i| get_sqlite_value(row, i))
                .collect()
        })
        .collect();

    Ok(QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        execution_time_ms: 0,
    })
}

fn get_pg_value(row: &PgRow, index: usize) -> String {
    // Try common types, fallback to debug representation
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .unwrap_or_else(|_| "NULL".to_string())
}

fn get_mysql_value(row: &MySqlRow, index: usize) -> String {
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .unwrap_or_else(|_| "NULL".to_string())
}

fn get_sqlite_value(row: &SqliteRow, index: usize) -> String {
    row.try_get::<String, _>(index)
        .or_else(|_| row.try_get::<i32, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<i64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<f64, _>(index).map(|v| v.to_string()))
        .or_else(|_| row.try_get::<bool, _>(index).map(|v| v.to_string()))
        .unwrap_or_else(|_| "NULL".to_string())
}
