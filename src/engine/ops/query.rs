use std::time::Instant;

use anyhow::{bail, Result};

use crate::engine::db::{batch_error_line, is_result_select, DatabaseConnection};
use crate::engine::models::{QueryResult, SchemaInfo};
use crate::engine::sql::statements::{
    get_execution_unit_at_cursor, is_transaction_end, is_transaction_start, single_table_source,
    split_statements, split_tsql_batches, Batch, ExecutionUnit,
};

/// Outcome of executing the SQL at the cursor.
#[derive(Debug)]
pub enum Executed {
    Query(QueryResult),
    /// A whole `BEGIN … COMMIT/ROLLBACK` block, committed.
    Transaction {
        result: QueryResult,
        statements: usize,
    },
}

/// Why the SQL at the cursor could not be run. `Display` gives the message
/// without any UI prefix; a single-statement failure shows the raw database
/// error.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("Transaction not terminated: add COMMIT or ROLLBACK")]
    Unterminated,
    #[error("No query at cursor position")]
    Empty,
    /// The single statement at the cursor failed.
    #[error("{0}")]
    Query(anyhow::Error),
    /// A `BEGIN … COMMIT/ROLLBACK` block failed and was rolled back.
    #[error("Transaction rolled back: {0}")]
    RolledBack(anyhow::Error),
}

/// One executed unit of a script (a statement, or a whole transaction block).
#[derive(Debug)]
pub struct StatementOutcome {
    /// The SQL as executed (a transaction block is joined with ";\n").
    pub sql: String,
    pub result: Result<QueryResult, String>,
    pub elapsed_ms: u128,
}

/// Execute one query and, when it reads from a single table, annotate the
/// result columns with nullability and primary-key flags (used by row editing).
pub async fn run_query(conn: &DatabaseConnection, sql: &str) -> Result<QueryResult> {
    run_query_limited(conn, sql, None).await
}

/// `run_query` for a page of `table`'s rows (`SELECT * …`). Drivers read
/// the column list from the returned rows, so an empty page has none: take
/// it from the table's metadata then, so headers and inserts still work.
/// The rows are read on `conn`, the metadata on `meta` (the same connection,
/// or the session's pool when `conn` holds a manual transaction: a failing
/// metadata query must not break it).
pub async fn run_table_page(
    conn: &DatabaseConnection,
    meta: &DatabaseConnection,
    sql: &str,
    table: &str,
) -> Result<QueryResult> {
    let mut result = conn.execute_query(sql).await?;
    enrich(meta, sql, &mut result).await;
    if result.columns.is_empty() && result.rows.is_empty() {
        if let Ok(columns) = meta.get_table_column_details(table).await {
            result.columns = columns;
        }
    }
    Ok(result)
}

/// `run_query` keeping at most `max_rows` rows (all when `None`).
async fn run_query_limited(
    conn: &DatabaseConnection,
    sql: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    let mut result = conn.execute_query_limited(sql, max_rows).await?;
    enrich(conn, sql, &mut result).await;
    Ok(result)
}

/// When each row of `sql` is a row of a single table
/// (`single_table_source`), flag the result columns with their nullability
/// and primary-key status and record the table's key in
/// `result.primary_key`. Joins and other shapes are left alone: a column
/// named like a key column of the first table is not that key. Metadata
/// lookups are best effort.
pub(crate) async fn enrich(conn: &DatabaseConnection, sql: &str, result: &mut QueryResult) {
    let Some(table) = single_table_source(sql) else {
        return;
    };
    if let Ok(nullability) = conn.get_column_nullability(&table).await {
        for col in &mut result.columns {
            if let Some(&nullable) = nullability.get(&col.name) {
                col.nullable = nullable;
            }
        }
    }
    if let Ok(pks) = conn.get_primary_keys(&table).await {
        for col in &mut result.columns {
            col.is_primary_key = pks.contains(&col.name);
        }
        result.primary_key = pks;
    }
}

