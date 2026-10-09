use anyhow::Result;
use futures_util::TryStreamExt;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config, QueryItem, QueryStream};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::engine::models::{
    Column, ConnectionConfig, DatabaseType, ForeignKeyInfo, IndexInfo, QueryResult, SchemaInfo,
};

use crate::engine::sql::statements::split_statements;

use super::utils::{
    group_foreign_keys, group_indexes, group_tables_by_schema, is_dml, leading_keyword,
    skip_leading_comments, split_qualified,
};

/// A tiberius client over TCP.
pub type TdsClient = Client<Compat<TcpStream>>;

/// The connection's single tiberius client, shared by every operation and
/// used by one at a time.
///
/// An operation interrupted between sending its batch and reading the whole
/// response (task aborted by a GUI cancel, timeout…) leaves the client out of
/// step with the server: the next query would first wait for the abandoned
/// batch to finish (and a transaction it opened would then commit or stay
/// open). `lock` therefore marks the client dirty until the operation calls
/// `settle`; a client still dirty at the next `lock` is dropped (the server
/// rolls back the session's transaction) and replaced by a new connection
/// made from the saved configuration (Azure included, token re-acquired).
#[derive(Clone)]
pub struct SqlServerClient(Arc<Shared>);

struct Shared {
    config: ConnectionConfig,
    slot: Mutex<Slot>,
}

struct Slot {
    /// `None` after a failed reconnection.
    client: Option<TdsClient>,
    dirty: bool,
}

/// Exclusive use of the client, from `SqlServerClient::lock`.
pub struct ClientGuard<'a> {
    slot: MutexGuard<'a, Slot>,
}

impl SqlServerClient {
    pub fn new(config: ConnectionConfig, client: TdsClient) -> Self {
        Self(Arc::new(Shared {
            config,
            slot: Mutex::new(Slot {
                client: Some(client),
                dirty: false,
            }),
        }))
    }

    /// Wait for the client, reconnecting first if the previous operation was
    /// interrupted. The client is marked dirty until `ClientGuard::settle`.
    pub async fn lock(&self) -> Result<ClientGuard<'_>> {
        let mut slot = self.0.slot.lock().await;
        if slot.dirty || slot.client.is_none() {
            if slot.dirty {
                tracing::warn!("SQL Server: previous operation interrupted, reconnecting");
            }
            // Closing the old connection makes the server roll back its work.
            slot.client = None;
            slot.client = Some(reopen(&self.0.config).await?);
        }
        slot.dirty = true;
        Ok(ClientGuard { slot })
    }
}

/// A new client for `config`, with the authentication of its database type.
async fn reopen(config: &ConnectionConfig) -> Result<TdsClient> {
    match config.db_type {
        DatabaseType::Azure => super::azure::open(config).await,
        _ => open(config).await,
    }
}

impl ClientGuard<'_> {
    /// The operation is over: unless it failed with an I/O or protocol error
    /// (the response may be partly unread), the client is in step again.
    /// A server error (bad SQL, constraint…) is reported after the request
    /// was answered, so the client stays usable.
    pub fn settle<T>(mut self, outcome: &Result<T>) {
        self.slot.dirty = match outcome {
            Ok(_) => false,
            Err(e) => !is_server_error(e),
        };
    }
}

impl std::ops::Deref for ClientGuard<'_> {
    type Target = TdsClient;

    fn deref(&self) -> &TdsClient {
        self.slot.client.as_ref().expect("connected by lock")
    }
}

impl std::ops::DerefMut for ClientGuard<'_> {
    fn deref_mut(&mut self) -> &mut TdsClient {
        self.slot.client.as_mut().expect("connected by lock")
    }
}

/// Whether `e` is an error reported by the server (as opposed to I/O,
/// protocol or decoding errors).
fn is_server_error(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<tiberius::error::Error>(),
        Some(tiberius::error::Error::Server(_))
    )
}

