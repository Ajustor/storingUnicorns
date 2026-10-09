//! Open connections and the metadata cached for them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::engine::models::{ConnectionConfig, SchemaInfo, TableDetails};
use crate::engine::sql::statements::quote_chars;

use super::worker::{Conn, RunId, Worker};

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

    pub fn conn(&self, name: &str) -> Option<Conn> {
        self.open.get(name).map(|s| s.conn.clone())
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
    fn tables_of_dedups_and_sorts() {
        let schemas = vec![schema("a", &["z", "x"]), schema("b", &["x", "y"])];
        assert_eq!(tables_of(&schemas), vec!["x", "y", "z"]);
        assert!(tables_of(&[]).is_empty());
    }
}
