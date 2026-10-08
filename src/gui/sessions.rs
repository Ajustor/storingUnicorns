//! Open connections and the metadata cached for them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::engine::models::{ConnectionConfig, SchemaInfo, TableDetails};
use crate::engine::sql::statements::quote_chars;

use super::worker::Conn;

pub struct Session {
    pub config: ConnectionConfig,
    pub conn: Conn,
    pub quotes: (char, char),
    pub schemas: Vec<SchemaInfo>,
    /// table (qualified) → details, filled lazily by the explorer.
    pub details: HashMap<String, Result<TableDetails, String>>,
    /// Tables whose details have been requested and not received yet.
    pub loading: HashSet<String>,
}

impl Session {
    pub fn new(config: ConnectionConfig, conn: Conn, schemas: Vec<SchemaInfo>) -> Self {
        Self {
            quotes: quote_chars(&config.db_type),
            config,
            conn,
            schemas,
            details: HashMap::new(),
            loading: HashSet::new(),
        }
    }
}

#[derive(Default)]
pub struct Sessions {
    /// By connection name.
    pub open: BTreeMap<String, Session>,
    pub connecting: BTreeSet<String>,
    /// Last connection failure per connection name (cleared on retry).
    pub errors: HashMap<String, String>,
}

impl Sessions {
    pub fn get(&self, name: &str) -> Option<&Session> {
        self.open.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.open.get_mut(name)
    }

    #[allow(dead_code)] // console and data tabs (plan 3b, Tasks 6 and 8)
    pub fn conn(&self, name: &str) -> Option<Conn> {
        self.open.get(name).map(|s| s.conn.clone())
    }

    /// All `(connection, schema, table)` triples, for Ctrl+N and completion.
    #[allow(dead_code)] // table search (plan 3b, Task 9)
    pub fn all_tables(&self) -> Vec<(String, String, String)> {
        all_tables(
            self.open
                .iter()
                .map(|(name, s)| (name.as_str(), s.schemas.as_slice())),
        )
    }

    /// Table names of one connection (completion).
    #[allow(dead_code)] // console completion (plan 3b, Task 6)
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
    fn tables_of_dedups_and_sorts() {
        let schemas = vec![schema("a", &["z", "x"]), schema("b", &["x", "y"])];
        assert_eq!(tables_of(&schemas), vec!["x", "y", "z"]);
        assert!(tables_of(&[]).is_empty());
    }
}
