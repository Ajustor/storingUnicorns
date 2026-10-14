//! Manual transaction mode (DataGrip's "Tx: Manual"): the statements run from
//! the consoles and the Submits of the grids of a connection go to one
//! dedicated database session, in a transaction left open until the user
//! commits or rolls it back.
//!
//! The transaction begins with the first statement run after the previous
//! one ended. `BEGIN` / `COMMIT` / `ROLLBACK`… typed by the user are honoured
//! and tracked. When the session is lost (network, interrupted SQL Server
//! query…) the server rolled the transaction back: that is detected before
//! the next statement and reported, instead of carrying on outside a
//! transaction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Result};
use tokio::sync::Mutex;

use crate::engine::db::utils::{row_count_error, transaction_bounds};
use crate::engine::db::DatabaseConnection;
use crate::engine::models::{Column, ConnectionConfig, DatabaseType, QueryResult};
use crate::engine::sql::statements::{
    get_execution_unit_at_cursor, is_transaction_end, is_transaction_start, split_statements,
    ExecutionUnit,
};

use super::query::{enrich, StatementOutcome};
use super::rows::{submit_statements, RowChanges};

pub const SESSION_LOST: &str =
    "Connexion perdue : le serveur a annulé la transaction en cours, rien n'a été validé";
const ENDED_BY_SERVER: &str =
    "Le serveur a annulé la transaction : les instructions précédentes ne sont pas appliquées";
const PG_FAILED: &str = "une instruction a échoué, PostgreSQL n'accepte plus que ROLLBACK";

/// What the UI shows of a manual transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TxStatus {
    /// A transaction is open.
    pub open: bool,
    /// Statements run in it (console statements, Submit statements).
    pub statements: usize,
    /// PostgreSQL: a statement failed, the transaction can only roll back.
    pub failed: bool,
}

#[derive(Default)]
struct State {
    /// `opened` when the open transaction began; `None`: no transaction.
    began_on: Option<u64>,
    statements: usize,
    failed: bool,
}

pub struct ManualTx {
    /// The dedicated session.
    conn: Arc<DatabaseConnection>,
    /// The connection's shared pool, for metadata: a failing metadata query
    /// must not abort the transaction (PostgreSQL).
    meta: Arc<DatabaseConnection>,
    /// Sessions `conn` opened (see `DatabaseConnection::connect_dedicated`).
    opened: Arc<AtomicU64>,
    /// Held by each operation for its whole run: one at a time.
    state: Mutex<State>,
    /// Copy of `state` for the UI, updated at the end of each operation.
    status: StdMutex<TxStatus>,
}

/// SQL starting a transaction. MySQL turns autocommit off instead: a DDL
/// statement commits implicitly, and `START TRANSACTION` would then leave the
/// next statements in autocommit.
fn begin_sql(db: &DatabaseType) -> &'static str {
    match db {
        DatabaseType::MySQL => "SET autocommit = 0",
        _ => transaction_bounds(db).0,
    }
}

/// `(create, roll back to, release)` a savepoint, for a Submit.
fn savepoint(db: &DatabaseType) -> (&'static str, &'static str, Option<&'static str>) {
    match db {
        DatabaseType::SQLServer | DatabaseType::Azure => (
            "SAVE TRANSACTION su_submit",
            "ROLLBACK TRANSACTION su_submit",
            None,
        ),
        _ => (
            "SAVEPOINT su_submit",
            "ROLLBACK TO SAVEPOINT su_submit",
            Some("RELEASE SAVEPOINT su_submit"),
        ),
    }
}

/// `ROLLBACK … TO [SAVEPOINT] x`: ends the failure of a PostgreSQL transaction.
fn is_rollback_to(sql: &str) -> bool {
    let mut words = sql.split_whitespace().map(str::to_uppercase);
    words.next().as_deref() == Some("ROLLBACK") && words.take(2).any(|w| w == "TO")
}

/// What Ctrl+Enter runs in manual mode: the statement or transaction block
/// under the cursor, or only the statement under the cursor when it is in a
/// `BEGIN` not ended yet (the usual case in this mode).
pub fn statements_at_cursor(text: &str, cursor: usize) -> Vec<String> {
    match get_execution_unit_at_cursor(text, cursor) {
        ExecutionUnit::Single(sql) if sql.trim().is_empty() => Vec::new(),
        ExecutionUnit::Single(sql) => vec![sql],
        ExecutionUnit::Transaction(statements) => statements,
        ExecutionUnit::UnterminatedTransaction => {
            let all = split_statements(text);
            let i = all
                .iter()
                .position(|(_, end, _)| cursor <= *end)
                .unwrap_or(all.len().saturating_sub(1));
            all.into_iter().nth(i).map(|s| s.2).into_iter().collect()
        }
    }
}

