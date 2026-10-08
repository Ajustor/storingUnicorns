use anyhow::Result;

use crate::engine::db::utils::{build_delete_query, build_insert_query, build_update_query};
use crate::engine::db::DatabaseConnection;
use crate::engine::models::{Column, DatabaseType};
use crate::engine::sql::statements::quote_chars;

/// Update one row identified by its original values. Returns 0 without
/// touching the database when nothing changed.
pub async fn update_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    original: &[String],
    new: &[String],
) -> Result<u64> {
    if original == new {
        return Ok(0);
    }
    conn.update_row(table, columns, original, new).await
}

/// Insert a row, skipping the auto-generated `system_columns` indices.
pub async fn insert_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
) -> Result<u64> {
    conn.insert_row(table, columns, values, system_columns)
        .await
}

/// Delete the row matching `values`, quoting identifiers with `quotes`.
pub async fn delete_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    values: &[String],
    quotes: (char, char),
) -> Result<u64> {
    let sql = build_delete_query(table, columns, values, quotes.0, quotes.1);
    Ok(conn.execute_query(&sql).await?.rows_affected)
}

/// Pending edits of a data grid, applied together by `submit_changes`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RowChanges {
    /// (original row, edited row)
    pub updates: Vec<(Vec<String>, Vec<String>)>,
    pub inserts: Vec<Vec<String>>,
    /// Original rows to delete.
    pub deletes: Vec<Vec<String>>,
}

#[allow(dead_code)] // used by the GUI (plan 3b)
impl RowChanges {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.inserts.is_empty() && self.deletes.is_empty()
    }

    /// Number of pending edits (updates + inserts + deletes).
    pub fn len(&self) -> usize {
        self.updates.len() + self.inserts.len() + self.deletes.len()
    }
}

/// `(begin, commit)` statements for the dialect.
pub fn transaction_bounds(db: &DatabaseType) -> (&'static str, &'static str) {
    match db {
        DatabaseType::Postgres | DatabaseType::SQLite => ("BEGIN", "COMMIT"),
        DatabaseType::MySQL => ("START TRANSACTION", "COMMIT"),
        DatabaseType::SQLServer | DatabaseType::Azure => ("BEGIN TRANSACTION", "COMMIT"),
    }
}

/// Build the statements for `changes` (deletes, then updates, then inserts),
/// wrap them in the dialect's BEGIN/COMMIT and run them with
/// `execute_transaction` (rolled back on any error). Returns the number of
/// statements applied (unchanged updates are skipped).
pub async fn submit_changes(
    conn: &DatabaseConnection,
    db: &DatabaseType,
    table: &str,
    columns: &[Column],
    system_columns: &[usize],
    changes: &RowChanges,
) -> Result<usize> {
    let (qs, qe) = quote_chars(db);
    let deletes = changes
        .deletes
        .iter()
        .map(|values| build_delete_query(table, columns, values, qs, qe));
    let updates = changes
        .updates
        .iter()
        .filter_map(|(original, new)| build_update_query(table, columns, original, new, qs, qe));
    let inserts = changes
        .inserts
        .iter()
        .filter_map(|values| build_insert_query(table, columns, values, system_columns, qs, qe));
    let statements: Vec<String> = deletes.chain(updates).chain(inserts).collect();
    if statements.is_empty() {
        return Ok(0);
    }

    let applied = statements.len();
    let (begin, commit) = transaction_bounds(db);
    let mut block = Vec::with_capacity(applied + 2);
    block.push(begin.to_string());
    block.extend(statements);
    block.push(commit.to_string());
    conn.execute_transaction(&block).await?;
    Ok(applied)
}

/// Result of an operation applied to several tables.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BatchReport {
    pub total: usize,
    pub succeeded: usize,
    pub rows_affected: u64,
    /// `"<table>: <error>"`, one per failed table.
    pub errors: Vec<String>,
}

/// `DELETE FROM` each (already quoted) table name. `progress(done, total, table)`
/// is called before each table.
pub async fn truncate_tables(
    conn: &DatabaseConnection,
    tables: &[String],
    mut progress: impl FnMut(usize, usize, &str),
) -> BatchReport {
    let mut report = BatchReport {
        total: tables.len(),
        ..Default::default()
    };
    for (i, table) in tables.iter().enumerate() {
        progress(i, tables.len(), table);
        match conn.execute_query(&format!("DELETE FROM {table}")).await {
            Ok(r) => {
                report.succeeded += 1;
                report.rows_affected += r.rows_affected;
            }
            Err(e) => report.errors.push(format!("{table}: {e}")),
        }
    }
    report
}