/// The table whose rows `result` (from `sql`) shows, when they can be
/// edited: `sql` reads a single table (`single_table_source`), that table
/// has a primary key, every key column appears exactly once in the result
/// and no column name is repeated. Updates and deletes then target exactly
/// the displayed row.
pub fn editable_table(sql: &str, result: &QueryResult) -> Option<String> {
    let table = single_table_source(sql)?;
    has_full_key(result).then_some(table)
}

/// Whether the rows of `result` (read from a single table) can be told
/// apart: the table has a primary key, every key column is in the result
/// and no column name is repeated.
pub fn has_full_key(result: &QueryResult) -> bool {
    let names = &result.columns;
    let unique = names
        .iter()
        .enumerate()
        .all(|(i, c)| names[..i].iter().all(|o| o.name != c.name));
    let keyed = result
        .primary_key
        .iter()
        .all(|pk| names.iter().any(|c| c.name == *pk));
    !result.primary_key.is_empty() && unique && keyed
}

/// Run one execution unit. Single statements are capped at `max_rows` and
/// enriched; a transaction's result is trimmed to `max_rows` afterwards
/// (`execute_transaction` has no cap) so every outcome has the same shape.
async fn execute_unit(
    conn: &DatabaseConnection,
    unit: ExecutionUnit,
    max_rows: Option<usize>,
) -> Result<Executed, RunError> {
    match unit {
        ExecutionUnit::UnterminatedTransaction => Err(RunError::Unterminated),
        ExecutionUnit::Transaction(statements) => {
            let mut result = conn
                .execute_transaction(&statements)
                .await
                .map_err(RunError::RolledBack)?;
            if let Some(max) = max_rows {
                if result.rows.len() > max {
                    result.rows.truncate(max);
                    result.truncated = true;
                }
            }
            Ok(Executed::Transaction {
                result,
                statements: statements.len(),
            })
        }
        ExecutionUnit::Single(sql) if sql.trim().is_empty() => Err(RunError::Empty),
        ExecutionUnit::Single(sql) => {
            let result = run_query_limited(conn, &sql, max_rows)
                .await
                .map_err(RunError::Query)?;
            Ok(Executed::Query(result))
        }
    }
}

/// Execute the statement (or transaction block) under `cursor` in `text`.
pub async fn run_at_cursor(
    conn: &DatabaseConnection,
    text: &str,
    cursor: usize,
) -> Result<Executed, RunError> {
    execute_unit(conn, get_execution_unit_at_cursor(text, cursor), None).await
}

/// SQL of an execution unit as shown in its outcome.
fn unit_sql(unit: &ExecutionUnit) -> String {
    match unit {
        ExecutionUnit::Single(sql) => sql.clone(),
        ExecutionUnit::Transaction(statements) => statements.join(";\n"),
        ExecutionUnit::UnterminatedTransaction => String::new(),
    }
}

/// Run `unit` and time it.
async fn unit_outcome(
    conn: &DatabaseConnection,
    unit: ExecutionUnit,
    sql: String,
    max_rows: Option<usize>,
) -> StatementOutcome {
    let start = Instant::now();
    let result = execute_unit(conn, unit, max_rows)
        .await
        .map(|executed| match executed {
            Executed::Query(result) | Executed::Transaction { result, .. } => result,
        })
        .map_err(|e| e.to_string());
    StatementOutcome {
        sql,
        result,
        elapsed_ms: start.elapsed().as_millis(),
    }
}