impl ManualTx {
    /// Open a dedicated session to `config`; `meta` is the connection's
    /// shared pool.
    pub async fn open(config: &ConnectionConfig, meta: Arc<DatabaseConnection>) -> Result<Self> {
        let opened = Arc::new(AtomicU64::new(0));
        let conn = DatabaseConnection::connect_dedicated(config, opened.clone()).await?;
        Ok(Self {
            conn: Arc::new(conn),
            meta,
            opened,
            state: Mutex::default(),
            status: StdMutex::default(),
        })
    }

    /// The dedicated session: reads through it see the pending changes.
    pub fn conn(&self) -> Arc<DatabaseConnection> {
        self.conn.clone()
    }

    pub fn meta(&self) -> Arc<DatabaseConnection> {
        self.meta.clone()
    }

    pub fn status(&self) -> TxStatus {
        *self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn publish(&self, st: &State) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = TxStatus {
            open: st.began_on.is_some(),
            statements: st.statements,
            failed: st.failed,
        };
    }

    fn sessions(&self) -> u64 {
        self.opened.load(Ordering::SeqCst)
    }

    fn db_type(&self) -> DatabaseType {
        self.conn.db_type()
    }

    /// Fail, forgetting the transaction, when its session was replaced.
    async fn check(&self, st: &mut State) -> Result<()> {
        let Some(began_on) = st.began_on else {
            return Ok(());
        };
        // A dead sqlx connection is replaced when next acquired, SQL
        // Server's interrupted client at its next use: probe to make either
        // happen now. Its own error (a failed PostgreSQL transaction) is
        // irrelevant here.
        let _ = self.conn.execute_query("SELECT 1").await;
        if self.sessions() != began_on {
            *st = State::default();
            bail!("{SESSION_LOST}");
        }
        Ok(())
    }

    /// Run `sql` on the session. Fails when the session was replaced while
    /// a transaction was open: `sql` then ran outside of it.
    async fn exec(
        &self,
        st: &mut State,
        sql: &str,
        max_rows: Option<usize>,
    ) -> Result<QueryResult> {
        let result = self.conn.execute_query_limited(sql, max_rows).await;
        if st.began_on.is_some_and(|b| b != self.sessions()) {
            *st = State::default();
            bail!("{SESSION_LOST} (cette instruction a été exécutée hors transaction)");
        }
        result
    }

    /// Note what the failure `e` of a statement did to the transaction.
    async fn after_error(&self, st: &mut State, e: anyhow::Error) -> anyhow::Error {
        if st.began_on.is_none() {
            return e;
        }
        let ended = match self.db_type() {
            DatabaseType::Postgres => {
                st.failed = true;
                false
            }
            // Some errors (deadlock, XACT_ABORT…) roll the transaction back.
            DatabaseType::SQLServer | DatabaseType::Azure => self
                .conn
                .execute_query("SELECT @@TRANCOUNT")
                .await
                .is_ok_and(|r| {
                    r.rows
                        .first()
                        .and_then(|r| r.first())
                        .is_some_and(|c| c == "0")
                }),
            DatabaseType::MySQL => format!("{e:#}").contains("Deadlock"),
            DatabaseType::SQLite => false,
        };
        if ended {
            *st = State::default();
            return anyhow!("{e}\n{ENDED_BY_SERVER}");
        }
        e
    }

    async fn begin(&self, st: &mut State, sql: &str) -> Result<QueryResult> {
        let result = self.exec(st, sql, None).await?;
        *st = State {
            began_on: Some(self.sessions()),
            ..State::default()
        };
        Ok(result)
    }

    async fn ensure_open(&self, st: &mut State) -> Result<()> {
        if st.began_on.is_some() {
            return self.check(st).await;
        }
        self.begin(st, begin_sql(&self.db_type())).await.map(drop)
    }

