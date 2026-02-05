use anyhow::Result;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::models::{Column, ConnectionConfig, QueryResult, SchemaInfo};

use super::utils::{build_update_clauses, group_tables_by_schema};

/// SQL Server client type alias
pub type SqlServerClient = Arc<Mutex<Client<Compat<TcpStream>>>>;

/// Connect to SQL Server
pub async fn connect(config: &ConnectionConfig) -> Result<SqlServerClient> {
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
    Ok(Arc::new(Mutex::new(client)))
}

/// Execute a query on SQL Server
pub async fn execute_query(client: &SqlServerClient, query: &str) -> Result<QueryResult> {
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
pub async fn get_tables_by_schema(client: &SqlServerClient) -> Result<Vec<SchemaInfo>> {
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

/// Update a row in SQL Server
pub async fn update_row(
    client: &SqlServerClient,
    table_name: &str,
    columns: &[Column],
    original_values: &[String],
    new_values: &[String],
) -> Result<u64> {
    let (set_clause, where_clause) =
        build_update_clauses(columns, original_values, new_values, '[', ']');

    if set_clause.is_empty() {
        return Ok(0); // No changes
    }

    let query = format!(
        "UPDATE {} SET {} WHERE {}",
        table_name, set_clause, where_clause
    );

    tracing::debug!("SQL Server UPDATE query: {}", query);
    let mut client = client.lock().await;
    let result = client.execute(&query, &[]).await?;
    Ok(result.total())
}

/// Insert a new row into a SQL Server table
pub async fn insert_row(
    client: &SqlServerClient,
    table_name: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
) -> Result<u64> {
    let (columns_part, values_part) =
        super::utils::build_insert_parts(columns, values, system_columns, '[', ']');

    if columns_part.is_empty() {
        return Err(anyhow::anyhow!("No columns to insert"));
    }

    let query = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table_name, columns_part, values_part
    );

    tracing::debug!("SQL Server INSERT query: {}", query);
    let mut client = client.lock().await;
    let result = client.execute(&query, &[]).await?;
    Ok(result.total())
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    // Parse schema.table or just table
    let (schema, table) = if table_name.contains('.') {
        let parts: Vec<&str> = table_name.split('.').collect();
        (
            parts[0].trim_matches(|c| c == '[' || c == ']'),
            parts[1].trim_matches(|c| c == '[' || c == ']'),
        )
    } else {
        ("dbo", table_name.trim_matches(|c| c == '[' || c == ']'))
    };

    let query = format!(
        "SELECT COLUMN_NAME, IS_NULLABLE 
         FROM INFORMATION_SCHEMA.COLUMNS 
         WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}'",
        schema, table
    );

    let mut client = client.lock().await;
    let stream = client.simple_query(&query).await?;
    let rows = stream.into_first_result().await?;

    let mut result = std::collections::HashMap::new();
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let nullable: &str = row.try_get(1)?.unwrap_or("YES");
        result.insert(name.to_string(), nullable == "YES");
    }

    Ok(result)
}

/// Get primary key columns for a table
pub async fn get_primary_keys(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<String>> {
    // Parse schema.table or just table
    let (schema, table) = if table_name.contains('.') {
        let parts: Vec<&str> = table_name.split('.').collect();
        (
            parts[0].trim_matches(|c| c == '[' || c == ']'),
            parts[1].trim_matches(|c| c == '[' || c == ']'),
        )
    } else {
        ("dbo", table_name.trim_matches(|c| c == '[' || c == ']'))
    };

    let query = format!(
        "SELECT COLUMN_NAME 
         FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE 
         WHERE OBJECTPROPERTY(OBJECT_ID(CONSTRAINT_SCHEMA + '.' + QUOTENAME(CONSTRAINT_NAME)), 'IsPrimaryKey') = 1
         AND TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}'
         ORDER BY ORDINAL_POSITION",
        schema, table
    );

    let mut client = client.lock().await;
    let stream = client.simple_query(&query).await?;
    let rows = stream.into_first_result().await?;

    let mut primary_keys = Vec::new();
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        primary_keys.push(name.to_string());
    }

    Ok(primary_keys)
}

/// Test the connection
pub async fn test(client: &SqlServerClient) -> Result<()> {
    let mut client = client.lock().await;
    client.simple_query("SELECT 1").await?;
    Ok(())
}

fn get_value(row: &tiberius::Row, index: usize) -> String {
    row.try_get::<&str, _>(index)
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .or_else(|| {
            row.try_get::<i32, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .or_else(|| {
            row.try_get::<i64, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .or_else(|| {
            row.try_get::<f64, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .or_else(|| {
            row.try_get::<bool, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .unwrap_or_else(|| "NULL".to_string())
}
