use std::path::Path;

use anyhow::{anyhow, Result};

use crate::engine::db::DatabaseConnection;
use crate::engine::ops::rows::BatchReport;
use crate::engine::services::export_import::{
    build_upsert_import_actions, export_to_file, parse_csv, BatchExportState, ExportFormat,
    ImportAction,
};

/// Outcome of a single-table CSV import.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportStats {
    pub total: usize,
    pub updated: usize,
    pub inserted: usize,
    pub errors: Vec<String>,
}

impl ImportStats {
    pub fn succeeded(&self) -> usize {
        self.updated + self.inserted
    }
}

/// Run one import action: UPDATE then INSERT when nothing matched (upsert),
/// or a plain INSERT. Returns `true` when the row was updated.
async fn apply_action(conn: &DatabaseConnection, action: &ImportAction) -> Result<bool> {
    match action {
        ImportAction::Upsert {
            update_query,
            insert_query,
        } => {
            if conn.execute_query(update_query).await?.rows_affected > 0 {
                Ok(true)
            } else {
                conn.execute_query(insert_query).await?;
                Ok(false)
            }
        }
        ImportAction::InsertOnly { query } => {
            conn.execute_query(query).await?;
            Ok(false)
        }
    }
}

/// Import CSV `content` into `table`, row by row. Row failures are collected,
/// not fatal. `progress(done, total)` is called after each row.
pub async fn import_csv(
    conn: &DatabaseConnection,
    table: &str,
    content: &str,
    quotes: (char, char),
    mut progress: impl FnMut(usize, usize),
) -> Result<ImportStats> {
    let (columns, rows) = parse_csv(content).map_err(|e| anyhow!("CSV parse error: {e}"))?;
    let actions = build_upsert_import_actions(table, &columns, &rows, quotes.0, quotes.1);
    let mut stats = ImportStats {
        total: actions.len(),
        ..Default::default()
    };
    for (i, action) in actions.iter().enumerate() {
        match apply_action(conn, action).await {
            Ok(true) => stats.updated += 1,
            Ok(false) => stats.inserted += 1,
            Err(e) => stats.errors.push(e.to_string()),
        }
        progress(i + 1, stats.total);
    }
    Ok(stats)
}

/// `"schema"."table"` with the database's identifier quotes.
pub fn qualified(schema: &str, table: &str, (q0, q1): (char, char)) -> String {
    format!("{q0}{schema}{q1}.{q0}{table}{q1}")
}

/// Export each `(schema, table)` to `<dir>/<table>.<ext>`. `dir` must exist:
/// the caller creates it so it can report that failure on its own.
/// `progress(done, total, table)` is called before each table.
pub async fn export_tables(
    conn: &DatabaseConnection,
    tables: &[(String, String)],
    dir: &Path,
    format: ExportFormat,
    quotes: (char, char),
    mut progress: impl FnMut(usize, usize, &str),
) -> BatchReport {
    let mut report = BatchReport {
        total: tables.len(),
        ..Default::default()
    };
    for (i, (schema, table)) in tables.iter().enumerate() {
        progress(i, tables.len(), table);
        let full = qualified(schema, table, quotes);
        let file = dir.join(format!(
            "{}.{}",
            BatchExportState::clean_table_name(table),
            format.extension()
        ));
        let outcome = match conn.execute_query(&format!("SELECT * FROM {full}")).await {
            Ok(result) => export_to_file(
                &result,
                format,
                &file.to_string_lossy(),
                &full,
                quotes.0,
                quotes.1,
            )
            .map_err(|e| anyhow!(e)),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(rows) => {
                report.succeeded += 1;
                report.rows_affected += rows as u64;
            }
            Err(e) => report.errors.push(format!("{table}: {e}")),
        }
    }
    report
}

/// Import `<dir>/<table>.csv` into each `(schema, table)`. A table stops at
/// its first failing row and counts as failed. `progress(done, total, table)`
/// is called before each table.
pub async fn import_tables(
    conn: &DatabaseConnection,
    tables: &[(String, String)],
    dir: &Path,
    quotes: (char, char),
    mut progress: impl FnMut(usize, usize, &str),
) -> BatchReport {
    let mut report = BatchReport {
        total: tables.len(),
        ..Default::default()
    };
    'tables: for (i, (schema, table)) in tables.iter().enumerate() {
        progress(i, tables.len(), table);
        let path = dir.join(format!("{}.csv", BatchExportState::clean_table_name(table)));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                report.errors.push(format!("{table}: {e}"));
                continue;
            }
        };
        let (columns, rows) = match parse_csv(&content) {
            Ok(d) => d,
            Err(e) => {
                report.errors.push(format!("{table}: CSV parse error: {e}"));
                continue;
            }
        };
        let full = qualified(schema, table, quotes);
        for action in build_upsert_import_actions(&full, &columns, &rows, quotes.0, quotes.1) {
            match apply_action(conn, &action).await {
                Ok(_) => report.rows_affected += 1,
                Err(e) => {
                    report.errors.push(format!("{table}: {e}"));
                    continue 'tables;
                }
            }
        }
        report.succeeded += 1;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ops::test_support::{count, sqlite_mem};
    use crate::engine::services::export_import::ExportFormat;

    const SETUP: &[&str] = &[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b')",
    ];

    #[tokio::test]
    async fn import_csv_upserts_by_id() {
        let conn = sqlite_mem(SETUP).await;
        let csv = "id,name\n1,z\n,new\n";
        let stats = import_csv(&conn, "t", csv, ('"', '"'), |_, _| {})
            .await
            .unwrap();
        assert_eq!(
            stats,
            ImportStats {
                total: 2,
                updated: 1,
                inserted: 1,
                errors: vec![]
            }
        );
        assert_eq!(count(&conn, "t").await, "3");
    }

    #[tokio::test]
    async fn import_csv_rejects_empty_file() {
        let conn = sqlite_mem(SETUP).await;
        assert!(import_csv(&conn, "t", "", ('"', '"'), |_, _| {})
            .await
            .is_err());
    }

    #[tokio::test]
    async fn export_then_import_tables_roundtrip() {
        let conn = sqlite_mem(SETUP).await;
        let dir = tempfile::tempdir().unwrap();
        let tables = vec![("main".to_string(), "t".to_string())];
        let report = export_tables(
            &conn,
            &tables,
            dir.path(),
            ExportFormat::Csv,
            ('"', '"'),
            |_, _, _| {},
        )
        .await;
        assert_eq!(report.succeeded, 1, "{:?}", report.errors);
        assert!(dir.path().join("t.csv").exists());

        conn.execute_query("DELETE FROM t").await.unwrap();
        let report = import_tables(&conn, &tables, dir.path(), ('"', '"'), |_, _, _| {}).await;
        assert_eq!(report.succeeded, 1, "{:?}", report.errors);
        assert_eq!(report.rows_affected, 2);
        assert_eq!(count(&conn, "t").await, "2");
    }
}