/// Indices of columns the database fills itself (ids, serials, timestamps),
/// left out of INSERT statements by default.
pub fn detect_system_columns(columns: &[Column]) -> Vec<usize> {
    let first = columns
        .first()
        .map(|c| c.name.to_lowercase())
        .unwrap_or_default();
    columns
        .iter()
        .enumerate()
        .filter_map(|(idx, col)| {
            let name = col.name.to_lowercase();
            let ty = col.type_name.to_lowercase();
            let is_auto_id = name == "id"
                || (name.ends_with("_id") && name.starts_with(&first))
                || ty.contains("serial")
                || ty.contains("identity")
                || ty.contains("auto_increment");
            let is_timestamp = [
                "created_at",
                "updated_at",
                "createdat",
                "updatedat",
                "created_on",
                "updated_on",
                "inserted_at",
                "modified_at",
            ]
            .iter()
            .any(|p| name.contains(p));
            (is_auto_id || is_timestamp).then_some(idx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::Column;
    use crate::engine::ops::test_support::{count, sqlite_mem};

    const SETUP: &[&str] = &[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b')",
        "CREATE TABLE u (x INTEGER)",
        "INSERT INTO u VALUES (1), (2), (3)",
    ];

    fn cols() -> Vec<Column> {
        ["id", "name"]
            .iter()
            .map(|n| Column {
                name: n.to_string(),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: *n == "id",
            })
            .collect()
    }

    fn row(a: &str, b: &str) -> Vec<String> {
        vec![a.into(), b.into()]
    }

    #[tokio::test]
    async fn update_insert_delete_roundtrip() {
        let conn = sqlite_mem(SETUP).await;
        let n = update_row(&conn, "t", &cols(), &row("1", "a"), &row("1", "z"))
            .await
            .unwrap();
        assert_eq!(n, 1);
        let n = insert_row(&conn, "t", &cols(), &row("", "c"), &[0])
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(count(&conn, "t").await, "3");
        let n = delete_row(&conn, "t", &cols(), &row("2", "b"), ('"', '"'))
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(count(&conn, "t").await, "2");
    }

    #[tokio::test]
    async fn update_without_changes_is_noop() {
        let conn = sqlite_mem(SETUP).await;
        let n = update_row(&conn, "t", &cols(), &row("1", "a"), &row("1", "a"))
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn truncate_tables_reports_per_table() {
        let conn = sqlite_mem(SETUP).await;
        let report = truncate_tables(
            &conn,
            &["t".into(), "missing".into(), "u".into()],
            |_, _, _| {},
        )
        .await;
        assert_eq!(report.total, 3);
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.rows_affected, 5);
        assert_eq!(count(&conn, "t").await, "0");
        assert_eq!(count(&conn, "u").await, "0");
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].starts_with("missing:"));
    }

    #[tokio::test]
    async fn submit_applies_all_changes_atomically() {
        let conn = sqlite_mem(SETUP).await;
        let changes = RowChanges {
            updates: vec![(row("1", "a"), row("1", "A"))],
            inserts: vec![row("", "c")],
            deletes: vec![row("2", "b")],
        };
        let n = submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &changes)
            .await
            .unwrap();
        assert_eq!(n, 3);
        let r = conn
            .execute_query("SELECT name FROM t ORDER BY name")
            .await
            .unwrap();
        let names: Vec<_> = r.rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(names, ["A", "c"]);
    }

    #[tokio::test]
    async fn submit_rolls_back_everything_on_error() {
        let conn = sqlite_mem(&[
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "INSERT INTO t (id, name) VALUES (1, 'a')",
        ])
        .await;
        let changes = RowChanges {
            updates: vec![(row("1", "a"), row("1", "z"))],
            inserts: vec![row("", "NULL")], // violates NOT NULL
            deletes: vec![],
        };
        assert!(
            submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &changes)
                .await
                .is_err()
        );
        let r = conn.execute_query("SELECT name FROM t").await.unwrap();
        assert_eq!(r.rows[0][0], "a", "update must be rolled back");
    }

    #[tokio::test]
    async fn submit_skips_unchanged_updates_and_empty_changes() {
        let conn = sqlite_mem(SETUP).await;
        let changes = RowChanges {
            updates: vec![(row("1", "a"), row("1", "a"))],
            ..Default::default()
        };
        assert!(!changes.is_empty());
        assert_eq!(changes.len(), 1);
        let n = submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &changes)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let empty = RowChanges::default();
        assert!(empty.is_empty());
        let n = submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &empty)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(count(&conn, "t").await, "2");
    }

    #[test]
    fn bounds_per_dialect() {
        assert_eq!(
            transaction_bounds(&DatabaseType::MySQL).0,
            "START TRANSACTION"
        );
        assert_eq!(
            transaction_bounds(&DatabaseType::Azure).0,
            "BEGIN TRANSACTION"
        );
        assert_eq!(
            transaction_bounds(&DatabaseType::SQLite),
            ("BEGIN", "COMMIT")
        );
    }

    #[test]
    fn detects_system_columns() {
        let mk = |n: &str, t: &str| Column {
            name: n.into(),
            type_name: t.into(),
            nullable: true,
            is_primary_key: false,
        };
        let cols = vec![
            mk("id", "int"),
            mk("label", "text"),
            mk("created_at", "timestamp"),
            mk("seq", "serial"),
        ];
        assert_eq!(detect_system_columns(&cols), vec![0, 2, 3]);
    }
}