/// Execute every statement of `text` in order. Transaction blocks
/// (BEGIN … COMMIT/ROLLBACK/END) run as one unit via `execute_transaction`.
/// Execution stops after the first failing unit (its error is the last outcome).
/// Row-returning results are capped at `max_rows` and enriched with
/// PK/nullability like `run_query` when they read a single table.
///
/// SQL Server and Azure run the script as T-SQL batches instead
/// (`run_tsql_batches`), as SSMS does; a transaction it leaves open is
/// rolled back and reported (`rollback_unterminated`).
pub async fn run_script(
    conn: &DatabaseConnection,
    text: &str,
    max_rows: Option<usize>,
) -> Vec<StatementOutcome> {
    if conn.runs_tsql_batches() {
        let mut outcomes = run_tsql_batches(conn, text, max_rows).await;
        if outcomes.last().is_some_and(|o| o.result.is_ok()) {
            if let Some(unterminated) = rollback_unterminated(conn).await {
                outcomes.push(unterminated);
            }
        }
        return outcomes;
    }
    let statements: Vec<String> = split_statements(text)
        .into_iter()
        .map(|(_, _, stmt)| stmt)
        .collect();
    let mut outcomes = Vec::new();
    let mut i = 0;
    while i < statements.len() {
        let (unit, sql, next) = if is_transaction_start(&statements[i]) {
            match (i..statements.len()).find(|&j| is_transaction_end(&statements[j])) {
                Some(end) => {
                    let block = statements[i..=end].to_vec();
                    let sql = block.join(";\n");
                    (ExecutionUnit::Transaction(block), sql, end + 1)
                }
                None => (
                    ExecutionUnit::UnterminatedTransaction,
                    statements[i..].join(";\n"),
                    statements.len(),
                ),
            }
        } else {
            let sql = statements[i].clone();
            (ExecutionUnit::Single(sql.clone()), sql, i + 1)
        };
        let outcome = unit_outcome(conn, unit, sql, max_rows).await;
        let failed = outcome.result.is_err();
        outcomes.push(outcome);
        if failed {
            break;
        }
        i = next;
    }
    outcomes
}

/// Script line (1-based) of line `server_line` of a batch whose first line
/// is script line `start_line`.
fn script_line(start_line: usize, server_line: u32) -> usize {
    start_line + (server_line as usize).saturating_sub(1)
}

/// Message of a failed batch: a server error raised by the batch text keeps
/// the server's message and names the script line.
fn batch_error(e: &anyhow::Error, start_line: usize) -> String {
    match batch_error_line(e) {
        Some((message, line)) => format!(
            "{message} (ligne {} du script)",
            script_line(start_line, line)
        ),
        None => e.to_string(),
    }
}

/// The statements of `batch` that produced its `sets` result sets, when
/// each can be matched with one: every result set then comes from a
/// `SELECT` that always returns one (`is_result_select`), in order. `None`
/// when they don't add up (procedure calls, `IF … SELECT`, statements not
/// separated by `;`…).
pub(crate) fn result_statements(batch: &str, sets: usize) -> Option<Vec<String>> {
    let selects: Vec<String> = split_statements(batch)
        .into_iter()
        .map(|(_, _, stmt)| stmt)
        .filter(|stmt| is_result_select(stmt))
        .collect();
    (sets > 0 && selects.len() == sets).then_some(selects)
}

/// Run the T-SQL script `text` (SQL Server, Azure) in auto-commit mode,
/// batch by batch (`split_tsql_batches`, `run_tsql_batch`), stopping at the
/// first failing batch.
async fn run_tsql_batches(
    conn: &DatabaseConnection,
    text: &str,
    max_rows: Option<usize>,
) -> Vec<StatementOutcome> {
    let mut outcomes = Vec::new();
    for batch in split_tsql_batches(text) {
        outcomes.extend(run_tsql_batch(conn, conn, batch, max_rows, true).await);
        if outcomes.last().is_some_and(|o| o.result.is_err()) {
            break;
        }
    }
    outcomes
}

