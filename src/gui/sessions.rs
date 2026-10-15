//! Open connections and the metadata cached for them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use crate::engine::models::{ConnectionConfig, SchemaInfo, TableDetails};
use crate::engine::ops::tx::ManualTx;
use crate::engine::sql::statements::quote_chars;

use super::tx::TxView;
use super::worker::{Conn, RunId, Runner, Worker};

/// Transaction mode of a session.
#[derive(Default)]
pub enum TxMode {
    /// Each statement commits on its own.
    #[default]
    Auto,
    /// Switching to manual: the dedicated session is being opened.
    Opening,
    Manual {
        tx: Arc<ManualTx>,
        /// A Commit or Rollback is running.
        ending: bool,
        /// Back to auto-commit once that Commit or Rollback succeeded.
        then_auto: bool,
    },
}

pub struct Session {
    pub config: ConnectionConfig,
    pub conn: Conn,
    pub quotes: (char, char),
    pub schemas: Vec<SchemaInfo>,
    /// table (qualified) → details, filled lazily by the explorer.
    pub details: HashMap<String, Result<TableDetails, String>>,
    /// Tables whose details have been requested and not received yet, with
    /// the request awaited: an answer to another one is stale.
    pub loading: HashMap<String, RunId>,
    pub tx: TxMode,
}

impl Session {
    pub fn new(config: ConnectionConfig, conn: Conn, schemas: Vec<SchemaInfo>) -> Self {
        Self {
            quotes: quote_chars(&config.db_type),
            config,
            conn,
            schemas,
            details: HashMap::new(),
            loading: HashMap::new(),
            tx: TxMode::Auto,
        }
    }

    /// Where the consoles and grids of this session run their SQL.
    pub fn runner(&self) -> Runner {
        match &self.tx {
            TxMode::Manual { tx, .. } => Runner::Manual(tx.clone()),
            _ => Runner::Auto(self.conn.clone()),
        }
    }

    pub fn manual_tx(&self) -> Option<&Arc<ManualTx>> {
        match &self.tx {
            TxMode::Manual { tx, .. } => Some(tx),
            _ => None,
        }
    }

    /// Statements waiting for a Commit (0 in auto-commit mode).
    pub fn pending_statements(&self) -> usize {
        self.manual_tx().map_or(0, |tx| {
            let s = tx.status();
            if s.open {
                s.statements
            } else {
                0
            }
        })
    }

    pub fn tx_view(&self) -> TxView {
        match &self.tx {
            TxMode::Auto => TxView::Auto,
            TxMode::Opening => TxView::Opening,
            TxMode::Manual { tx, ending, .. } => TxView::Manual {
                status: tx.status(),
                ending: *ending,
            },
        }
    }

    /// Request the details of `table` (qualified) of this session `name`,
    /// superseding a request still running for it.
    pub fn load_details(&mut self, worker: &mut Worker, name: &str, table: &str) {
        let run = worker.table_details(name.to_string(), self.conn.clone(), table.to_string());
        self.loading.insert(table.to_string(), run);
    }
}

/// Store the details of `table` answered by request `run`, unless that
/// request is no longer awaited (the metadata was refreshed, or the table's
/// details requested again, since): returns whether they were stored.
pub fn accept_details(
    details: &mut HashMap<String, Result<TableDetails, String>>,
    loading: &mut HashMap<String, RunId>,
    table: String,
    run: RunId,
    outcome: Result<TableDetails, String>,
) -> bool {
    if loading.get(&table) != Some(&run) {
        return false;
    }
    loading.remove(&table);
    details.insert(table, outcome);
    true
}

#[derive(Default)]
pub struct Sessions {
    /// By connection name.
    pub open: BTreeMap<String, Session>,
    pub connecting: BTreeSet<String>,
    /// Last connection failure per connection name (cleared on retry).
    pub errors: HashMap<String, String>,
    /// Bumped by every mutable access to a session (schemas, details):
    /// views caching what they derive from the sessions compare it.
    generation: u64,
}