    /// End the open transaction with `sql` (`COMMIT`, `ROLLBACK`, `END`…).
    /// Returns the number of statements it held.
    async fn end(&self, st: &mut State, sql: &str) -> Result<usize> {
        self.check(st).await?;
        let rollback = sql.trim_start().to_uppercase().starts_with("ROLLBACK");
        let (statements, failed) = (st.statements, st.failed);
        match self.exec(st, sql, None).await {
            Ok(_) => {
                *st = State::default();
                if failed && !rollback {
                    // PostgreSQL answers COMMIT with a silent rollback.
                    bail!("Transaction annulée : {PG_FAILED}");
                }
                Ok(statements)
            }
            Err(e) => {
                // A failed COMMIT ends a PostgreSQL transaction.
                if self.db_type() == DatabaseType::Postgres {
                    *st = State::default();
                }
                Err(e)
            }
        }
    }

    /// One statement typed by the user.
    async fn statement(
        &self,
        st: &mut State,
        sql: &str,
        max_rows: Option<usize>,
    ) -> Result<QueryResult> {
        if is_transaction_start(sql) {
            if st.began_on.is_some() {
                // Already in one: nested BEGINs are not supported everywhere.
                self.check(st).await?;
                return Ok(QueryResult::default());
            }
            return self.begin(st, sql).await;
        }
        if is_transaction_end(sql) {
            if st.began_on.is_none() {
                return self.exec(st, sql, max_rows).await;
            }
            return self.end(st, sql).await.map(|_| QueryResult::default());
        }
        self.ensure_open(st).await?;
        match self.exec(st, sql, max_rows).await {
            Ok(mut result) => {
                st.statements += 1;
                if is_rollback_to(sql) {
                    st.failed = false;
                }
                enrich(&self.meta, sql, &mut result).await;
                Ok(result)
            }
            Err(e) => Err(self.after_error(st, e).await),
        }
    }

    /// Run `statements` in order, stopping after the first failure (its
    /// error is the last outcome).
    async fn run_statements(
        &self,
        statements: Vec<String>,
        max_rows: Option<usize>,
    ) -> Vec<StatementOutcome> {
        let mut st = self.state.lock().await;
        let mut outcomes = Vec::new();
        for sql in statements {
            let start = Instant::now();
            let result = self
                .statement(&mut st, &sql, max_rows)
                .await
                .map_err(|e| e.to_string());
            let failed = result.is_err();
            outcomes.push(StatementOutcome {
                sql,
                result,
                elapsed_ms: start.elapsed().as_millis(),
            });
            if failed {
                break;
            }
        }
        self.publish(&st);
        outcomes
    }

    /// Every statement of `text` (F5, selection).
    pub async fn run_script(&self, text: &str, max_rows: Option<usize>) -> Vec<StatementOutcome> {
        let statements = split_statements(text).into_iter().map(|s| s.2).collect();
        self.run_statements(statements, max_rows).await
    }

    /// The statement under `cursor` (`statements_at_cursor`).
    pub async fn run_at_cursor(
        &self,
        text: &str,
        cursor: usize,
        max_rows: Option<usize>,
    ) -> Vec<StatementOutcome> {
        let statements = statements_at_cursor(text, cursor);
        if statements.is_empty() {
            return vec![StatementOutcome {
                sql: String::new(),
                result: Err("No query at cursor position".into()),
                elapsed_ms: 0,
            }];
        }
        self.run_statements(statements, max_rows).await
    }

    /// Apply a grid's pending changes in the transaction, without
    /// committing. As in auto-commit, UPDATE and DELETE must affect exactly
    /// one row: otherwise (or on any error) the Submit is undone back to a
    /// savepoint, the rest of the transaction kept. Returns the number of
    /// statements applied.
    pub async fn submit(
        &self,
        table: &str,
        columns: &[Column],
        changes: &RowChanges,
    ) -> Result<usize> {
        let db = self.db_type();
        let (statements, expected) = submit_statements(&db, table, columns, changes)?;
        if statements.is_empty() {
            return Ok(0);
        }
        let mut st = self.state.lock().await;
        let outcome = self
            .submit_locked(&mut st, &db, &statements, &expected)
            .await;
        self.publish(&st);
        outcome
    }