/// Open a client with SQL Server authentication.
pub async fn open(config: &ConnectionConfig) -> Result<TdsClient> {
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
    Ok(Client::connect(tib_config, tcp.compat_write()).await?)
}

/// Connect to SQL Server
pub async fn connect(config: &ConnectionConfig) -> Result<SqlServerClient> {
    Ok(SqlServerClient::new(config.clone(), open(config).await?))
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
async fn execute_counting(client: &mut TdsClient, batch: &str) -> Result<QueryResult> {
    let affected = client.execute(batch, &[]).await?.total();
    Ok(QueryResult {
        rows_affected: affected,
        ..QueryResult::default()
    })
}

/// Collect the rows of the first result set of `stream`, keeping at most
/// `max_rows` of them. Returns the rows and whether more were available.
///
/// The stream is always read to the end, extra rows being discarded right
/// away (only the kept rows use memory). Dropping a partially read
/// `QueryStream` would leave the rest of the response on the wire: tiberius
/// then flushes it packet by packet at the start of the next query on this
/// shared client, logging a warning for every packet, and a later error in the
/// batch would surface on that unrelated query. Draining here keeps the client
/// clean and reports errors where they belong.
async fn first_result_capped(
    mut stream: QueryStream<'_>,
    max_rows: Option<usize>,
) -> Result<(Vec<tiberius::Row>, bool)> {
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut result_sets = 0usize;
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(_) => result_sets += 1,
            QueryItem::Row(row) if result_sets <= 1 => {
                if max_rows.is_some_and(|max| rows.len() >= max) {
                    truncated = true;
                } else {
                    rows.push(row);
                }
            }
            QueryItem::Row(_) => {}
        }
    }
    Ok((rows, truncated))
}

/// Execute a query on SQL Server, keeping at most `max_rows` rows of the first
/// result set (all when `None`). DML that returns no rows goes through
/// `Client::execute` so that `rows_affected` is the real count.
pub async fn execute_query_limited(
    client: &SqlServerClient,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    let statements: Vec<String> = split_statements(query)
        .into_iter()
        .map(|(_, _, stmt)| stmt)
        .collect();
    let mut client = client.lock().await?;
    let outcome = async {
        if counts_only(&statements) {
            return execute_counting(&mut client, query).await;
        }
        let stream = client.simple_query(query).await?;
        let (rows, truncated) = first_result_capped(stream, max_rows).await?;
        Ok(QueryResult {
            truncated,
            ..rows_to_result(&rows)
        })
    }
    .await;
    client.settle(&outcome);
    outcome
}

/// Join transaction statements into a single T-SQL batch. Running the whole
/// block as ONE batch keeps batch-scoped constructs (e.g. `DECLARE @var`)
/// visible across all statements — running them as separate batches would drop
/// the variables between statements.
///
/// The batch runs with `XACT_ABORT ON`: otherwise most errors (constraint
/// violations…) only end their statement, the batch goes on and its `COMMIT`
/// commits the statements that succeeded. The session setting is restored at
/// the end of the batch (and by the error path of `execute_transaction`).
fn build_tsql_batch(statements: &[String]) -> String {
    format!(
        "SET XACT_ABORT ON;\n{};\nSET XACT_ABORT OFF",
        statements.join(";\n")
    )
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
    let mut client = client.lock().await?;

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

    if outcome.is_err() && is_server_error(outcome.as_ref().unwrap_err()) {
        // The failed batch may have left a transaction open on the shared
        // client; roll it back so later queries aren't poisoned. If that
        // fails too, the client is left dirty and replaced on next use.
        let cleanup = async {
            client
                .simple_query("IF @@TRANCOUNT > 0 ROLLBACK; SET XACT_ABORT OFF")
                .await?
                .into_results()
                .await?;
            Ok(())
        }
        .await;
        client.settle(&cleanup);
    } else {
        // Other errors may leave the response unread: reconnect.
        client.settle(&outcome);
    }
    outcome
}

/// Run `sql` as one batch and read its whole response.
async fn run_batch(client: &mut TdsClient, sql: &str) -> Result<Vec<Vec<tiberius::Row>>> {
    Ok(client.simple_query(sql).await?.into_results().await?)
}