/// Run one T-SQL batch in one round trip, so its variables are visible to
/// all its statements. Each result set gives an outcome (a result tab),
/// titled and enriched (metadata read on `meta`) with the `SELECT` that
/// produced it when known (`result_statements`), else with the whole batch;
/// a batch without result sets gives one outcome with its affected-row
/// count. The batch's time goes to its first outcome. On failure, the only
/// outcome is the error, naming the script line. `auto`: see
/// `sqlserver::execute_batch`.
pub(crate) async fn run_tsql_batch(
    conn: &DatabaseConnection,
    meta: &DatabaseConnection,
    batch: Batch,
    max_rows: Option<usize>,
    auto: bool,
) -> Vec<StatementOutcome> {
    let start = Instant::now();
    let executed = conn.execute_tsql_batch(&batch.sql, max_rows, auto).await;
    let elapsed_ms = start.elapsed().as_millis();
    let results = match executed {
        Ok(results) => results,
        Err(e) => {
            return vec![StatementOutcome {
                result: Err(batch_error(&e, batch.start_line)),
                sql: batch.sql,
                elapsed_ms,
            }]
        }
    };
    let sets = results.iter().filter(|r| !r.columns.is_empty()).count();
    let mut statements = result_statements(&batch.sql, sets).map(Vec::into_iter);
    let mut outcomes = Vec::new();
    for (i, mut result) in results.into_iter().enumerate() {
        let statement = match &mut statements {
            Some(statements) if !result.columns.is_empty() => statements.next(),
            _ => None,
        };
        let sql = match statement {
            Some(sql) => {
                enrich(meta, &sql, &mut result).await;
                sql
            }
            None => batch.sql.clone(),
        };
        let elapsed_ms = if i == 0 { elapsed_ms } else { 0 };
        result.execution_time_ms = elapsed_ms;
        outcomes.push(StatementOutcome {
            sql,
            result: Ok(result),
            elapsed_ms,
        });
    }
    outcomes
}

/// After a script run in auto-commit mode: a transaction it left open (a
/// `BEGIN TRAN` without `COMMIT`) would hold its locks on the shared
/// session; it is rolled back and reported.
async fn rollback_unterminated(conn: &DatabaseConnection) -> Option<StatementOutcome> {
    let message = match conn.rollback_open_transaction().await {
        Ok(false) => return None,
        Ok(true) => TSQL_UNTERMINATED.to_string(),
        Err(e) => format!("Fin du script : {e}"),
    };
    Some(StatementOutcome {
        sql: String::new(),
        result: Err(message),
        elapsed_ms: 0,
    })
}

/// A script left a transaction open in auto-commit mode.
pub const TSQL_UNTERMINATED: &str =
    "Transaction non terminée à la fin du script : elle a été annulée (ajoutez COMMIT ou ROLLBACK)";

/// Execute the statement or transaction block under the cursor (Ctrl+Enter
/// without selection), with the same outcome shape as `run_script`.
pub async fn run_at_cursor_outcomes(
    conn: &DatabaseConnection,
    text: &str,
    cursor: usize,
    max_rows: Option<usize>,
) -> Vec<StatementOutcome> {
    let unit = get_execution_unit_at_cursor(text, cursor);
    let sql = unit_sql(&unit);
    vec![unit_outcome(conn, unit, sql, max_rows).await]
}

/// Execute the whole editor content as one query (F5).
///
/// SQL Server and Azure run it as T-SQL batches (`run_script`): the result
/// is the last result set, or the last outcome when none returned rows.
pub async fn run_all(conn: &DatabaseConnection, text: &str) -> Result<QueryResult> {
    if text.trim().is_empty() {
        bail!("Query is empty");
    }
    if !conn.runs_tsql_batches() {
        return run_query(conn, text).await;
    }
    let mut outcomes = run_script(conn, text, None).await;
    if let Some(i) = outcomes.iter().position(|o| o.result.is_err()) {
        bail!("{}", outcomes.swap_remove(i).result.unwrap_err());
    }
    let last_set = outcomes
        .iter()
        .rposition(|o| o.result.as_ref().is_ok_and(|r| !r.columns.is_empty()));
    match last_set.or(outcomes.len().checked_sub(1)) {
        Some(i) => Ok(outcomes.swap_remove(i).result.unwrap_or_default()),
        None => bail!("Query is empty"),
    }
}

