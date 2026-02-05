use anyhow::Result;
use sqlx::{
    mysql::MySqlRow, postgres::PgRow, sqlite::SqliteRow, Column as SqlxColumn, MySqlPool, PgPool,
    Row, SqlitePool, TypeInfo,
};
use std::sync::Arc;
use std::time::Instant;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::models::{Column, ConnectionConfig, DatabaseType, QueryResult, SchemaInfo};

/// Unified database connection handle
pub enum DatabaseConnection {
    Postgres(PgPool),
    MySQL(MySqlPool),
    SQLite(SqlitePool),
    SQLServer(Arc<Mutex<Client<Compat<TcpStream>>>>),
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
            DatabaseType::SQLServer => {
                let mut tib_config = Config::new();
                tib_config.host(config.host.as_deref().unwrap_or("localhost"));
                tib_config.port(config.port.unwrap_or(1433));
                tib_config.database(&config.database);
                tib_config.authentication(AuthMethod::sql_server(
                    config.username.as_deref().unwrap_or("sa"),
                    config.password.as_deref().unwrap_or(""),
                ));
                tib_config.trust_cert();

                let tcp = TcpStream::connect(tib_config.get_addr()).await?;
                tcp.set_nodelay(true)?;
                let client = Client::connect(tib_config, tcp.compat_write()).await?;
                Ok(DatabaseConnection::SQLServer(Arc::new(Mutex::new(client))))
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
            DatabaseConnection::SQLServer(client) => execute_mssql_query(client, query).await?,
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
            DatabaseConnection::SQLServer(client) => {
                let mut client = client.lock().await;
                client.simple_query("SELECT 1").await?;
            }
        }
        Ok(())
    }

    /// Get list of tables/schemas (legacy - returns flat list)
    #[allow(dead_code)]
    pub async fn get_tables(&self) -> Result<Vec<String>> {
        let schemas = self.get_tables_by_schema().await?;
        Ok(schemas
            .into_iter()
            .flat_map(|s| {
                s.tables.into_iter().map(move |t| {
                    if s.name.is_empty()
                        || s.name == "public"
                        || s.name == "dbo"
                        || s.name == "main"
                    {
                        t
                    } else {
                        format!("{}.{}", s.name, t)
                    }
                })
            })
            .collect())
    }

    /// Get list of tables grouped by schema
    pub async fn get_tables_by_schema(&self) -> Result<Vec<SchemaInfo>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                let rows: Vec<(String, String)> = sqlx::query_as(
                    "SELECT table_schema, table_name FROM information_schema.tables 
                     WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
                     ORDER BY table_schema, table_name",
                )
                .fetch_all(pool)
                .await?;
                Ok(group_tables_by_schema(rows))
            }
            DatabaseConnection::MySQL(pool) => {
                let rows: Vec<(String, String)> = sqlx::query_as(
                    "SELECT TABLE_SCHEMA, TABLE_NAME FROM information_schema.tables 
                     WHERE TABLE_SCHEMA NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
                     ORDER BY TABLE_SCHEMA, TABLE_NAME",
                )
                .fetch_all(pool)
                .await?;
                Ok(group_tables_by_schema(rows))
            }
            DatabaseConnection::SQLite(pool) => {
                // SQLite doesn't have schemas, use "main" as the default schema
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
            DatabaseConnection::SQLServer(client) => {
                let mut client = client.lock().await;
                let stream = client
                    .simple_query(
                        "SELECT TABLE_SCHEMA, TABLE_NAME FROM INFORMATION_SCHEMA.TABLES 
                         WHERE TABLE_TYPE = 'BASE TABLE' 
                         ORDER BY TABLE_SCHEMA, TABLE_NAME",
                    )
                    .await?;
                let rows = stream.into_first_result().await?;
                let tuples: Vec<(String, String)> = rows
                    .iter()
                    .filter_map(|row| {
                        let schema = row.get::<&str, _>(0)?.to_string();
                        let table = row.get::<&str, _>(1)?.to_string();
                        Some((schema, table))
                    })
                    .collect();
                Ok(group_tables_by_schema(tuples))
            }
        }
    }

    /// Update a row in the database
    /// Uses the original values to build a WHERE clause and the new values for the SET clause
    pub async fn update_row(
        &self,
        table_name: &str,
        columns: &[Column],
        original_values: &[String],
        new_values: &[String],
    ) -> Result<u64> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                update_row_pg(pool, table_name, columns, original_values, new_values).await
            }
            DatabaseConnection::MySQL(pool) => {
                update_row_mysql(pool, table_name, columns, original_values, new_values).await
            }
            DatabaseConnection::SQLite(pool) => {
                update_row_sqlite(pool, table_name, columns, original_values, new_values).await
            }
            DatabaseConnection::SQLServer(client) => {
                update_row_mssql(client, table_name, columns, original_values, new_values).await
            }
        }
    }

    /// Close the connection
    pub async fn close(self) {
        match self {
            DatabaseConnection::Postgres(pool) => pool.close().await,
            DatabaseConnection::MySQL(pool) => pool.close().await,
            DatabaseConnection::SQLite(pool) => pool.close().await,
            DatabaseConnection::SQLServer(_) => {
                // Tiberius client is dropped automatically
            }
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
        .map(|row| (0..columns.len()).map(|i| get_pg_value(row, i)).collect())
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

async fn execute_mssql_query(
    client: &Arc<Mutex<Client<Compat<TcpStream>>>>,
    query: &str,
) -> Result<QueryResult> {
    let mut client = client.lock().await;
    let stream = client.simple_query(query).await?;
    let rows = stream.into_first_result().await?;

    if rows.is_empty() {
        return Ok(QueryResult::default());
    }

    // Get column info from first row
    let columns: Vec<Column> = rows[0]
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: format!("{:?}", c.column_type()),
        })
        .collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|i| {
                    // Try to get the value as different types
                    row.try_get::<&str, _>(i)
                        .ok()
                        .flatten()
                        .map(|s| s.to_string())
                        .or_else(|| {
                            row.try_get::<i32, _>(i)
                                .ok()
                                .flatten()
                                .map(|v| v.to_string())
                        })
                        .or_else(|| {
                            row.try_get::<i64, _>(i)
                                .ok()
                                .flatten()
                                .map(|v| v.to_string())
                        })
                        .or_else(|| {
                            row.try_get::<f64, _>(i)
                                .ok()
                                .flatten()
                                .map(|v| v.to_string())
                        })
                        .or_else(|| {
                            row.try_get::<bool, _>(i)
                                .ok()
                                .flatten()
                                .map(|v| v.to_string())
                        })
                        .unwrap_or_else(|| "NULL".to_string())
                })
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

