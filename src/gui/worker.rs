//! Runs database work off the UI thread. Each operation is spawned on a tokio
//! runtime; its outcome comes back as an `Event` tagged with the connection
//! name and/or tab id it belongs to, drained by `poll()` every frame.
//! `repaint` wakes the UI when an event is ready.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{
    Column, ConnectionConfig, DatabaseType, QueryResult, SchemaInfo, TableDetails,
};
use crate::engine::ops::{
    self,
    query::StatementOutcome,
    rows::{BatchReport, RowChanges},
    transfer::ImportStats,
};
use crate::engine::services::export_import::{export_to_file, ExportFormat};
use crate::engine::services::table_cache::TableCache;
use crate::engine::services::SchemaModification;

pub type Conn = Arc<DatabaseConnection>;
pub type TabId = u64;
/// Identifies one operation started for a tab. A replaced or cancelled run
/// may still deliver its event; the tab drops events of runs it no longer
/// waits for.
pub type RunId = u64;

/// Result of a background operation. Errors are carried as display strings.
pub enum Event {
    Connected {
        name: String,
        conn: Conn,
        schemas: Vec<SchemaInfo>,
    },
    ConnectFailed {
        name: String,
        error: String,
    },
    TestFinished(Result<(), String>),
    Schemas {
        name: String,
        outcome: Result<Vec<SchemaInfo>, String>,
    },
    Details {
        name: String,
        table: String,
        outcome: Result<TableDetails, String>,
    },
    /// Console execution, one outcome per executed unit.
    Script {
        tab: TabId,
        run: RunId,
        outcomes: Vec<StatementOutcome>,
    },
    /// One page of a data editor.
    Page {
        tab: TabId,
        run: RunId,
        outcome: Result<QueryResult, String>,
    },
    Count {
        tab: TabId,
        run: RunId,
        outcome: Result<u64, String>,
    },
    /// Number of statements applied by a Submit.
    Submitted {
        tab: TabId,
        run: RunId,
        outcome: Result<usize, String>,
    },
    Ddl {
        tab: TabId,
        run: RunId,
        outcome: Result<String, String>,
    },
    /// `outcome` is the SQL that ran.
    SchemaApplied {
        name: String,
        table: String,
        outcome: Result<String, String>,
    },
    /// Rows written and the file path.
    Exported(Result<(usize, PathBuf), String>),
    Imported {
        name: String,
        table: String,
        outcome: Result<ImportStats, String>,
    },
    /// `kind` is "Export", "Import" or "Vidage".
    Batch {
        name: String,
        kind: &'static str,
        report: BatchReport,
    },
    Progress {
        done: usize,
        total: usize,
        label: String,
    },
}

impl Event {
    /// Tab an outcome belongs to (`Script`, `Page`, `Count`, `Submitted`, `Ddl`).
    pub fn tab(&self) -> Option<TabId> {
        match self {
            Event::Script { tab, .. }
            | Event::Page { tab, .. }
            | Event::Count { tab, .. }
            | Event::Submitted { tab, .. }
            | Event::Ddl { tab, .. } => Some(*tab),
            _ => None,
        }
    }
}