pub async fn refresh_schemas(conn: &DatabaseConnection) -> Result<Vec<SchemaInfo>> {
    conn.get_tables_by_schema().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ops::test_support::{count, sqlite_mem};

    const SETUP: &[&str] = &[
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)",
        "INSERT INTO users (name, email) VALUES ('Alice', 'a@x'), ('Bob', NULL)",
    ];

    #[tokio::test]
    async fn run_query_enriches_columns_with_pk_and_nullability() {
        let conn = sqlite_mem(SETUP).await;
        let r = run_query(&conn, "SELECT * FROM users").await.unwrap();
        assert_eq!(r.rows.len(), 2);
        let id = r.columns.iter().find(|c| c.name == "id").unwrap();
        let name = r.columns.iter().find(|c| c.name == "name").unwrap();
        assert!(id.is_primary_key);
        assert!(!name.nullable);
    }

    #[tokio::test]
    async fn only_single_table_results_are_enriched_and_editable() {
        let conn = sqlite_mem(&[
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, valid_from TEXT)",
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total INTEGER)",
            "CREATE TABLE pair (a INTEGER, b INTEGER, v TEXT, PRIMARY KEY (a, b))",
            "CREATE TABLE nokey (x INTEGER, y TEXT)",
            "INSERT INTO users VALUES (1, 'Alice', '2024'), (2, 'Bob', NULL)",
            "INSERT INTO orders VALUES (10, 1, 5)",
            "INSERT INTO pair VALUES (1, 1, 'x'), (1, 2, 'y')",
            "INSERT INTO nokey VALUES (1, 'a')",
        ])
        .await;
        let editable = |sql: &'static str| {
            let conn = &conn;
            async move {
                let r = run_query(conn, sql).await.unwrap();
                (editable_table(sql, &r), r)
            }
        };

        let (table, r) = editable("SELECT valid_from, id FROM users").await;
        assert_eq!(table.as_deref(), Some("users"));
        assert_eq!(r.primary_key, ["id"]);
        assert!(r.columns[1].is_primary_key);

        // A join: the "id" column is not flagged, nothing is editable.
        let (table, r) =
            editable("SELECT o.id, u.name FROM orders o JOIN users u ON u.id = o.user_id").await;
        assert_eq!(table, None);
        assert!(r.columns.iter().all(|c| !c.is_primary_key));
        assert!(r.primary_key.is_empty());

        // Composite key: every key column must be selected, once.
        let (table, r) = editable("SELECT a, v FROM pair").await;
        assert_eq!(table, None, "b missing");
        assert_eq!(r.primary_key, ["a", "b"]);
        let (table, _) = editable("SELECT a, b, v FROM pair").await;
        assert_eq!(table.as_deref(), Some("pair"));
        let (table, _) = editable("SELECT a, b, a FROM pair").await;
        assert_eq!(table, None, "a twice");
        let (table, _) = editable("SELECT *, v FROM pair").await;
        assert_eq!(table, None, "duplicate column names");

        // No primary key: never editable.
        let (table, _) = editable("SELECT * FROM nokey").await;
        assert_eq!(table, None);
        // Aliases and expressions: not editable.
        let (table, _) = editable("SELECT name AS id FROM users").await;
        assert_eq!(table, None);
    }

    #[tokio::test]
    async fn table_page_of_an_empty_table_still_has_its_columns() {
        let conn =
            sqlite_mem(&["CREATE TABLE e (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"]).await;
        let r = run_table_page(&conn, &conn, "SELECT * FROM e LIMIT 500", "e")
            .await
            .unwrap();
        assert!(r.rows.is_empty());
        let names: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "name"]);
        assert!(r.columns[0].is_primary_key);
        assert!(!r.columns[1].nullable);
    }

    #[tokio::test]
    async fn table_page_with_rows_keeps_the_result_columns() {
        let conn = sqlite_mem(SETUP).await;
        let r = run_table_page(&conn, &conn, "SELECT * FROM users", "users")
            .await
            .unwrap();
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.columns.len(), 3);
    }

    #[tokio::test]
    async fn run_unit_single_and_transaction() {
        let conn = sqlite_mem(SETUP).await;
        let sql = "BEGIN;\nINSERT INTO users (name) VALUES ('C');\nCOMMIT;\nSELECT * FROM users";
        let out = run_at_cursor(&conn, sql, 10).await.unwrap();
        assert!(matches!(out, Executed::Transaction { statements: 3, .. }));
        assert_eq!(count(&conn, "users").await, "3");

        let cursor = sql.find("SELECT").unwrap();
        let out = run_at_cursor(&conn, sql, cursor).await.unwrap();
        let Executed::Query(r) = out else {
            panic!("expected a query")
        };
        assert_eq!(r.rows.len(), 3);
    }

    #[tokio::test]
    async fn failing_transaction_rolls_back() {
        let conn = sqlite_mem(SETUP).await;
        let sql =
            "BEGIN;\nINSERT INTO users (name) VALUES ('C');\nINSERT INTO nope VALUES (1);\nCOMMIT;";
        let err = run_at_cursor(&conn, sql, 0).await.unwrap_err();
        assert!(matches!(err, RunError::RolledBack(_)));
        assert!(err.to_string().starts_with("Transaction rolled back: "));
        assert_eq!(count(&conn, "users").await, "2");
    }

    #[tokio::test]
    async fn single_statement_error_is_raw() {
        let conn = sqlite_mem(SETUP).await;
        let err = run_at_cursor(&conn, "SELECT * FROM nope", 0)
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Query(_)));
        assert!(err.to_string().contains("nope"));
        assert!(!err.to_string().starts_with("Query error"));
    }

    #[tokio::test]
    async fn unterminated_transaction_and_empty_are_errors() {
        let conn = sqlite_mem(SETUP).await;
        let err = run_at_cursor(&conn, "BEGIN; SELECT 1;", 0)
            .await
            .unwrap_err();
        assert!(matches!(err, RunError::Unterminated));
        assert_eq!(
            err.to_string(),
            "Transaction not terminated: add COMMIT or ROLLBACK"
        );
        let err = run_at_cursor(&conn, "   ", 0).await.unwrap_err();
        assert!(matches!(err, RunError::Empty));
        assert_eq!(err.to_string(), "No query at cursor position");
    }

    #[tokio::test]
    async fn run_script_returns_one_outcome_per_statement() {
        let conn = sqlite_mem(SETUP).await;
        let out = run_script(
            &conn,
            "SELECT * FROM users; UPDATE users SET email = 'z'; SELECT 1 AS one",
            None,
        )
        .await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].result.as_ref().unwrap().rows.len(), 2);
        assert_eq!(out[1].result.as_ref().unwrap().rows_affected, 2);
        assert_eq!(out[2].result.as_ref().unwrap().rows[0][0], "1");
    }

    #[tokio::test]
    async fn run_script_groups_transactions_and_stops_on_error() {
        let conn = sqlite_mem(SETUP).await;
        let sql =
            "BEGIN; INSERT INTO users (name) VALUES ('C'); COMMIT; SELECT * FROM nope; SELECT 2";
        let out = run_script(&conn, sql, None).await;
        assert_eq!(
            out.len(),
            2,
            "transaction block + failing select, then stop"
        );
        assert!(out[0].result.is_ok());
        assert!(out[0].sql.contains("INSERT"));
        assert!(out[1].result.is_err());
        assert_eq!(count(&conn, "users").await, "3");
    }

    #[tokio::test]
    async fn run_script_caps_rows_and_enriches_columns() {
        let conn = sqlite_mem(SETUP).await;
        let out = run_script(&conn, "SELECT * FROM users", Some(1)).await;
        let r = out[0].result.as_ref().unwrap();
        assert_eq!(r.rows.len(), 1);
        assert!(r.truncated);
        assert!(
            r.columns
                .iter()
                .find(|c| c.name == "id")
                .unwrap()
                .is_primary_key
        );
    }

    #[tokio::test]
    async fn run_script_reports_unterminated_transaction_and_stops() {
        let conn = sqlite_mem(SETUP).await;
        let out = run_script(
            &conn,
            "SELECT 1; BEGIN; INSERT INTO users (name) VALUES ('C')",
            None,
        )
        .await;
        assert_eq!(out.len(), 2);
        assert!(out[1]
            .result
            .as_ref()
            .unwrap_err()
            .contains("COMMIT or ROLLBACK"));
        assert_eq!(count(&conn, "users").await, "2");
    }

    #[tokio::test]
    async fn run_at_cursor_outcomes_reports_unterminated_transaction() {
        let conn = sqlite_mem(SETUP).await;
        let out = run_at_cursor_outcomes(&conn, "BEGIN; SELECT 1;", 0, None).await;
        assert_eq!(out.len(), 1);
        assert!(out[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("COMMIT or ROLLBACK"));
    }

    #[tokio::test]
    async fn run_at_cursor_outcomes_runs_statement_with_cap() {
        let conn = sqlite_mem(SETUP).await;
        let sql = "SELECT 1; SELECT * FROM users";
        let out = run_at_cursor_outcomes(&conn, sql, sql.len(), Some(1)).await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sql, "SELECT * FROM users");
        let r = out[0].result.as_ref().unwrap();
        assert_eq!(r.rows.len(), 1);
        assert!(r.truncated);
    }

    #[test]
    fn script_lines_of_batch_lines() {
        assert_eq!(script_line(1, 3), 3);
        assert_eq!(script_line(37, 1), 37);
        assert_eq!(script_line(10, 4), 13);
        assert_eq!(script_line(5, 0), 5);
    }

    #[test]
    fn result_sets_are_matched_with_their_selects() {
        let batch = "DECLARE @n INT = 1;
SELECT @n = 2;
SELECT * FROM t;
                     INSERT INTO u SELECT 1;
SELECT @n AS n";
        assert_eq!(
            result_statements(batch, 2).unwrap(),
            ["SELECT * FROM t", "SELECT @n AS n"]
        );
        // Not adding up (an `IF … SELECT`, a procedure…): unknown.
        assert_eq!(result_statements(batch, 3), None);
        assert_eq!(result_statements(batch, 1), None);
        assert_eq!(result_statements("UPDATE t SET a = 1", 0), None);
        // Statements not separated by `;`: one statement, two result sets.
        assert_eq!(
            result_statements(
                "SELECT 1
SELECT 2",
                2
            ),
            None
        );
        assert_eq!(
            result_statements(
                "SELECT 1
SELECT 2",
                1
            )
            .unwrap(),
            ["SELECT 1
SELECT 2"]
        );
    }

    #[tokio::test]
    async fn run_script_on_other_engines_ignores_go() {
        // `GO` means nothing outside SQL Server: SQLite reports the error.
        let conn = sqlite_mem(SETUP).await;
        let out = run_script(
            &conn,
            "SELECT 1;
GO
SELECT 2",
            None,
        )
        .await;
        assert_eq!(out.len(), 2);
        assert!(out[1].result.is_err());
    }

    #[tokio::test]
    async fn refresh_schemas_lists_tables() {
        let conn = sqlite_mem(SETUP).await;
        let schemas = refresh_schemas(&conn).await.unwrap();
        assert!(schemas
            .iter()
            .any(|s| s.tables.iter().any(|t| t == "users")));
    }
}
