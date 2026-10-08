use anyhow::Result;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::engine::models::{Column, ConnectionConfig, QueryResult, SchemaInfo};

use crate::engine::sql::statements::split_statements;

use super::utils::{
    build_update_clauses, group_tables_by_schema, is_dml, leading_keyword, skip_leading_comments,
};

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

/// Convert fetched rows into a `QueryResult`.
fn rows_to_result(rows: &[tiberius::Row]) -> QueryResult {
    if rows.is_empty() {
        return QueryResult::default();
    }

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

    QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        ..QueryResult::default()
    }
}

/// Whether `stmt` contains the word `OUTPUT` outside string literals, quoted
/// identifiers and comments. `INSERT/UPDATE/DELETE/MERGE ... OUTPUT` returns
/// rows, so it must keep the row-fetching path.
fn has_output_clause(stmt: &str) -> bool {
    let is_word_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$');
    let mut rest = stmt;
    while let Some(c) = rest.chars().next() {
        let quote_end = match c {
            '\'' => Some('\''),
            '"' => Some('"'),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(end) = quote_end {
            rest = rest[1..].split_once(end).map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else if is_word_char(c) {
            let end = rest.find(|ch| !is_word_char(ch)).unwrap_or(rest.len());
            if rest[..end].eq_ignore_ascii_case("OUTPUT") {
                return true;
            }
            rest = &rest[end..];
        } else {
            rest = &rest[c.len_utf8()..];
        }
    }
    false
}

/// Transaction control or variable handling: statements that return no rows.
fn is_rowless_control(stmt: &str) -> bool {
    let keyword = leading_keyword(stmt);
    let after = &skip_leading_comments(stmt)[keyword.len()..];
    match keyword.as_str() {
        "COMMIT" | "ROLLBACK" | "SAVE" | "DECLARE" => true,
        "BEGIN" => matches!(
            leading_keyword(after).as_str(),
            "TRAN" | "TRANSACTION" | "DISTRIBUTED"
        ),
        "SET" => after.trim_start().starts_with('@'),
        _ => false,
    }
}

/// Whether running `statements` can only report affected-row counts, never
/// rows: at least one DML statement without `OUTPUT`, the others transaction
/// control or variables. tiberius' `QueryStream` (`simple_query`) drops the
/// DONE row counts, so such batches go through `Client::execute`, which
/// reports them.
fn counts_only<S: AsRef<str>>(statements: &[S]) -> bool {
    let mut any_dml = false;
    for stmt in statements.iter().map(AsRef::as_ref) {
        if is_dml(stmt) && !has_output_clause(stmt) {
            any_dml = true;
        } else if !is_rowless_control(stmt) {
            return false;
        }
    }
    any_dml
}

/// Run `batch` with `Client::execute` and sum the affected-row counts. It goes
/// through `sp_executesql`: the whole text is still one batch, so variables
/// declared in it stay visible across its statements.
async fn execute_counting(
    client: &mut Client<Compat<TcpStream>>,
    batch: &str,
) -> Result<QueryResult> {
    let affected = client.execute(batch, &[]).await?.total();
    Ok(QueryResult {
        rows_affected: affected,
        ..QueryResult::default()
    })
}

/// Execute a query on SQL Server. DML that returns no rows goes through
/// `Client::execute` so that `rows_affected` is the real count.
pub async fn execute_query(client: &SqlServerClient, query: &str) -> Result<QueryResult> {
    let statements: Vec<String> = split_statements(query)
        .into_iter()
        .map(|(_, _, stmt)| stmt)
        .collect();
    let mut client = client.lock().await;
    if counts_only(&statements) {
        return execute_counting(&mut client, query).await;
    }
    let stream = client.simple_query(query).await?;
    let rows = stream.into_first_result().await?;
    Ok(rows_to_result(&rows))
}

/// Join transaction statements into a single T-SQL batch. Running the whole
/// block as ONE batch keeps batch-scoped constructs (e.g. `DECLARE @var`)
/// visible across all statements — running them as separate batches would drop
/// the variables between statements.
fn build_tsql_batch(statements: &[String]) -> String {
    statements.join(";\n")
}

/// Execute a transaction block as one T-SQL batch. The single tiberius client
/// is locked for the whole block (atomicity, no interleaving). Statements
/// include the user's `BEGIN`/`COMMIT`/`ROLLBACK`. If the batch fails, any
/// transaction it left open is rolled back and the error is returned.
pub async fn execute_transaction(
    client: &SqlServerClient,
    statements: &[String],
) -> Result<QueryResult> {
    let batch = build_tsql_batch(statements);
    let mut client = client.lock().await;

    let outcome = async {
        if counts_only(statements) {
            return execute_counting(&mut client, &batch).await;
        }
        let stream = client.simple_query(&batch).await?;
        let results = stream.into_results().await?;
        // Return the last result set that produced rows (typically the final
        // SELECT); otherwise an empty result.
        let rows = results
            .into_iter()
            .rev()
            .find(|r| !r.is_empty())
            .unwrap_or_default();
        Ok(rows_to_result(&rows))
    }
    .await;

    match outcome {
        Ok(result) => Ok(result),
        Err(e) => {
            // The failed batch may have left a transaction open on the shared
            // client; roll it back so later queries aren't poisoned.
            let _ = client.simple_query("IF @@TRANCOUNT > 0 ROLLBACK").await;
            Err(e)
        }
    }
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
pub async fn get_primary_keys(client: &SqlServerClient, table_name: &str) -> Result<Vec<String>> {
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

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(client: &SqlServerClient, table_name: &str) -> Result<Vec<String>> {
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
         FROM INFORMATION_SCHEMA.COLUMNS 
         WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = '{}'
         ORDER BY ORDINAL_POSITION",
        schema, table
    );

    let mut client = client.lock().await;
    let stream = client.simple_query(&query).await?;
    let rows = stream.into_first_result().await?;

    let mut columns = Vec::new();
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        columns.push(name.to_string());
    }

    Ok(columns)
}

/// Get full column details for a table (for schema modification)
pub async fn get_table_column_details(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<crate::engine::models::Column>> {
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

    // Get column info
    let query = format!(
        "SELECT c.COLUMN_NAME, c.DATA_TYPE, c.IS_NULLABLE,
                CASE WHEN pk.COLUMN_NAME IS NOT NULL THEN 1 ELSE 0 END AS is_pk
         FROM INFORMATION_SCHEMA.COLUMNS c
         LEFT JOIN (
             SELECT ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
             FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
             WHERE OBJECTPROPERTY(OBJECT_ID(ku.CONSTRAINT_SCHEMA + '.' + QUOTENAME(ku.CONSTRAINT_NAME)), 'IsPrimaryKey') = 1
         ) pk ON c.TABLE_SCHEMA = pk.TABLE_SCHEMA AND c.TABLE_NAME = pk.TABLE_NAME AND c.COLUMN_NAME = pk.COLUMN_NAME
         WHERE c.TABLE_SCHEMA = '{}' AND c.TABLE_NAME = '{}'
         ORDER BY c.ORDINAL_POSITION",
        schema, table
    );

    let mut client = client.lock().await;
    let stream = client.simple_query(&query).await?;
    let rows = stream.into_first_result().await?;

    let mut columns = Vec::new();
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let type_name: &str = row.try_get(1)?.unwrap_or("");
        let nullable: &str = row.try_get(2)?.unwrap_or("YES");
        let is_pk: i32 = row.try_get(3)?.unwrap_or(0);

        columns.push(crate::engine::models::Column {
            name: name.to_string(),
            type_name: type_name.to_string(),
            nullable: nullable == "YES",
            is_primary_key: is_pk == 1,
        });
    }

    Ok(columns)
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
            row.try_get::<rust_decimal::Decimal, _>(index)
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
        .or_else(|| {
            row.try_get::<chrono::DateTime<chrono::Utc>, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_rfc3339())
        })
        .or_else(|| {
            row.try_get::<chrono::DateTime<chrono::FixedOffset>, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_rfc3339())
        })
        .or_else(|| {
            row.try_get::<chrono::NaiveDateTime, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .or_else(|| {
            row.try_get::<chrono::NaiveDate, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .or_else(|| {
            row.try_get::<chrono::NaiveTime, _>(index)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        })
        .or_else(|| {
            row.try_get::<&[u8], _>(index)
                .ok()
                .flatten()
                .map(|v| String::from_utf8_lossy(v).into_owned())
        })
        .unwrap_or_else(|| "NULL".to_string())
}

#[cfg(test)]
mod tests {
    use super::{build_tsql_batch, counts_only, has_output_clause, is_rowless_control};

    #[test]
    fn detects_output_clause() {
        assert!(has_output_clause(
            "INSERT INTO t (a) OUTPUT inserted.id VALUES (1)"
        ));
        assert!(has_output_clause("delete from t output deleted.*"));
        assert!(has_output_clause("UPDATE t SET a = 1 OUTPUT"));
        assert!(!has_output_clause("UPDATE t SET a = 'OUTPUT'"));
        assert!(!has_output_clause("UPDATE t SET a = 'it''s OUTPUT'"));
        assert!(!has_output_clause("UPDATE t SET [output] = 1"));
        assert!(!has_output_clause("UPDATE t SET \"Output\" = 1"));
        assert!(!has_output_clause("UPDATE t SET a = @output"));
        assert!(!has_output_clause("UPDATE t SET outputs = 1 -- OUTPUT\n"));
        assert!(!has_output_clause("UPDATE t /* OUTPUT */ SET a = 1"));
    }

    #[test]
    fn rowless_control_statements() {
        assert!(is_rowless_control("BEGIN TRAN"));
        assert!(is_rowless_control("begin transaction"));
        assert!(is_rowless_control("COMMIT"));
        assert!(is_rowless_control("ROLLBACK TRANSACTION"));
        assert!(is_rowless_control("DECLARE @x INT = 5"));
        assert!(is_rowless_control("SET @x = 6"));
        assert!(!is_rowless_control("SET SHOWPLAN_ALL ON"));
        assert!(!is_rowless_control("BEGIN TRY"));
        assert!(!is_rowless_control("SELECT 1"));
    }

    #[test]
    fn counts_only_requires_dml_without_rows() {
        assert!(counts_only(&["UPDATE t SET a = 1"]));
        assert!(counts_only(&[
            "-- c\nMERGE t USING s ON 1 = 1 WHEN MATCHED THEN DELETE"
        ]));
        assert!(counts_only(&[
            "BEGIN TRAN",
            "DECLARE @x INT = 5",
            "DELETE FROM t WHERE a = @x",
            "COMMIT",
        ]));
        assert!(!counts_only(&[
            "INSERT INTO t OUTPUT inserted.* VALUES (1)"
        ]));
        assert!(!counts_only(&["UPDATE t SET a = 1", "SELECT * FROM t"]));
        assert!(!counts_only(&["BEGIN TRAN", "COMMIT"]));
        assert!(!counts_only(&["SELECT 1"]));
        assert!(!counts_only::<&str>(&[]));
    }

    #[test]
    fn declare_and_usage_stay_in_one_batch() {
        let stmts = [
            "BEGIN TRAN".to_string(),
            "DECLARE @x INT = 5".to_string(),
            "SELECT @x".to_string(),
            "COMMIT".to_string(),
        ];
        // The whole block must be a single batch string so that @x declared in
        // one statement stays in scope for the SELECT that uses it.
        assert_eq!(
            build_tsql_batch(&stmts),
            "BEGIN TRAN;\nDECLARE @x INT = 5;\nSELECT @x;\nCOMMIT"
        );
    }
}
