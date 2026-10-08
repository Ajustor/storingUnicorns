use anyhow::Result;

use crate::engine::db::{utils::build_delete_query, DatabaseConnection};
use crate::engine::models::Column;

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
