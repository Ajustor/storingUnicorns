use anyhow::Result;

use crate::engine::db::utils::{
    build_delete_query, build_insert_query, build_update_query, display_qualified,
};
use crate::engine::db::DatabaseConnection;
use crate::engine::models::{Column, DatabaseType};
use crate::engine::sql::statements::quote_chars;

#[cfg(test)] // integration tests
pub use crate::engine::db::utils::transaction_bounds;

/// Update one row identified by its original values. Returns 0 without
/// touching the database when nothing changed; fails (and changes nothing)
/// unless exactly one row matches.
pub async fn update_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    original: &[String],
    new: &[String],
) -> Result<u64> {
    let (qs, qe) = quote_chars(&conn.db_type());
    let Some(sql) = build_update_query(table, columns, original, new, qs, qe) else {
        return Ok(0);
    };
    conn.execute_checked_batch(&[sql], &[Some(1)]).await
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
/// Fails (and deletes nothing) unless exactly one row matches.
pub async fn delete_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    values: &[String],
    quotes: (char, char),
) -> Result<u64> {
    let sql = build_delete_query(table, columns, values, quotes.0, quotes.1);
    conn.execute_checked_batch(&[sql], &[Some(1)]).await
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

/// Message of a submit refused because rows can't be identified.
pub const NO_PRIMARY_KEY: &str = "Lecture seule : pas de clé primaire";

/// Build the statements for `changes` (deletes, then updates, then inserts)
/// and run them in one transaction with `execute_checked_batch`: every
/// UPDATE and DELETE must affect exactly one row (its row may have been
/// deleted or changed since it was loaded, or the key may not be unique),
/// otherwise everything is rolled back. Rows are identified by the primary
/// key: without one (`columns` has no key column) the submit is refused.
/// Returns the number of statements applied (unchanged updates are skipped).
pub async fn submit_changes(
    conn: &DatabaseConnection,
    db: &DatabaseType,
    table: &str,
    columns: &[Column],
    system_columns: &[usize],
    changes: &RowChanges,
) -> Result<usize> {
    if changes.is_empty() {
        return Ok(0);
    }
    if !columns.iter().any(|c| c.is_primary_key) {
        anyhow::bail!("{NO_PRIMARY_KEY}");
    }
    let (qs, qe) = quote_chars(db);
    let deletes = changes
        .deletes
        .iter()
        .map(|values| (build_delete_query(table, columns, values, qs, qe), Some(1)));
    let updates = changes.updates.iter().filter_map(|(original, new)| {
        build_update_query(table, columns, original, new, qs, qe).map(|sql| (sql, Some(1)))
    });
    let inserts = changes.inserts.iter().filter_map(|values| {
        build_insert_query(table, columns, values, system_columns, qs, qe).map(|sql| (sql, None))
    });
    let (statements, expected): (Vec<String>, Vec<Option<u64>>) =
        deletes.chain(updates).chain(inserts).unzip();
    if statements.is_empty() {
        return Ok(0);
    }
    conn.execute_checked_batch(&statements, &expected).await?;
    Ok(statements.len())
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
        let shown = display_qualified(table);
        progress(i, tables.len(), &shown);
        match conn.execute_query(&format!("DELETE FROM {table}")).await {
            Ok(r) => {
                report.succeeded += 1;
                report.rows_affected += r.rows_affected;
            }
            Err(e) => report.errors.push(format!("{shown}: {e}")),
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
    use crate::engine::models::{Column, NULL_CELL};
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
            &["t".into(), "\"main\".\"missing\"".into(), "u".into()],
            |_, _, _| {},
        )
        .await;
        assert_eq!(report.total, 3);
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.rows_affected, 5);
        assert_eq!(count(&conn, "t").await, "0");
        assert_eq!(count(&conn, "u").await, "0");
        assert_eq!(report.errors.len(), 1);
        // Shown to the user: unquoted.
        assert!(report.errors[0].starts_with("main.missing:"));
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
            inserts: vec![row("", NULL_CELL)], // violates NOT NULL
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

    /// Names of `t`, ordered.
    async fn names(conn: &DatabaseConnection) -> Vec<String> {
        let r = conn
            .execute_query("SELECT name FROM t ORDER BY id")
            .await
            .unwrap();
        r.rows.into_iter().map(|r| r[0].clone()).collect()
    }

    #[tokio::test]
    async fn submit_rolls_back_when_a_row_is_no_longer_there() {
        let conn = sqlite_mem(SETUP).await;
        let changes = RowChanges {
            updates: vec![
                (row("1", "a"), row("1", "A")),
                // Deleted (or its key changed) since it was loaded.
                (row("7", "x"), row("7", "y")),
            ],
            inserts: vec![row("", "c")],
            deletes: vec![row("2", "b")],
        };
        let err = submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &changes)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("La ligne n'a pas été trouvée ou n'est pas unique (0 ligne)"),
            "{err}"
        );
        assert!(err.contains("modifications annulées"), "{err}");
        assert_eq!(names(&conn).await, ["a", "b"], "everything rolled back");
        // The connection is usable (no transaction left open).
        update_row(&conn, "t", &cols(), &row("1", "a"), &row("1", "z"))
            .await
            .unwrap();
        assert_eq!(names(&conn).await, ["z", "b"]);
    }

    #[tokio::test]
    async fn submit_rolls_back_when_a_key_matches_several_rows() {
        // `k` is presented as the key but the table does not enforce it.
        let conn = sqlite_mem(&[
            "CREATE TABLE d (k INTEGER, v TEXT)",
            "INSERT INTO d VALUES (1, 'a'), (1, 'a'), (2, 'b')",
        ])
        .await;
        let columns: Vec<Column> = ["k", "v"]
            .iter()
            .map(|n| Column {
                name: n.to_string(),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: *n == "k",
            })
            .collect();
        for changes in [
            RowChanges {
                updates: vec![(row("1", "a"), row("1", "z"))],
                ..Default::default()
            },
            RowChanges {
                deletes: vec![row("2", "b"), row("1", "a")],
                ..Default::default()
            },
        ] {
            let err = submit_changes(&conn, &DatabaseType::SQLite, "d", &columns, &[], &changes)
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains("n'est pas unique (2 lignes)"), "{err}");
            let r = conn
                .execute_query("SELECT k, v FROM d ORDER BY k, v")
                .await
                .unwrap();
            assert_eq!(r.rows.len(), 3, "nothing deleted");
            assert!(r.rows.iter().all(|r| r[1] != "z"), "nothing updated");
        }
        // Single-row helpers are checked too.
        assert!(
            update_row(&conn, "d", &columns, &row("1", "a"), &row("1", "z"))
                .await
                .is_err()
        );
        assert!(delete_row(&conn, "d", &columns, &row("1", "a"), ('"', '"'))
            .await
            .is_err());
        assert_eq!(count(&conn, "d").await, "3");
    }

    #[tokio::test]
    async fn submit_refuses_a_table_without_primary_key() {
        let conn = sqlite_mem(&[
            "CREATE TABLE n (a TEXT, b TEXT)",
            "INSERT INTO n VALUES ('x', 'y')",
        ])
        .await;
        let columns: Vec<Column> = ["a", "b"]
            .iter()
            .map(|n| Column {
                name: n.to_string(),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: false,
            })
            .collect();
        let changes = RowChanges {
            updates: vec![(row("x", "y"), row("x", "z"))],
            inserts: vec![row("p", "q")],
            deletes: vec![],
        };
        let err = submit_changes(&conn, &DatabaseType::SQLite, "n", &columns, &[], &changes)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("pas de clé primaire"), "{err}");
        let r = conn.execute_query("SELECT a, b FROM n").await.unwrap();
        assert_eq!(r.rows, vec![vec!["x".to_string(), "y".to_string()]]);
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

    /// The text "NULL", the empty string and NULL are three different values.
    #[tokio::test]
    async fn null_text_and_empty_string_round_trip() {
        let conn = sqlite_mem(SETUP).await;
        let changes = RowChanges {
            updates: vec![
                (row("1", "a"), row("1", "NULL")),
                (row("2", "b"), row("2", NULL_CELL)),
            ],
            inserts: vec![row("3", ""), row("4", NULL_CELL)],
            deletes: vec![],
        };
        submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[], &changes)
            .await
            .unwrap();
        let r = conn
            .execute_query("SELECT name, name IS NULL FROM t ORDER BY id")
            .await
            .unwrap();
        let got: Vec<(&str, &str)> = r
            .rows
            .iter()
            .map(|r| (r[0].as_str(), r[1].as_str()))
            .collect();
        assert_eq!(
            got,
            [("NULL", "0"), (NULL_CELL, "1"), ("", "0"), (NULL_CELL, "1")]
        );
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