impl Sessions {
    pub fn get(&self, name: &str) -> Option<&Session> {
        self.open.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.generation += 1;
        self.open.get_mut(name)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Connection `name` was edited (`same_target` or not, `renamed` or
    /// not) while it may be opening: whether that attempt is outdated and no
    /// longer awaited (the caller aborts it). A renamed connection's outcome
    /// would be dropped anyway.
    pub fn edited(&mut self, name: &str, same_target: bool, renamed: bool) -> bool {
        (!same_target || renamed) && self.connecting.remove(name)
    }

    /// An attempt to open `name` ended: whether it was still awaited (else
    /// it was cancelled, and its late outcome is dropped).
    pub fn connect_finished(&mut self, name: &str) -> bool {
        self.connecting.remove(name)
    }

    pub fn conn(&self, name: &str) -> Option<Conn> {
        self.open.get(name).map(|s| s.conn.clone())
    }

    pub fn runner(&self, name: &str) -> Option<Runner> {
        self.open.get(name).map(Session::runner)
    }

    /// Suffix of a Submit's success message: in manual mode its changes
    /// still need a Commit.
    pub fn submit_note(&self, name: &str) -> &'static str {
        if self.is_manual(name) {
            " (en attente de Commit)"
        } else {
            ""
        }
    }

    /// Whether connection `name` is in manual transaction mode.
    pub fn is_manual(&self, name: &str) -> bool {
        self.open.get(name).is_some_and(|s| s.manual_tx().is_some())
    }

    pub fn tx_view(&self, name: &str) -> TxView {
        self.open.get(name).map_or(TxView::Off, Session::tx_view)
    }

    /// `(connection, statements)` of the manual transactions holding
    /// statements not committed.
    pub fn pending_transactions(&self) -> Vec<(String, usize)> {
        self.open
            .iter()
            .map(|(name, s)| (name.clone(), s.pending_statements()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    /// All `(connection, schema, table)` triples, for Ctrl+N and completion.
    pub fn all_tables(&self) -> Vec<(String, String, String)> {
        all_tables(
            self.open
                .iter()
                .map(|(name, s)| (name.as_str(), s.schemas.as_slice())),
        )
    }

    /// Table names of one connection (completion).
    pub fn tables_of(&self, name: &str) -> Vec<String> {
        self.open
            .get(name)
            .map(|s| tables_of(&s.schemas))
            .unwrap_or_default()
    }
}

/// Flatten `(connection, schemas)` pairs into `(connection, schema, table)`.
pub fn all_tables<'a>(
    sessions: impl IntoIterator<Item = (&'a str, &'a [SchemaInfo])>,
) -> Vec<(String, String, String)> {
    sessions
        .into_iter()
        .flat_map(|(conn, schemas)| {
            schemas.iter().flat_map(move |s| {
                s.tables
                    .iter()
                    .map(move |t| (conn.to_string(), s.name.clone(), t.clone()))
            })
        })
        .collect()
}

/// Distinct table names across `schemas`, sorted.
pub fn tables_of(schemas: &[SchemaInfo]) -> Vec<String> {
    let names: BTreeSet<&String> = schemas.iter().flat_map(|s| &s.tables).collect();
    names.into_iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str, tables: &[&str]) -> SchemaInfo {
        SchemaInfo {
            name: name.into(),
            tables: tables.iter().map(|t| t.to_string()).collect(),
            expanded: false,
        }
    }

    #[test]
    fn all_tables_flattens_every_connection() {
        let prod = vec![schema("public", &["users", "orders"])];
        let dev = vec![schema("main", &["t"]), schema("aux", &[])];
        let all = all_tables([("prod", prod.as_slice()), ("dev", dev.as_slice())]);
        let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
        assert_eq!(
            all,
            vec![
                s("prod", "public", "users"),
                s("prod", "public", "orders"),
                s("dev", "main", "t"),
            ]
        );
    }

    #[test]
    fn stale_details_are_dropped() {
        let (mut details, mut loading) = (HashMap::new(), HashMap::new());
        let ok = || Ok(TableDetails::default());
        loading.insert("t".to_string(), 2);
        assert!(
            !accept_details(&mut details, &mut loading, "t".into(), 1, ok()),
            "superseded"
        );
        assert!(details.is_empty());
        assert!(accept_details(
            &mut details,
            &mut loading,
            "t".into(),
            2,
            ok()
        ));
        assert!(details.contains_key("t") && loading.is_empty());
        // After a refresh (nothing awaited any more) a late answer is dropped.
        details.clear();
        assert!(!accept_details(
            &mut details,
            &mut loading,
            "t".into(),
            2,
            ok()
        ));
        assert!(details.is_empty());
    }

    #[test]
    fn a_cancelled_attempt_is_no_longer_awaited() {
        let mut s = Sessions::default();
        s.connecting.insert("prod".into());
        // « Annuler »: the outcome may already be on its way.
        s.connecting.remove("prod");
        assert!(!s.connect_finished("prod"));
        s.connecting.insert("prod".into());
        assert!(s.connect_finished("prod"));
        assert!(s.connecting.is_empty());
    }

    #[test]
    fn editing_a_connection_being_opened_drops_the_attempt() {
        let mut s = Sessions::default();
        s.connecting.insert("prod".into());
        // Same target, same name: the attempt goes on.
        assert!(!s.edited("prod", true, false));
        assert!(s.connecting.contains("prod"));
        // Renamed: its outcome would be dropped anyway.
        assert!(s.edited("prod", true, true));
        assert!(s.connecting.is_empty());
        // Another target.
        s.connecting.insert("prod".into());
        assert!(s.edited("prod", false, false));
        // Nothing being opened.
        assert!(!s.edited("prod", false, false));
    }

    #[test]
    fn tables_of_dedups_and_sorts() {
        let schemas = vec![schema("a", &["z", "x"]), schema("b", &["x", "y"])];
        assert_eq!(tables_of(&schemas), vec!["x", "y", "z"]);
        assert!(tables_of(&[]).is_empty());
    }
}
