use std::time::Instant;

use anyhow::{bail, Result};

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{QueryResult, SchemaInfo};
use crate::engine::sql::statements::{
    get_execution_unit_at_cursor, is_transaction_end, is_transaction_start, single_table_source,
    split_statements, ExecutionUnit,
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
pub async fn run_table_page(
    conn: &DatabaseConnection,
    sql: &str,
    table: &str,
) -> Result<QueryResult> {
    let mut result = run_query(conn, sql).await?;
    if result.columns.is_empty() && result.rows.is_empty() {
        if let Ok(columns) = conn.get_table_column_details(table).await {
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
async fn enrich(conn: &DatabaseConnection, sql: &str, result: &mut QueryResult) {
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
    if result.primary_key.is_empty() {
        return None;
    }
    let names = &result.columns;
    let unique = names
        .iter()
        .enumerate()
        .all(|(i, c)| names[..i].iter().all(|o| o.name != c.name));
    let keyed = result
        .primary_key
        .iter()
        .all(|pk| names.iter().any(|c| c.name == *pk));
    (unique && keyed).then_some(table)
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
pub async fn run_script(
    conn: &DatabaseConnection,
    text: &str,
    max_rows: Option<usize>,
) -> Vec<StatementOutcome> {
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
pub async fn run_all(conn: &DatabaseConnection, text: &str) -> Result<QueryResult> {
    if text.trim().is_empty() {
        bail!("Query is empty");
    }
    run_query(conn, text).await
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
        let r = run_table_page(&conn, "SELECT * FROM e LIMIT 500", "e")
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
        let r = run_table_page(&conn, "SELECT * FROM users", "users")
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

    #[tokio::test]
    async fn refresh_schemas_lists_tables() {
        let conn = sqlite_mem(SETUP).await;
        let schemas = refresh_schemas(&conn).await.unwrap();
        assert!(schemas
            .iter()
            .any(|s| s.tables.iter().any(|t| t == "users")));
    }
}