fn group_tables_by_schema(rows: Vec<(String, String)>) -> Vec<SchemaInfo> {
    use std::collections::BTreeMap;

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
fn build_update_clauses(
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
    quote_char: char,
) -> (String, String) {
    // Build SET clause only for changed values
    let set_parts: Vec<String> = columns
        .iter()
        .zip(original_values.iter().zip(new_values.iter()))
        .filter(|(_, (orig, new))| orig != new)
        .map(|(col, (_, new))| {
            let escaped_value = new.replace('\'', "''");
            format!("{0}{1}{0} = '{2}'", quote_char, col.name, escaped_value)
        })
        .collect();

    // Build WHERE clause using all original values
    let where_parts: Vec<String> = columns
        .iter()
        .zip(original_values.iter())
        .map(|(col, val)| {
            if val == "NULL" {
                format!("{0}{1}{0} IS NULL", quote_char, col.name)
            } else {
                let escaped_value = val.replace('\'', "''");
                format!("{0}{1}{0} = '{2}'", quote_char, col.name, escaped_value)
            }
        })
        .collect();

    (set_parts.join(", "), where_parts.join(" AND "))
}

/// PostgreSQL row update
async fn update_row_pg(
    pool: &PgPool,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, where_clause) = build_update_clauses(columns, original_values, new_values, '"');
    
    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    );

    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// MySQL row update
async fn update_row_mysql(
    pool: &MySqlPool,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, where_clause) = build_update_clauses(columns, original_values, new_values, '`');
    
    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    );

    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// SQLite row update
async fn update_row_sqlite(
    pool: &SqlitePool,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, where_clause) = build_update_clauses(columns, original_values, new_values, '"');
    
    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    );

    let result = sqlx::query(&query).execute(pool).await?;
    Ok(result.rows_affected())
}

/// SQL Server row update
async fn update_row_mssql(
    client: &Arc<Mutex<Client<Compat<TcpStream>>>>,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, _where_clause) = build_update_clauses(columns, original_values, new_values, '[');
    
    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    // SQL Server uses [] for quoting - rebuild with proper brackets
    let set_parts: Vec<String> = columns
        .iter()
        .zip(original_values.iter().zip(new_values.iter()))
        .filter(|(_, (orig, new))| orig != new)
        .map(|(col, (_, new))| {
            let escaped_value = new.replace('\'', "''");
            format!("[{}] = '{}'", col.name, escaped_value)
        })
        .collect();

    let where_parts: Vec<String> = columns
        .iter()
        .zip(original_values.iter())
        .map(|(col, val)| {
            if val == "NULL" {
                format!("[{}] IS NULL", col.name)
            } else {
                let escaped_value = val.replace('\'', "''");
                format!("[{}] = '{}'", col.name, escaped_value)
            }
        })
        .collect();

    if set_parts.is_empty() {
        return Ok(0);
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name,
        set_parts.join(", "),
        where_parts.join(" AND ")
    );

    let mut client = client.lock().await;
    let result = client.execute(&query, &[]).await?;
    Ok(result.total())
}