/// Run one statement and return the number of rows it affected
/// (`@@ROWCOUNT`, which does not count the rows changed by triggers).
async fn rows_affected_by(client: &mut TdsClient, stmt: &str) -> Result<u64> {
    let results = run_batch(client, &format!("{stmt};\nSELECT @@ROWCOUNT")).await?;
    let count: Option<i32> = results
        .last()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(0));
    Ok(count.unwrap_or(0).max(0) as u64)
}

/// Run `statements` one by one in a transaction on the shared client.
/// Statement `i` must affect exactly `expected[i]` rows when that is
/// `Some`; on a different count or any error the transaction is rolled back
/// and the error returned. Returns the total number of affected rows.
pub async fn execute_checked_batch(
    client: &SqlServerClient,
    statements: &[String],
    expected: &[Option<u64>],
) -> Result<u64> {
    let mut client = client.lock().await?;
    let outcome = async {
        run_batch(&mut client, "BEGIN TRANSACTION").await?;
        let mut total = 0;
        for (i, stmt) in statements.iter().enumerate() {
            let affected = rows_affected_by(&mut client, stmt).await?;
            if let Some(want) = expected.get(i).copied().flatten() {
                if affected != want {
                    return Err(super::utils::row_count_error(affected));
                }
            }
            total += affected;
        }
        run_batch(&mut client, "COMMIT").await?;
        Ok(total)
    }
    .await;
    let in_step = match &outcome {
        Ok(_) => true,
        // A count mismatch or a server error: the response was read.
        Err(e) => e.downcast_ref::<tiberius::error::Error>().is_none() || is_server_error(e),
    };
    if outcome.is_err() && in_step {
        let cleanup = run_batch(&mut client, "IF @@TRANCOUNT > 0 ROLLBACK").await;
        client.settle(&cleanup);
    } else {
        // Other errors may leave the response unread: the client is
        // replaced (and the server rolls the transaction back).
        client.settle(&outcome);
    }
    outcome
}

/// Get tables grouped by schema
pub async fn get_tables_by_schema(client: &SqlServerClient) -> Result<Vec<SchemaInfo>> {
    let mut client = client.lock().await?;
    let rows = async {
        let stream = client
            .simple_query(
                "SELECT TABLE_SCHEMA, TABLE_NAME FROM INFORMATION_SCHEMA.TABLES 
                 WHERE TABLE_TYPE = 'BASE TABLE' 
                 ORDER BY TABLE_SCHEMA, TABLE_NAME",
            )
            .await?;
        Ok(stream.into_first_result().await?)
    }
    .await;
    client.settle(&rows);
    let rows = rows?;
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
    execute_counted(client, &query).await
}

/// Run a DML statement and return its affected-row count.
async fn execute_counted(client: &SqlServerClient, query: &str) -> Result<u64> {
    let mut client = client.lock().await?;
    let outcome = async { Ok(client.execute(query, &[]).await?.total()) }.await;
    client.settle(&outcome);
    outcome
}

/// Rows of a metadata `query` taking the schema (`@P1`, NULL for the
/// default one) and the table name (`@P2`).
async fn query_rows(
    client: &SqlServerClient,
    query: &str,
    schema: &Option<String>,
    table: &str,
) -> Result<Vec<tiberius::Row>> {
    let mut client = client.lock().await?;
    let outcome = async {
        Ok(client
            .query(query, &[&schema.as_deref(), &table])
            .await?
            .into_first_result()
            .await?)
    }
    .await;
    client.settle(&outcome);
    outcome
}

/// `(schema, table)` of a possibly qualified/bracketed name. `None` schema:
/// the queries use the user's default schema (`SCHEMA_NAME()`).
fn split_table(table_name: &str) -> (Option<String>, String) {
    split_qualified(table_name)
}

