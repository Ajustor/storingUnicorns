//! Open connections and the metadata cached for them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use crate::engine::models::{ConnectionConfig, SchemaInfo, TableDetails};
use crate::engine::ops::transfer::qualified;
use crate::engine::ops::tx::ManualTx;
use crate::engine::sql::completion::{ColumnInfo, TableRef};
use crate::engine::sql::statements::{quote_chars, quote_ident};

use super::editor::completion::CompletionSource;

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

    /// Key in `details` of the table `t` names: its schema when given, else
    /// the first schema holding it (exact name first, then ignoring case).
    pub fn details_key(&self, t: &TableRef) -> Option<String> {
        details_key(&self.schemas, t, self.quotes)
    }

    /// Columns of `t` for completion: `None` while not loaded (unknown
    /// tables, and failed loads, have none).
    pub fn completion_columns(&self, t: &TableRef) -> Option<Vec<ColumnInfo>> {
        completion_columns(&self.schemas, &self.details, self.quotes, t)
    }

    /// Load the details of `t` (completion), unless they are loaded or
    /// loading already.
    pub fn request_columns(&mut self, worker: &mut Worker, name: &str, t: &TableRef) {
        let Some(key) = self.details_key(t) else {
            return;
        };
        if !self.details.contains_key(&key) && !self.loading.contains_key(&key) {
            self.load_details(worker, name, &key);
        }
    }
}

/// See [`Session::details_key`].
pub fn details_key(schemas: &[SchemaInfo], t: &TableRef, quotes: (char, char)) -> Option<String> {
    let in_schema = |s: &SchemaInfo| {
        t.schema
            .as_deref()
            .is_none_or(|want| s.name.eq_ignore_ascii_case(want))
    };
    let find = |eq: &dyn Fn(&str) -> bool| {
        schemas.iter().filter(|s| in_schema(s)).find_map(|s| {
            s.tables
                .iter()
                .find(|n| eq(n))
                .map(|n| qualified(&s.name, n, quotes))
        })
    };
    find(&|n| n == t.name).or_else(|| find(&|n| n.eq_ignore_ascii_case(&t.name)))
}

/// See [`Session::completion_columns`].
pub fn completion_columns(
    schemas: &[SchemaInfo],
    details: &HashMap<String, Result<TableDetails, String>>,
    quotes: (char, char),
    t: &TableRef,
) -> Option<Vec<ColumnInfo>> {
    let Some(key) = details_key(schemas, t, quotes) else {
        return Some(Vec::new());
    };
    match details.get(&key)? {
        Ok(d) => Some(
            d.columns
                .iter()
                .map(|c| ColumnInfo {
                    name: c.name.clone(),
                    type_name: c.type_name.clone(),
                    primary_key: c.is_primary_key,
                })
                .collect(),
        ),
        Err(_) => Some(Vec::new()),
    }
}

/// Tables offered by completion: bare names with a single schema, else
/// `schema.table` (a part holding a dot is quoted).
pub fn completion_tables(schemas: &[SchemaInfo], quotes: (char, char)) -> Vec<String> {
    if schemas.len() <= 1 {
        return tables_of(schemas);
    }
    let part = |p: &str| {
        if p.contains('.') {
            quote_ident(p, quotes)
        } else {
            p.to_string()
        }
    };
    schemas
        .iter()
        .flat_map(|s| {
            s.tables
                .iter()
                .map(move |t| format!("{}.{}", part(&s.name), part(t)))
        })
        .collect()
}

/// Completion over a session (none: disconnected).
pub struct SessionSource<'a>(pub Option<&'a Session>);

impl CompletionSource for SessionSource<'_> {
    fn quotes(&self) -> (char, char) {
        self.0.map_or(('"', '"'), |s| s.quotes)
    }

    fn tables(&self) -> Vec<String> {
        self.0
            .map(|s| completion_tables(&s.schemas, s.quotes))
            .unwrap_or_default()
    }

    fn columns(&self, t: &TableRef) -> Option<Vec<ColumnInfo>> {
        self.0.map_or(Some(Vec::new()), |s| s.completion_columns(t))
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

    fn table(schema: Option<&str>, name: &str) -> TableRef {
        TableRef {
            schema: schema.map(Into::into),
            name: name.into(),
            alias: None,
        }
    }

    #[test]
    fn details_key_finds_the_schema_of_a_table() {
        let schemas = vec![
            schema("public", &["users", "Orders"]),
            schema("sales", &["orders", "users"]),
        ];
        let q = ('"', '"');
        let key = |s, n| details_key(&schemas, &table(s, n), q);
        assert_eq!(key(None, "users").as_deref(), Some("\"public\".\"users\""));
        assert_eq!(
            key(Some("SALES"), "users").as_deref(),
            Some("\"sales\".\"users\"")
        );
        // An exact name wins over one differing in case only.
        assert_eq!(key(None, "orders").as_deref(), Some("\"sales\".\"orders\""));
        assert_eq!(
            key(None, "ORDERS").as_deref(),
            Some("\"public\".\"Orders\"")
        );
        assert_eq!(key(None, "nope"), None);
        assert_eq!(key(Some("aux"), "users"), None);
    }

    #[test]
    fn completion_tables_are_qualified_with_several_schemas() {
        let q = ('"', '"');
        let one = vec![schema("main", &["b", "a"])];
        assert_eq!(completion_tables(&one, q), vec!["a", "b"]);
        let two = vec![schema("public", &["users"]), schema("a.b", &["t"])];
        assert_eq!(
            completion_tables(&two, q),
            vec!["public.users", "\"a.b\".t"]
        );
    }

    #[test]
    fn completion_columns_follow_the_loaded_details() {
        use crate::engine::models::Column;
        let schemas = vec![schema("main", &["users", "bad"])];
        let mut details = HashMap::new();
        let q = ('"', '"');
        let cols =
            |details: &HashMap<_, _>, n| completion_columns(&schemas, details, q, &table(None, n));
        assert_eq!(cols(&details, "users"), None, "not loaded yet");
        details.insert(
            qualified("main", "users", q),
            Ok(TableDetails {
                columns: vec![Column {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                    nullable: false,
                    is_primary_key: true,
                }],
                ..TableDetails::default()
            }),
        );
        details.insert(qualified("main", "bad", q), Err("boom".into()));
        assert_eq!(
            cols(&details, "USERS"),
            Some(vec![ColumnInfo {
                name: "id".into(),
                type_name: "INTEGER".into(),
                primary_key: true,
            }])
        );
        assert_eq!(
            cols(&details, "bad"),
            Some(vec![]),
            "failed: nothing to wait for"
        );
        assert_eq!(
            cols(&details, "nope"),
            Some(vec![]),
            "unknown: nothing to load"
        );
    }

    #[test]
    fn tables_of_dedups_and_sorts() {
        let schemas = vec![schema("a", &["z", "x"]), schema("b", &["x", "y"])];
        assert_eq!(tables_of(&schemas), vec!["x", "y", "z"]);
        assert!(tables_of(&[]).is_empty());
    }
}
