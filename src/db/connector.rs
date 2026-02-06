use anyhow::Result;
use sqlx::{MySqlPool, PgPool, SqlitePool};
use std::time::Instant;

use crate::models::{Column, ConnectionConfig, DatabaseType, QueryResult, SchemaInfo};

pub use super::sqlserver::SqlServerClient;
use super::{mysql, postgres, sqlite, sqlserver};

/// Unified database connection handle
pub enum DatabaseConnection {
    Postgres(PgPool),
    MySQL(MySqlPool),
    SQLite(SqlitePool),
    SQLServer(SqlServerClient),
}

impl DatabaseConnection {
    /// Connect to a database using the provided configuration
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        let conn_str = config.to_connection_string();

        match config.db_type {
            DatabaseType::Postgres => {
                let pool = postgres::connect(&conn_str).await?;
                Ok(DatabaseConnection::Postgres(pool))
            }
            DatabaseType::MySQL => {
                let pool = mysql::connect(&conn_str).await?;
                Ok(DatabaseConnection::MySQL(pool))
            }
            DatabaseType::SQLite => {
                let pool = sqlite::connect(&conn_str).await?;
                Ok(DatabaseConnection::SQLite(pool))
            }
            DatabaseType::SQLServer => {
                let client = sqlserver::connect(config).await?;
                Ok(DatabaseConnection::SQLServer(client))
            }
        }
    }

    /// Execute a query and return results
    pub async fn execute_query(&self, query: &str) -> Result<QueryResult> {
        let start = Instant::now();

        let result = match self {
            DatabaseConnection::Postgres(pool) => postgres::execute_query(pool, query).await?,
            DatabaseConnection::MySQL(pool) => mysql::execute_query(pool, query).await?,
            DatabaseConnection::SQLite(pool) => sqlite::execute_query(pool, query).await?,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::execute_query(client, query).await?
            }
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
            DatabaseConnection::Postgres(pool) => postgres::test(pool).await,
            DatabaseConnection::MySQL(pool) => mysql::test(pool).await,
            DatabaseConnection::SQLite(pool) => sqlite::test(pool).await,
            DatabaseConnection::SQLServer(client) => sqlserver::test(client).await,
        }
    }

    /// Get list of tables/schemas (legacy - returns flat list)
    #[allow(dead_code)]
    pub async fn get_tables(&self) -> Result<Vec<String>> {
        let schemas = self.get_tables_by_schema().await?;
        Ok(schemas
            .into_iter()
            .flat_map(|s| {
                s.tables.into_iter().map(move |t| {
                    if s.name.is_empty() {
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
            DatabaseConnection::Postgres(pool) => postgres::get_tables_by_schema(pool).await,
            DatabaseConnection::MySQL(pool) => mysql::get_tables_by_schema(pool).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_tables_by_schema(pool).await,
            DatabaseConnection::SQLServer(client) => sqlserver::get_tables_by_schema(client).await,
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
                postgres::update_row(pool, table_name, columns, original_values, new_values).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::update_row(pool, table_name, columns, original_values, new_values).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::update_row(pool, table_name, columns, original_values, new_values).await
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::update_row(client, table_name, columns, original_values, new_values)
                    .await
            }
        }
    }

    /// Insert a new row into the database
    /// Excludes system-generated columns (like auto-increment, timestamps)
    pub async fn insert_row(
        &self,
        table_name: &str,
        columns: &[Column],
        values: &[String],
        system_columns: &[usize],
    ) -> Result<u64> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::insert_row(pool, table_name, columns, values, system_columns).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::insert_row(pool, table_name, columns, values, system_columns).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::insert_row(pool, table_name, columns, values, system_columns).await
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::insert_row(client, table_name, columns, values, system_columns).await
            }
        }
    }

    /// Get column metadata (name, type, nullable) for a table
    /// Returns a map of column_name -> nullable
    pub async fn get_column_nullability(
        &self,
        table_name: &str,
    ) -> Result<std::collections::HashMap<String, bool>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_column_nullability(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::get_column_nullability(pool, table_name).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::get_column_nullability(pool, table_name).await
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_column_nullability(client, table_name).await
            }
        }
    }

    /// Get primary key columns for a table
    pub async fn get_primary_keys(&self, table_name: &str) -> Result<Vec<String>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_primary_keys(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => mysql::get_primary_keys(pool, table_name).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_primary_keys(pool, table_name).await,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_primary_keys(client, table_name).await
            }
        }
    }

    /// Get column names for a specific table (for autocompletion)
    #[allow(dead_code)]
    pub async fn get_table_columns(&self, table_name: &str) -> Result<Vec<String>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_table_columns(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => mysql::get_table_columns(pool, table_name).await,
            DatabaseConnection::SQLite(pool) => sqlite::get_table_columns(pool, table_name).await,
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_table_columns(client, table_name).await
            }
        }
    }

    /// Get full column metadata for a table (for schema modification)
    pub async fn get_table_column_details(
        &self,
        table_name: &str,
    ) -> Result<Vec<crate::models::Column>> {
        match self {
            DatabaseConnection::Postgres(pool) => {
                postgres::get_table_column_details(pool, table_name).await
            }
            DatabaseConnection::MySQL(pool) => {
                mysql::get_table_column_details(pool, table_name).await
            }
            DatabaseConnection::SQLite(pool) => {
                sqlite::get_table_column_details(pool, table_name).await
            }
            DatabaseConnection::SQLServer(client) => {
                sqlserver::get_table_column_details(client, table_name).await
            }
        }
    }

    /// Close the connection
    pub async fn close(self) {
        match self {
            DatabaseConnection::Postgres(pool) => postgres::close(pool).await,
            DatabaseConnection::MySQL(pool) => mysql::close(pool).await,
            DatabaseConnection::SQLite(pool) => sqlite::close(pool).await,
            DatabaseConnection::SQLServer(_) => {
                // Tiberius client is dropped automatically
            }
        }
    }
}