/// Columns of a table as `(name, type, nullable, primary key)`, in order.
/// The type carries its length/precision (`nvarchar(50)`, `varchar(max)`,
/// `decimal(10,2)`) so it can be used in DDL.
async fn column_rows(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<(String, String, bool, bool)>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT c.COLUMN_NAME,
                c.DATA_TYPE + CASE
                    WHEN c.DATA_TYPE IN ('char', 'varchar', 'nchar', 'nvarchar', 'binary', 'varbinary')
                        THEN '(' + CASE WHEN c.CHARACTER_MAXIMUM_LENGTH = -1 THEN 'max'
                                        ELSE CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(10)) END + ')'
                    WHEN c.DATA_TYPE IN ('decimal', 'numeric')
                        THEN '(' + CAST(c.NUMERIC_PRECISION AS varchar(10)) + ','
                                 + CAST(c.NUMERIC_SCALE AS varchar(10)) + ')'
                    ELSE '' END,
                c.IS_NULLABLE,
                CASE WHEN pk.COLUMN_NAME IS NOT NULL THEN 1 ELSE 0 END
         FROM INFORMATION_SCHEMA.COLUMNS c
         LEFT JOIN (
             SELECT ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
             FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
             JOIN INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
               ON tc.CONSTRAINT_SCHEMA = ku.CONSTRAINT_SCHEMA
              AND tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME
             WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
         ) pk ON c.TABLE_SCHEMA = pk.TABLE_SCHEMA AND c.TABLE_NAME = pk.TABLE_NAME
             AND c.COLUMN_NAME = pk.COLUMN_NAME
         WHERE c.TABLE_SCHEMA = COALESCE(@P1, SCHEMA_NAME()) AND c.TABLE_NAME = @P2
         ORDER BY c.ORDINAL_POSITION";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let type_name: &str = row.try_get(1)?.unwrap_or("");
        let nullable: &str = row.try_get(2)?.unwrap_or("YES");
        let is_pk: i32 = row.try_get(3)?.unwrap_or(0);
        columns.push((
            name.to_string(),
            type_name.to_string(),
            nullable == "YES",
            is_pk == 1,
        ));
    }
    Ok(columns)
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    Ok(column_rows(client, table_name)
        .await?
        .into_iter()
        .map(|(name, _, nullable, _)| (name, nullable))
        .collect())
}

/// Get primary key columns for a table, in key order
pub async fn get_primary_keys(client: &SqlServerClient, table_name: &str) -> Result<Vec<String>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT ku.COLUMN_NAME
         FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
         JOIN INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
           ON tc.CONSTRAINT_SCHEMA = ku.CONSTRAINT_SCHEMA
          AND tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME
         WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
           AND ku.TABLE_SCHEMA = COALESCE(@P1, SCHEMA_NAME()) AND ku.TABLE_NAME = @P2
         ORDER BY ku.ORDINAL_POSITION";

    let rows = query_rows(client, query, &schema, &table).await?;

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
    Ok(column_rows(client, table_name)
        .await?
        .into_iter()
        .map(|(name, ..)| name)
        .collect())
}

