use anyhow::{anyhow, bail, Result};

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{QueryResult, SchemaInfo};
use crate::engine::sql::statements::{
    extract_table_from_query, get_execution_unit_at_cursor, ExecutionUnit,
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

/// Execute one query and, when it reads from a single table, annotate the
/// result columns with nullability and primary-key flags (used by row editing).
pub async fn run_query(conn: &DatabaseConnection, sql: &str) -> Result<QueryResult> {
    let mut result = conn.execute_query(sql).await?;
    if let Some(table) = extract_table_from_query(sql) {
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
        }
    }
    Ok(result)
}

/// Execute the statement (or transaction block) under `cursor` in `text`.
///
/// Errors are prefixed with "Transaction rolled back:" or "Query error:"
/// depending on the kind of unit that failed.
pub async fn run_at_cursor(
    conn: &DatabaseConnection,
    text: &str,
    cursor: usize,
) -> Result<Executed> {
    match get_execution_unit_at_cursor(text, cursor) {
        ExecutionUnit::UnterminatedTransaction => {
            bail!("Transaction not terminated: add COMMIT or ROLLBACK")
        }
        ExecutionUnit::Transaction(statements) => {
            let result = conn
                .execute_transaction(&statements)
                .await
                .map_err(|e| anyhow!("Transaction rolled back: {e}"))?;
            Ok(Executed::Transaction {
                result,
                statements: statements.len(),
            })
        }
        ExecutionUnit::Single(sql) if sql.trim().is_empty() => {
            bail!("No query at cursor position")
        }
        ExecutionUnit::Single(sql) => {
            let result = run_query(conn, &sql)
                .await
                .map_err(|e| anyhow!("Query error: {e}"))?;
            Ok(Executed::Query(result))
        }
    }
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
        assert!(run_at_cursor(&conn, sql, 0).await.is_err());
        assert_eq!(count(&conn, "users").await, "2");
    }

    #[tokio::test]
    async fn unterminated_transaction_and_empty_are_errors() {
        let conn = sqlite_mem(SETUP).await;
        let err = run_at_cursor(&conn, "BEGIN; SELECT 1;", 0)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("COMMIT or ROLLBACK"));
        assert!(run_at_cursor(&conn, "   ", 0).await.is_err());
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