    async fn submit_locked(
        &self,
        st: &mut State,
        db: &DatabaseType,
        statements: &[String],
        expected: &[Option<u64>],
    ) -> Result<usize> {
        self.ensure_open(st).await?;
        if st.failed {
            bail!("Transaction en échec ({PG_FAILED}) : faites un Rollback");
        }
        let (save, undo, release) = savepoint(db);
        self.exec(st, save, None).await?;
        let mut outcome = Ok(());
        for (stmt, want) in statements.iter().zip(expected) {
            match self.affected(st, db, stmt).await {
                Ok(n) if want.is_some_and(|w| w != n) => {
                    outcome = Err(row_count_error(n));
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    outcome = Err(e);
                    break;
                }
            }
        }
        if let Err(e) = outcome {
            if st.began_on.is_none() {
                return Err(e); // session lost: nothing left to undo
            }
            return match self.exec(st, undo, None).await {
                Ok(_) => Err(e),
                Err(undo_error) => {
                    let e = anyhow!("{e}\nRetour au point de sauvegarde impossible : {undo_error}");
                    Err(self.after_error(st, e).await)
                }
            };
        }
        if let Some(release) = release {
            self.exec(st, release, None).await?;
        }
        st.statements += statements.len();
        Ok(statements.len())
    }