/// Get full column details for a table (for schema modification)
pub async fn get_table_column_details(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<crate::engine::models::Column>> {
    Ok(column_rows(client, table_name)
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

/// Get the indexes of a table from `sys.indexes` (heaps and INCLUDE columns
/// are left out).
pub async fn get_indexes(client: &SqlServerClient, table_name: &str) -> Result<Vec<IndexInfo>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT i.name, c.name, i.is_unique, i.is_primary_key
         FROM sys.indexes i
         JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
         JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
         JOIN sys.tables t ON t.object_id = i.object_id
         JOIN sys.schemas s ON s.schema_id = t.schema_id
         WHERE s.name = COALESCE(@P1, SCHEMA_NAME()) AND t.name = @P2
           AND i.name IS NOT NULL AND ic.is_included_column = 0
         ORDER BY i.is_primary_key DESC, i.name, ic.key_ordinal";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let column: &str = row.try_get(1)?.unwrap_or("");
        let unique: bool = row.try_get(2)?.unwrap_or(false);
        let primary: bool = row.try_get(3)?.unwrap_or(false);
        out.push((name.to_string(), column.to_string(), unique, primary));
    }
    Ok(group_indexes(out))
}

/// Get the foreign keys of a table from `sys.foreign_keys`. The referenced
/// table is qualified with its schema when it lives in another one.
pub async fn get_foreign_keys(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<ForeignKeyInfo>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT fk.name, pc.name, rs.name, rt.name, rc.name, s.name
         FROM sys.foreign_keys fk
         JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
         JOIN sys.tables t ON t.object_id = fk.parent_object_id
         JOIN sys.schemas s ON s.schema_id = t.schema_id
         JOIN sys.columns pc
              ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
         JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id
         JOIN sys.schemas rs ON rs.schema_id = rt.schema_id
         JOIN sys.columns rc
              ON rc.object_id = fkc.referenced_object_id
             AND rc.column_id = fkc.referenced_column_id
         WHERE s.name = COALESCE(@P1, SCHEMA_NAME()) AND t.name = @P2
         ORDER BY fk.name, fkc.constraint_column_id";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let column: &str = row.try_get(1)?.unwrap_or("");
        let ref_schema: &str = row.try_get(2)?.unwrap_or("");
        let ref_table: &str = row.try_get(3)?.unwrap_or("");
        let ref_column: &str = row.try_get(4)?.unwrap_or("");
        let own_schema: &str = row.try_get(5)?.unwrap_or("");
        let ref_table = if ref_schema.eq_ignore_ascii_case(own_schema) {
            ref_table.to_string()
        } else {
            format!("{ref_schema}.{ref_table}")
        };
        out.push((
            name.to_string(),
            column.to_string(),
            ref_table,
            ref_column.to_string(),
        ));
    }
    Ok(group_foreign_keys(out))
}

/// Test the connection
pub async fn test(client: &SqlServerClient) -> Result<()> {
    let mut client = client.lock().await?;
    let outcome = async {
        client
            .simple_query("SELECT 1")
            .await?
            .into_results()
            .await?;
        Ok(())
    }
    .await;
    client.settle(&outcome);
    outcome
}

/// Text of a cell, decoded from its TDS type (so that tinyint, smallint,
/// real, uniqueidentifier… are not mistaken for NULL).
fn get_value(row: &tiberius::Row, index: usize) -> String {
    use tiberius::ColumnData;
    fn text<T: ToString>(v: &Option<T>) -> Option<String> {
        v.as_ref().map(ToString::to_string)
    }
    let Some((_, data)) = row.cells().nth(index) else {
        return "NULL".to_string();
    };
    let value = match data {
        ColumnData::U8(v) => text(v),
        ColumnData::I16(v) => text(v),
        ColumnData::I32(v) => text(v),
        ColumnData::I64(v) => text(v),
        ColumnData::F32(v) => text(v),
        ColumnData::F64(v) => text(v),
        ColumnData::Bit(v) => text(v),
        ColumnData::Guid(v) => text(v),
        ColumnData::Numeric(v) => text(v),
        ColumnData::String(v) => v.as_deref().map(str::to_string),
        ColumnData::Xml(v) => v.as_deref().map(ToString::to_string),
        ColumnData::Binary(v) => v
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned()),
        // Date/time types: no offset unless the column has one.
        _ => row
            .try_get::<chrono::NaiveDateTime, _>(index)
            .ok()
            .flatten()
            .map(|v| v.to_string())
            .or_else(|| {
                row.try_get::<chrono::DateTime<chrono::FixedOffset>, _>(index)
                    .ok()
                    .flatten()
                    .map(|v| v.to_rfc3339())
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
            }),
    };
    value.unwrap_or_else(|| "NULL".to_string())
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
            "SET XACT_ABORT ON;\nBEGIN TRAN;\nDECLARE @x INT = 5;\nSELECT @x;\nCOMMIT;\n\
             SET XACT_ABORT OFF"
        );
    }
}