pub struct Worker {
    rt: Runtime,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    /// Cancellable work per tab (console executions, data pages).
    running: HashMap<TabId, JoinHandle<()>>,
    /// Column metadata cache per connection name.
    caches: HashMap<String, Arc<TableCache>>,
    /// Last run id handed out (monotonic).
    last_run: RunId,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// First cell of a `COUNT(*)` result.
fn parse_count(result: &QueryResult) -> Result<u64, String> {
    let cell = result
        .rows
        .first()
        .and_then(|row| row.first())
        .ok_or_else(|| "COUNT returned no rows".to_string())?;
    cell.trim()
        .parse()
        .map_err(|_| format!("COUNT returned a non-numeric value: {cell}"))
}

impl Worker {
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            rt: Runtime::new().expect("tokio runtime"),
            tx,
            rx,
            repaint: Arc::new(repaint),
            running: HashMap::new(),
            caches: HashMap::new(),
            last_run: 0,
        }
    }

    /// Events ready since the last call.
    pub fn poll(&mut self) -> Vec<Event> {
        self.running.retain(|_, h| !h.is_finished());
        self.rx.try_iter().collect()
    }

    #[allow(dead_code)] // console and data tabs (plan 3b, Tasks 6 and 8)
    pub fn is_running(&self, tab: TabId) -> bool {
        self.running.get(&tab).is_some_and(|h| !h.is_finished())
    }

    #[allow(dead_code)] // console and data tabs (plan 3b, Tasks 6 and 8)
    pub fn any_running(&self) -> bool {
        self.running.values().any(|h| !h.is_finished())
    }

    /// Abort the work running in `tab`. Returns whether something was
    /// cancelled. Dropping a transaction's dedicated connection without
    /// COMMIT makes the database roll it back.
    pub fn cancel(&mut self, tab: TabId) -> bool {
        match self.running.remove(&tab) {
            Some(h) if !h.is_finished() => {
                h.abort();
                true
            }
            _ => false,
        }
    }

    fn next_run(&mut self) -> RunId {
        self.last_run += 1;
        self.last_run
    }

    /// Column cache of connection `name`.
    pub fn cache_for(&mut self, name: &str) -> Arc<TableCache> {
        self.caches.entry(name.to_string()).or_default().clone()
    }

    /// Drop the cached metadata of connection `name` (on disconnect).
    pub fn forget(&mut self, name: &str) {
        self.caches.remove(name);
    }

    fn spawn<F>(&self, fut: F) -> JoinHandle<()>
    where
        F: Future<Output = Event> + Send + 'static,
    {
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
            repaint();
        })
    }

    /// Spawn cancellable work for `tab`, replacing (aborting) the previous one.
    fn spawn_for_tab<F>(&mut self, tab: TabId, fut: F)
    where
        F: Future<Output = Event> + Send + 'static,
    {
        self.cancel(tab);
        let handle = self.spawn(fut);
        self.running.insert(tab, handle);
    }

    /// Progress callback usable from inside a spawned task.
    fn progress(&self) -> impl FnMut(usize, usize, &str) + Send + 'static {
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        move |done, total, label| {
            let _ = tx.send(Event::Progress {
                done,
                total,
                label: label.to_string(),
            });
            repaint();
        }
    }

    pub fn connect(&mut self, config: ConnectionConfig) {
        self.forget(&config.name);
        let tx = self.tx.clone();
        self.spawn(async move {
            let name = config.name.clone();
            match DatabaseConnection::connect(&config).await {
                Ok(conn) => {
                    let schemas = match ops::query::refresh_schemas(&conn).await {
                        Ok(schemas) => schemas,
                        Err(e) => {
                            // Connected all the same; report the listing failure.
                            let _ = tx.send(Event::Schemas {
                                name: name.clone(),
                                outcome: Err(err(e)),
                            });
                            Vec::new()
                        }
                    };
                    Event::Connected {
                        name,
                        conn: Arc::new(conn),
                        schemas,
                    }
                }
                Err(e) => Event::ConnectFailed {
                    name,
                    error: err(e),
                },
            }
        });
    }

    pub fn test_connection(&self, config: ConnectionConfig) {
        self.spawn(async move {
            let outcome = match DatabaseConnection::connect(&config).await {
                Ok(conn) => {
                    let r = conn.test().await.map_err(err);
                    conn.close().await;
                    r
                }
                Err(e) => Err(err(e)),
            };
            Event::TestFinished(outcome)
        });
    }

    pub fn refresh_schemas(&self, name: String, conn: Conn) {
        self.spawn(async move {
            let outcome = ops::query::refresh_schemas(&conn).await.map_err(err);
            Event::Schemas { name, outcome }
        });
    }

    /// Columns, indexes and foreign keys of `table` on connection `name`.
    pub fn table_details(&mut self, name: String, conn: Conn, table: String) {
        let cache = self.cache_for(&name);
        self.spawn(async move {
            let outcome = ops::schema::table_details(&conn, &cache, &table)
                .await
                .map_err(err);
            Event::Details {
                name,
                table,
                outcome,
            }
        });
    }

    /// Execute every statement of `text` (F5 / selection).
    pub fn run_script(
        &mut self,
        tab: TabId,
        conn: Conn,
        text: String,
        max_rows: Option<usize>,
    ) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcomes = ops::query::run_script(&conn, &text, max_rows).await;
            Event::Script { tab, run, outcomes }
        });
        run
    }

    /// Execute the statement or transaction block under `cursor` (byte offset).
    pub fn run_at_cursor(
        &mut self,
        tab: TabId,
        conn: Conn,
        text: String,
        cursor: usize,
        max_rows: Option<usize>,
    ) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcomes = ops::query::run_at_cursor_outcomes(&conn, &text, cursor, max_rows).await;
            Event::Script { tab, run, outcomes }
        });
        run
    }

    /// Load one page of a data editor; replaces the tab's pending load.
    pub fn load_page(&mut self, tab: TabId, conn: Conn, sql: String) -> RunId {
        let run = self.next_run();
        self.spawn_for_tab(tab, async move {
            let outcome = ops::query::run_query(&conn, &sql).await.map_err(err);
            Event::Page { tab, run, outcome }
        });
        run
    }

    /// Run a `COUNT(*)` query and parse its first cell.
    pub fn count(&mut self, tab: TabId, conn: Conn, sql: String) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = match conn.execute_query(&sql).await {
                Ok(r) => parse_count(&r),
                Err(e) => Err(err(e)),
            };
            Event::Count { tab, run, outcome }
        });
        run
    }

    /// Apply a data editor's or console result's pending changes in one
    /// transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &mut self,
        tab: TabId,
        conn: Conn,
        db_type: DatabaseType,
        table: String,
        columns: Vec<Column>,
        system_columns: Vec<usize>,
        changes: RowChanges,
    ) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let outcome = ops::rows::submit_changes(
                &conn,
                &db_type,
                &table,
                &columns,
                &system_columns,
                &changes,
            )
            .await
            .map_err(err);
            Event::Submitted { tab, run, outcome }
        });
        run
    }

    /// `CREATE TABLE` of `table`, from fresh metadata.
    pub fn ddl(&mut self, tab: TabId, conn: Conn, db_type: DatabaseType, table: String) -> RunId {
        let run = self.next_run();
        self.spawn(async move {
            let cache = TableCache::default();
            let outcome = ops::schema::table_ddl(&conn, &cache, &db_type, &table)
                .await
                .map_err(err);
            Event::Ddl { tab, run, outcome }
        });
        run
    }

    pub fn apply_schema(
        &mut self,
        name: String,
        conn: Conn,
        m: SchemaModification,
        db_type: DatabaseType,
    ) {
        let cache = self.cache_for(&name);
        self.spawn(async move {
            let table = ops::schema::table_of(&m).unwrap_or_default().to_string();
            let outcome = ops::schema::apply_modification(&conn, &cache, &m, &db_type)
                .await
                .map_err(err);
            Event::SchemaApplied {
                name,
                table,
                outcome,
            }
        });
    }

    pub fn export_result(
        &self,
        result: QueryResult,
        format: ExportFormat,
        path: PathBuf,
        table: String,
        quotes: (char, char),
    ) {
        self.spawn(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                export_to_file(
                    &result,
                    format,
                    &path.to_string_lossy(),
                    &table,
                    quotes.0,
                    quotes.1,
                )
                .map(|n| (n, path))
            })
            .await
            .map_err(err)
            .and_then(|r| r);
            Event::Exported(outcome)
        });
    }

    pub fn import_csv(
        &self,
        name: String,
        conn: Conn,
        table: String,
        path: PathBuf,
        quotes: (char, char),
    ) {
        let mut progress = self.progress();
        self.spawn(async move {
            let outcome = match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    ops::transfer::import_csv(&conn, &table, &content, quotes, |d, t| {
                        progress(d, t, "lignes")
                    })
                    .await
                    .map_err(err)
                }
                Err(e) => Err(format!("{}: {e}", path.display())),
            };
            Event::Imported {
                name,
                table,
                outcome,
            }
        });
    }

    /// Export each `(schema, table)` to `dir`, created if missing.
    pub fn export_tables(
        &self,
        name: String,
        conn: Conn,
        tables: Vec<(String, String)>,
        dir: PathBuf,
        format: ExportFormat,
        quotes: (char, char),
    ) {
        let progress = self.progress();
        self.spawn(async move {
            let report = match tokio::fs::create_dir_all(&dir).await {
                Ok(()) => {
                    ops::transfer::export_tables(&conn, &tables, &dir, format, quotes, progress)
                        .await
                }
                Err(e) => BatchReport {
                    total: tables.len(),
                    errors: vec![format!("{}: {e}", dir.display())],
                    ..Default::default()
                },
            };
            Event::Batch {
                name,
                kind: "Export",
                report,
            }
        });
    }

    pub fn import_tables(
        &self,
        name: String,
        conn: Conn,
        tables: Vec<(String, String)>,
        dir: PathBuf,
        quotes: (char, char),
    ) {
        let progress = self.progress();
        self.spawn(async move {
            let report = ops::transfer::import_tables(&conn, &tables, &dir, quotes, progress).await;
            Event::Batch {
                name,
                kind: "Import",
                report,
            }
        });
    }

    /// `DELETE FROM` each (already quoted) table.
    pub fn truncate_tables(&self, name: String, conn: Conn, tables: Vec<String>) {
        let progress = self.progress();
        self.spawn(async move {
            let report = ops::rows::truncate_tables(&conn, &tables, progress).await;
            Event::Batch {
                name,
                kind: "Vidage",
                report,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{ConnectionConfig, DatabaseType};
    use crate::engine::ops::rows::RowChanges;
    use std::time::{Duration, Instant};

    fn sqlite_config(dir: &tempfile::TempDir) -> ConnectionConfig {
        let path = dir.path().join("test.db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rwc", path.display()))
                .await
                .unwrap();
            sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO t (name) VALUES ('a'), ('b')")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        });
        ConnectionConfig {
            name: "test".into(),
            db_type: DatabaseType::SQLite,
            database: path.display().to_string(),
            ..Default::default()
        }
    }

    fn next_event(w: &mut Worker) -> Event {
        let start = Instant::now();
        loop {
            if let Some(ev) = w.poll().into_iter().next() {
                return ev;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "no event");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Connect to a fresh database and return the connection.
    fn connected(w: &mut Worker, dir: &tempfile::TempDir) -> Conn {
        w.connect(sqlite_config(dir));
        let Event::Connected {
            name,
            conn,
            schemas,
        } = next_event(w)
        else {
            panic!("expected Connected")
        };
        assert_eq!(name, "test");
        assert!(schemas.iter().any(|s| s.tables.iter().any(|t| t == "t")));
        conn
    }

    /// `is_running` turns false right after the task sent its event.
    fn wait_idle(w: &Worker, tab: TabId) {
        let start = Instant::now();
        while w.is_running(tab) {
            assert!(start.elapsed() < Duration::from_secs(10), "still running");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn connect_then_script() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Worker::new(|| {});
        let conn = connected(&mut w, &dir);

        let id = w.run_script(1, conn, "SELECT * FROM t".into(), Some(1000));
        assert!(w.is_running(1));
        assert!(w.any_running());
        let Event::Script { tab, run, outcomes } = next_event(&mut w) else {
            panic!("expected Script")
        };
        assert_eq!((tab, run), (1, id));
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].result.as_ref().unwrap().rows.len(), 2);
        wait_idle(&w, 1);
        assert!(!w.any_running());
    }

    #[test]
    fn run_at_cursor_runs_one_statement() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Worker::new(|| {});
        let conn = connected(&mut w, &dir);

        let text = "SELECT 1;\nSELECT * FROM t;";
        let id = w.run_at_cursor(3, conn, text.into(), text.len() - 3, None);
        let Event::Script { tab, run, outcomes } = next_event(&mut w) else {
            panic!("expected Script")
        };
        assert_eq!((tab, run), (3, id));
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].result.as_ref().unwrap().rows.len(), 2);
    }

    #[test]
    fn connect_failure_is_reported() {
        let mut w = Worker::new(|| {});
        w.connect(ConnectionConfig {
            name: "bad".into(),
            db_type: DatabaseType::SQLite,
            database: "/definitely/not/here/x.db".into(),
            ..Default::default()
        });
        let Event::ConnectFailed { name, error } = next_event(&mut w) else {
            panic!("expected ConnectFailed")
        };
        assert_eq!(name, "bad");
        assert!(!error.is_empty());
    }

    #[test]
    fn cancel_aborts_running_query() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Worker::new(|| {});
        let conn = connected(&mut w, &dir);
        // A recursive CTE that runs for a long time.
        let slow = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c \
                    WHERE x < 1000000000) SELECT count(*) FROM c";
        w.run_script(1, conn, slow.into(), None);
        assert!(w.is_running(1));
        assert!(!w.cancel(2), "nothing runs in tab 2");
        assert!(w.cancel(1));
        assert!(!w.is_running(1));
        assert!(!w.cancel(1));
    }

    #[test]
    fn page_count_and_submit_events_carry_tab_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Worker::new(|| {});
        let conn = connected(&mut w, &dir);

        let id = w.load_page(7, conn.clone(), "SELECT * FROM t LIMIT 500 OFFSET 0".into());
        let Event::Page { tab, run, outcome } = next_event(&mut w) else {
            panic!("expected Page")
        };
        assert_eq!((tab, run), (7, id));
        let r = outcome.unwrap();
        assert_eq!(r.rows.len(), 2);
        assert!(r.columns[0].is_primary_key, "page results are enriched");

        let id = w.count(7, conn.clone(), "SELECT COUNT(*) FROM t".into());
        let Event::Count { tab, run, outcome } = next_event(&mut w) else {
            panic!("expected Count")
        };
        assert_eq!((tab, run), (7, id));
        assert_eq!(outcome, Ok(2));

        let changes = RowChanges {
            deletes: vec![r.rows[0].clone()],
            ..Default::default()
        };
        let id = w.submit(
            7,
            conn.clone(),
            DatabaseType::SQLite,
            "t".into(),
            r.columns.clone(),
            vec![0],
            changes,
        );
        let Event::Submitted { tab, run, outcome } = next_event(&mut w) else {
            panic!("expected Submitted")
        };
        assert_eq!((tab, run), (7, id));
        assert_eq!(outcome, Ok(1));

        w.count(7, conn, "SELECT COUNT(*) FROM t".into());
        let Event::Count { outcome, .. } = next_event(&mut w) else {
            panic!("expected Count")
        };
        assert_eq!(outcome, Ok(1));
    }

    #[test]
    fn details_and_ddl() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Worker::new(|| {});
        let conn = connected(&mut w, &dir);

        w.table_details("test".into(), conn.clone(), "t".into());
        let Event::Details {
            name,
            table,
            outcome,
        } = next_event(&mut w)
        else {
            panic!("expected Details")
        };
        assert_eq!((name.as_str(), table.as_str()), ("test", "t"));
        assert_eq!(outcome.unwrap().columns.len(), 2);

        let id = w.ddl(4, conn, DatabaseType::SQLite, "t".into());
        let Event::Ddl { tab, run, outcome } = next_event(&mut w) else {
            panic!("expected Ddl")
        };
        assert_eq!((tab, run), (4, id));
        assert!(outcome.unwrap().contains("CREATE TABLE t"));
    }

    #[test]
    fn superseded_page_load_is_identifiable() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Worker::new(|| {});
        let conn = connected(&mut w, &dir);

        let first = w.load_page(7, conn.clone(), "SELECT * FROM t".into());
        let second = w.load_page(7, conn, "SELECT name FROM t".into());
        assert_ne!(first, second, "each operation gets its own run id");
        // The first load may or may not have delivered before being aborted;
        // whatever arrives, only `second` is the run the tab waits for.
        loop {
            let Event::Page { tab, run, outcome } = next_event(&mut w) else {
                panic!("expected Page")
            };
            assert_eq!(tab, 7);
            if run == second {
                assert_eq!(outcome.unwrap().columns.len(), 1);
                break;
            }
            assert_eq!(run, first, "unexpected run id");
        }
    }
}