    /// Run `stmt` and return the number of rows it changed (on SQL Server,
    /// `@@ROWCOUNT`: rows changed by triggers do not count).
    async fn affected(&self, st: &mut State, db: &DatabaseType, stmt: &str) -> Result<u64> {
        match db {
            DatabaseType::SQLServer | DatabaseType::Azure => {
                let r = self
                    .exec(st, &format!("{stmt};\nSELECT @@ROWCOUNT"), None)
                    .await?;
                Ok(r.rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0))
            }
            _ => Ok(self.exec(st, stmt, None).await?.rows_affected),
        }
    }

    /// Commit; returns the number of statements committed (0: nothing was
    /// open).
    pub async fn commit(&self) -> Result<usize> {
        self.finish("COMMIT").await
    }

    /// Roll back; returns the number of statements undone.
    pub async fn rollback(&self) -> Result<usize> {
        self.finish("ROLLBACK").await
    }

    async fn finish(&self, sql: &str) -> Result<usize> {
        let mut st = self.state.lock().await;
        let outcome = if st.began_on.is_some() {
            self.end(&mut st, sql).await
        } else {
            Ok(0)
        };
        self.publish(&st);
        outcome
    }

    /// Roll back what is pending and close the session.
    pub async fn close(&self) {
        let mut st = self.state.lock().await;
        if st.began_on.is_some() {
            let _ = self.conn.execute_query("ROLLBACK").await;
        }
        *st = State::default();
        self.publish(&st);
        self.conn.close_sessions().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ops::rows::RowChanges;

    /// A SQLite file with `t(id, name)` holding 'a' and 'b', its shared
    /// connection and a manual transaction on it.
    async fn setup() -> (tempfile::TempDir, Arc<DatabaseConnection>, ManualTx) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        std::fs::File::create(&path).unwrap();
        let config = ConnectionConfig {
            name: "t".into(),
            db_type: DatabaseType::SQLite,
            database: path.display().to_string(),
            ..Default::default()
        };
        let shared = Arc::new(DatabaseConnection::connect(&config).await.unwrap());
        for sql in [
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
            "INSERT INTO t (name) VALUES ('a'), ('b')",
        ] {
            shared.execute_query(sql).await.unwrap();
        }
        let tx = ManualTx::open(&config, shared.clone()).await.unwrap();
        (dir, shared, tx)
    }

    async fn names(conn: &DatabaseConnection) -> Vec<String> {
        let r = conn
            .execute_query("SELECT name FROM t ORDER BY id")
            .await
            .unwrap();
        r.rows.into_iter().map(|mut r| r.remove(0)).collect()
    }

    fn ok(outcomes: &[StatementOutcome]) -> bool {
        outcomes.iter().all(|o| o.result.is_ok())
    }

    #[tokio::test]
    async fn changes_stay_pending_until_commit() {
        let (_dir, shared, tx) = setup().await;
        let out = tx
            .run_script(
                "UPDATE t SET name = 'z' WHERE id = 1; SELECT * FROM t",
                None,
            )
            .await;
        assert!(ok(&out), "{out:?}");
        assert_eq!(
            out[1].result.as_ref().unwrap().rows[0][1],
            "z",
            "seen in the transaction"
        );
        assert!(
            out[1].result.as_ref().unwrap().columns[0].is_primary_key,
            "enriched"
        );
        assert_eq!(
            tx.status(),
            TxStatus {
                open: true,
                statements: 2,
                failed: false
            }
        );
        assert_eq!(names(&shared).await, ["a", "b"], "not committed yet");
        assert_eq!(tx.commit().await.unwrap(), 2);
        assert_eq!(names(&shared).await, ["z", "b"]);
        assert!(!tx.status().open);
        assert_eq!(tx.commit().await.unwrap(), 0, "nothing open");
    }

    #[tokio::test]
    async fn rollback_undoes_everything() {
        let (_dir, shared, tx) = setup().await;
        assert!(ok(&tx
            .run_script("DELETE FROM t; INSERT INTO t (name) VALUES ('c')", None)
            .await));
        assert_eq!(names(&tx.conn()).await, ["c"]);
        assert_eq!(tx.rollback().await.unwrap(), 2);
        assert_eq!(names(&shared).await, ["a", "b"]);
        assert_eq!(names(&tx.conn()).await, ["a", "b"]);
    }

    #[tokio::test]
    async fn typed_transaction_statements_are_tracked() {
        let (_dir, shared, tx) = setup().await;
        // BEGIN while one is open is skipped; COMMIT ends it.
        let out = tx
            .run_script("UPDATE t SET name = 'x' WHERE id = 1; BEGIN; COMMIT", None)
            .await;
        assert!(ok(&out), "{out:?}");
        assert!(!tx.status().open);
        assert_eq!(names(&shared).await, ["x", "b"]);
        // The next statement opens a new one.
        assert!(ok(&tx
            .run_script("UPDATE t SET name = 'y' WHERE id = 2", None)
            .await));
        assert!(tx.status().open);
        assert_eq!(names(&shared).await, ["x", "b"]);
        assert!(ok(&tx.run_script("ROLLBACK", None).await));
        assert!(!tx.status().open);
        assert_eq!(names(&tx.conn()).await, ["x", "b"]);
    }

    #[tokio::test]
    async fn a_failing_statement_keeps_the_transaction() {
        let (_dir, shared, tx) = setup().await;
        let out = tx
            .run_script(
                "UPDATE t SET name = 'z' WHERE id = 1; SELECT nope FROM t; SELECT 1",
                None,
            )
            .await;
        assert_eq!(out.len(), 2, "stops at the failure");
        assert!(out[1].result.is_err());
        assert!(tx.status().open);
        tx.commit().await.unwrap();
        assert_eq!(names(&shared).await, ["z", "b"]);
    }

    #[tokio::test]
    async fn submit_is_pending_and_undone_alone_on_failure() {
        let (_dir, shared, tx) = setup().await;
        let page = crate::engine::ops::query::run_query(&tx.conn(), "SELECT * FROM t")
            .await
            .unwrap();
        let rename = |row: usize, name: &str| RowChanges {
            updates: vec![(
                page.rows[row].clone(),
                vec![page.rows[row][0].clone(), name.into()],
            )],
            ..Default::default()
        };
        assert_eq!(
            tx.submit("t", &page.columns, &rename(0, "q"))
                .await
                .unwrap(),
            1
        );
        assert_eq!(names(&tx.conn()).await, ["q", "b"]);
        assert_eq!(names(&shared).await, ["a", "b"], "not committed");

        // Row 1 deleted meanwhile in the transaction: the update finds no
        // row, only this Submit is undone.
        assert!(ok(&tx.run_script("DELETE FROM t WHERE id = 2", None).await));
        let mut stale = rename(1, "r");
        stale.inserts = vec![vec![None, Some("new".into())]];
        assert!(tx.submit("t", &page.columns, &stale).await.is_err());
        assert_eq!(names(&tx.conn()).await, ["q"], "earlier work kept");
        assert_eq!(tx.status().statements, 2);

        tx.commit().await.unwrap();
        assert_eq!(names(&shared).await, ["q"]);
    }

    #[tokio::test]
    async fn close_rolls_back() {
        let (_dir, shared, tx) = setup().await;
        assert!(ok(&tx.run_script("DELETE FROM t", None).await));
        tx.close().await;
        assert_eq!(names(&shared).await, ["a", "b"]);
    }

    #[test]
    fn statements_at_cursor_in_manual_mode() {
        let text = "BEGIN;\nUPDATE t SET a = 1;\nSELECT 2";
        let cursor = text.find("UPDATE").unwrap();
        assert_eq!(statements_at_cursor(text, cursor), ["UPDATE t SET a = 1"]);
        let block = "BEGIN; SELECT 1; COMMIT";
        assert_eq!(statements_at_cursor(block, 9).len(), 3);
        assert!(statements_at_cursor("  ", 0).is_empty());
        assert!(is_rollback_to("rollback to savepoint s"));
        assert!(!is_rollback_to("ROLLBACK"));
    }
}
