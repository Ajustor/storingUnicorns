//! The explorer's tree as a flat list of one-line rows (see the parent
//! module), and the open states the user set.

use std::collections::{BTreeSet, HashMap};

use egui::Id;

use crate::engine::models::{ConnectionConfig, SchemaInfo, TableDetails};
use crate::engine::ops::transfer::qualified;

use crate::gui::sessions::Sessions;
use crate::gui::worker::RunId;

/// `(schema index, table indexes)` of the tables whose names contain
/// `filter` (case-insensitive); schemas without a match are dropped.
pub fn filter_tables(schemas: &[SchemaInfo], filter: &str) -> Vec<(usize, Vec<usize>)> {
    let needle = filter.to_lowercase();
    schemas
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let tables: Vec<usize> = s
                .tables
                .iter()
                .enumerate()
                .filter(|(_, t)| needle.is_empty() || t.to_lowercase().contains(&needle))
                .map(|(j, _)| j)
                .collect();
            (!tables.is_empty()).then_some((i, tables))
        })
        .collect()
}

/// Group of a table's details.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Columns,
    Keys,
    Indexes,
}

/// A table, by indexes into the connections, their schemas and tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TableAt {
    pub conn: usize,
    pub schema: usize,
    pub table: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// `conn` indexes the configured connections.
    Connection {
        conn: usize,
        open: bool,
    },
    /// A weak note ("Connexion…", "Aucune table").
    Note(&'static str),
    /// Last connection failure of `conn`.
    ConnError {
        conn: usize,
    },
    Schema {
        conn: usize,
        schema: usize,
        count: usize,
        open: bool,
    },
    Table {
        at: TableAt,
        open: bool,
    },
    DetailsLoading,
    DetailsError {
        at: TableAt,
    },
    Group {
        at: TableAt,
        group: Group,
        count: usize,
        open: bool,
    },
    /// `index`-th line of a group (for keys, the primary key comes first).
    Item {
        at: TableAt,
        group: Group,
        index: usize,
    },
    /// `group` couldn't be loaded on this server.
    GroupError {
        at: TableAt,
        group: Group,
    },
}

/// One line of the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    pub depth: u8,
    pub kind: RowKind,
}

/// What the tree needs from an open session.
pub struct SessionView<'a> {
    pub schemas: &'a [SchemaInfo],
    pub details: &'a HashMap<String, Result<TableDetails, String>>,
    pub loading: &'a HashMap<String, RunId>,
    pub quotes: (char, char),
}

/// What the tree needs from a configured connection.
pub struct ConnView<'a> {
    pub name: &'a str,
    pub session: Option<SessionView<'a>>,
    pub connecting: bool,
    pub failed: bool,
}

pub(super) fn conn_views<'a>(
    connections: &'a [ConnectionConfig],
    sessions: &'a Sessions,
) -> Vec<ConnView<'a>> {
    connections
        .iter()
        .map(|c| ConnView {
            name: &c.name,
            session: sessions.get(&c.name).map(|s| SessionView {
                schemas: &s.schemas,
                details: &s.details,
                loading: &s.loading,
                quotes: s.quotes,
            }),
            connecting: sessions.connecting.contains(&c.name),
            failed: sessions.errors.contains_key(&c.name),
        })
        .collect()
}

pub fn connection_id(conn: &str) -> Id {
    Id::new(("explorer_connection", conn))
}

pub fn schema_id(conn: &str, schema: &str) -> Id {
    Id::new(("explorer_schema", conn, schema))
}

pub fn table_id(conn: &str, schema: &str, table: &str) -> Id {
    Id::new(("explorer_table", conn, schema, table))
}

pub fn group_id(conn: &str, schema: &str, table: &str, group: Group) -> Id {
    Id::new(("explorer_group", conn, schema, table, group))
}

/// Rows of the tree, and the tables shown open whose details are neither
/// loaded nor loading (`(connection index, qualified table)`).
#[derive(Default)]
pub struct Flat {
    pub rows: Vec<Row>,
    pub needs_details: Vec<(usize, String)>,
}

/// Flatten the tree. `open(id)` is the state the user gave a node, if any.
/// Connections are closed by default and can only stay open while
/// connected or connecting; schemas are open by default when their
/// connection has only one. While filtering, the connections and schemas
/// with matching tables are forced open (their state is kept) and the others
/// hidden.
pub fn flatten(conns: &[ConnView], filter: &str, open: impl Fn(Id) -> Option<bool>) -> Flat {
    let mut flat = Flat::default();
    for (ci, c) in conns.iter().enumerate() {
        let groups = match (&c.session, filter.is_empty()) {
            (_, true) => None,
            (None, false) => continue,
            (Some(s), false) => {
                let groups = filter_tables(s.schemas, filter);
                if groups.is_empty() {
                    continue;
                }
                Some(groups)
            }
        };
        let is_open = groups.is_some()
            || (open(connection_id(c.name)).unwrap_or(false)
                && (c.session.is_some() || c.connecting));
        flat.push(
            0,
            RowKind::Connection {
                conn: ci,
                open: is_open,
            },
        );
        if is_open {
            match &c.session {
                None => flat.push(1, RowKind::Note("Connexion…")),
                Some(s) => match groups {
                    Some(groups) => {
                        for (si, tables) in groups {
                            flat.schema(c, ci, s, si, Some(&tables), &open);
                        }
                    }
                    None if s.schemas.is_empty() => flat.push(1, RowKind::Note("Aucune table")),
                    None => {
                        for si in 0..s.schemas.len() {
                            flat.schema(c, ci, s, si, None, &open);
                        }
                    }
                },
            }
        }
        if c.session.is_none() && c.failed {
            flat.push(1, RowKind::ConnError { conn: ci });
        }
    }
    flat
}

impl Flat {
    fn push(&mut self, depth: u8, kind: RowKind) {
        self.rows.push(Row { depth, kind });
    }

    /// Schema `si` of `s`, with only `tables` (forced open) when filtering.
    fn schema(
        &mut self,
        c: &ConnView,
        ci: usize,
        s: &SessionView,
        si: usize,
        tables: Option<&[usize]>,
        open: &impl Fn(Id) -> Option<bool>,
    ) {
        let schema = &s.schemas[si];
        let count = tables.map_or(schema.tables.len(), <[usize]>::len);
        let is_open = tables.is_some()
            || open(schema_id(c.name, &schema.name)).unwrap_or(s.schemas.len() == 1);
        self.push(
            1,
            RowKind::Schema {
                conn: ci,
                schema: si,
                count,
                open: is_open,
            },
        );
        if !is_open {
            return;
        }
        let mut table = |ti: usize| {
            let name = &schema.tables[ti];
            let at = TableAt {
                conn: ci,
                schema: si,
                table: ti,
            };
            let is_open = open(table_id(c.name, &schema.name, name)).unwrap_or(false);
            self.push(2, RowKind::Table { at, open: is_open });
            if is_open {
                self.details(c, s, at, &schema.name, name, open);
            }
        };
        match tables {
            Some(tables) => tables.iter().copied().for_each(&mut table),
            None => (0..schema.tables.len()).for_each(table),
        }
    }

    fn details(
        &mut self,
        c: &ConnView,
        s: &SessionView,
        at: TableAt,
        schema: &str,
        table: &str,
        open: &impl Fn(Id) -> Option<bool>,
    ) {
        let key = qualified(schema, table, s.quotes);
        let d = match s.details.get(&key) {
            None => {
                self.push(3, RowKind::DetailsLoading);
                if !s.loading.contains_key(&key) {
                    self.needs_details.push((at.conn, key));
                }
                return;
            }
            Some(Err(_)) => {
                self.push(3, RowKind::DetailsError { at });
                return;
            }
            Some(Ok(d)) => d,
        };
        for group in [Group::Columns, Group::Keys, Group::Indexes] {
            let count = group_len(d, group);
            let is_open = open(group_id(c.name, schema, table, group)).unwrap_or(false);
            self.push(
                3,
                RowKind::Group {
                    at,
                    group,
                    count,
                    open: is_open,
                },
            );
            if is_open {
                for index in 0..count {
                    self.push(4, RowKind::Item { at, group, index });
                }
                // After the lines it has: keys still list the primary key
                // when the foreign keys couldn't be read.
                if group_error(d, group).is_some() {
                    self.push(4, RowKind::GroupError { at, group });
                }
            }
        }
    }
}

pub(super) fn has_pk(d: &TableDetails) -> bool {
    d.columns.iter().any(|c| c.is_primary_key)
}

/// Number of lines of `group` (keys: the primary key, then foreign keys).
fn group_len(d: &TableDetails, group: Group) -> usize {
    match group {
        Group::Columns => d.columns.len(),
        Group::Keys => usize::from(has_pk(d)) + d.foreign_keys.len(),
        Group::Indexes => d.indexes.len(),
    }
}

/// Why `group` is empty, when the server couldn't list it.
pub(super) fn group_error(d: &TableDetails, group: Group) -> Option<&str> {
    match group {
        Group::Columns => None,
        Group::Keys => d.foreign_keys_error.as_deref(),
        Group::Indexes => d.indexes_error.as_deref(),
    }
}

/// Open states set by the user, and the flattened rows cached until the
/// filter, the sessions or an open state change.
#[derive(Default)]
pub struct Tree {
    open: HashMap<Id, bool>,
    /// Bumped by each open-state change.
    pub(super) generation: u64,
    cache: Option<(u64, Flat)>,
}

impl Tree {
    pub(super) fn set_open(&mut self, id: Id, open: bool) {
        self.open.insert(id, open);
        self.generation += 1;
    }

    /// The rows for inputs hashing to `key`, built by `build` when they
    /// changed.
    pub(super) fn rows(
        &mut self,
        key: u64,
        build: impl FnOnce(&HashMap<Id, bool>) -> Flat,
    ) -> &Flat {
        if !matches!(&self.cache, Some((k, _)) if *k == key) {
            let flat = build(&self.open);
            self.cache = Some((key, flat));
        }
        &self.cache.as_ref().expect("just filled").1
    }
}

/// Hash of everything the rows depend on besides the open states.
pub(super) fn inputs_key(
    filter: &str,
    connections: &[ConnectionConfig],
    sessions: &Sessions,
    tree: u64,
) -> u64 {
    egui::util::hash((
        filter,
        tree,
        sessions.generation(),
        connections.iter().map(|c| &c.name).collect::<Vec<_>>(),
        sessions.open.keys().collect::<Vec<_>>(),
        &sessions.connecting,
        sessions.errors.keys().collect::<BTreeSet<_>>(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{Column, ForeignKeyInfo};

    fn schema(name: &str, tables: &[&str]) -> SchemaInfo {
        SchemaInfo {
            name: name.into(),
            tables: tables.iter().map(|t| t.to_string()).collect(),
            expanded: true,
        }
    }

    #[test]
    fn filter_is_case_insensitive_and_drops_empty_schemas() {
        let schemas = vec![
            schema("public", &["Users", "orders"]),
            schema("audit", &["logs"]),
        ];
        let f = filter_tables(&schemas, "user");
        assert_eq!(f, vec![(0, vec![0])]);
        assert_eq!(filter_tables(&schemas, "").len(), 2);
        assert!(filter_tables(&schemas, "nope").is_empty());
    }

    struct Fixture {
        schemas: Vec<SchemaInfo>,
        details: HashMap<String, Result<TableDetails, String>>,
        loading: HashMap<String, RunId>,
    }

    impl Fixture {
        fn new(schemas: Vec<SchemaInfo>) -> Self {
            Self {
                schemas,
                details: HashMap::new(),
                loading: HashMap::new(),
            }
        }

        fn view<'a>(&'a self, name: &'a str) -> ConnView<'a> {
            ConnView {
                name,
                session: Some(SessionView {
                    schemas: &self.schemas,
                    details: &self.details,
                    loading: &self.loading,
                    quotes: ('"', '"'),
                }),
                connecting: false,
                failed: false,
            }
        }
    }

    fn offline(name: &str, connecting: bool, failed: bool) -> ConnView<'_> {
        ConnView {
            name,
            session: None,
            connecting,
            failed,
        }
    }

    fn kinds(flat: &Flat) -> Vec<RowKind> {
        flat.rows.iter().map(|r| r.kind).collect()
    }

    fn opened(ids: &[Id]) -> impl Fn(Id) -> Option<bool> + '_ {
        move |id| ids.contains(&id).then_some(true)
    }

    #[test]
    fn connections_are_closed_by_default() {
        let f = Fixture::new(vec![schema("main", &["t"])]);
        let flat = flatten(&[f.view("a")], "", |_| None);
        assert_eq!(
            kinds(&flat),
            vec![RowKind::Connection {
                conn: 0,
                open: false
            }]
        );
    }

    #[test]
    fn a_lone_schema_is_open_by_default_several_are_closed() {
        let one = Fixture::new(vec![schema("main", &["t1", "t2"])]);
        let two = Fixture::new(vec![schema("a", &["t"]), schema("b", &["u"])]);
        let ids = [connection_id("one"), connection_id("two")];
        let flat = flatten(&[one.view("one"), two.view("two")], "", opened(&ids));
        let at = |table| TableAt {
            conn: 0,
            schema: 0,
            table,
        };
        assert_eq!(
            kinds(&flat),
            vec![
                RowKind::Connection {
                    conn: 0,
                    open: true
                },
                RowKind::Schema {
                    conn: 0,
                    schema: 0,
                    count: 2,
                    open: true
                },
                RowKind::Table {
                    at: at(0),
                    open: false
                },
                RowKind::Table {
                    at: at(1),
                    open: false
                },
                RowKind::Connection {
                    conn: 1,
                    open: true
                },
                RowKind::Schema {
                    conn: 1,
                    schema: 0,
                    count: 1,
                    open: false
                },
                RowKind::Schema {
                    conn: 1,
                    schema: 1,
                    count: 1,
                    open: false
                },
            ]
        );
        let depths: Vec<u8> = flat.rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 2, 0, 1, 1]);
        // The user's choice wins over the default.
        let closed = |id| (id == schema_id("one", "main")).then_some(false);
        let flat = flatten(&[one.view("one")], "", |id| closed(id).or(opened(&ids)(id)));
        assert_eq!(flat.rows.len(), 2);
    }

    #[test]
    fn offline_connections_only_stay_open_while_connecting() {
        let ids = [connection_id("a"), connection_id("b")];
        let flat = flatten(
            &[offline("a", false, true), offline("b", true, false)],
            "",
            opened(&ids),
        );
        assert_eq!(
            kinds(&flat),
            vec![
                RowKind::Connection {
                    conn: 0,
                    open: false
                },
                RowKind::ConnError { conn: 0 },
                RowKind::Connection {
                    conn: 1,
                    open: true
                },
                RowKind::Note("Connexion…"),
            ]
        );
        let empty = Fixture::new(Vec::new());
        let ids = [connection_id("e")];
        let flat = flatten(&[empty.view("e")], "", opened(&ids));
        assert_eq!(flat.rows[1].kind, RowKind::Note("Aucune table"));
    }

    #[test]
    fn filtering_forces_matches_open_and_hides_the_rest() {
        let a = Fixture::new(vec![
            schema("public", &["users", "orders", "user_roles"]),
            schema("audit", &["logs"]),
        ]);
        let b = Fixture::new(vec![schema("main", &["items"])]);
        let conns = [a.view("a"), b.view("b"), offline("c", false, false)];
        // Stored states say closed: ignored while filtering.
        let flat = flatten(&conns, "USER", |_| Some(false));
        let at = |table| TableAt {
            conn: 0,
            schema: 0,
            table,
        };
        assert_eq!(
            kinds(&flat),
            vec![
                RowKind::Connection {
                    conn: 0,
                    open: true
                },
                RowKind::Schema {
                    conn: 0,
                    schema: 0,
                    count: 2,
                    open: true
                },
                RowKind::Table {
                    at: at(0),
                    open: false
                },
                RowKind::Table {
                    at: at(2),
                    open: false
                },
            ]
        );
        assert!(flatten(&conns, "zzz", |_| None).rows.is_empty());
    }

    #[test]
    fn open_tables_show_their_details_or_ask_for_them() {
        let mut f = Fixture::new(vec![schema("main", &["t", "u"])]);
        let ids = [
            connection_id("a"),
            table_id("a", "main", "t"),
            table_id("a", "main", "u"),
            group_id("a", "main", "t", Group::Keys),
        ];
        let flat = flatten(&[f.view("a")], "", opened(&ids));
        assert_eq!(flat.rows[3].kind, RowKind::DetailsLoading);
        assert_eq!(
            flat.needs_details,
            vec![
                (0, "\"main\".\"t\"".to_string()),
                (0, "\"main\".\"u\"".to_string())
            ]
        );

        let column = |name: &str, pk: bool| Column {
            name: name.into(),
            type_name: "INTEGER".into(),
            nullable: !pk,
            is_primary_key: pk,
        };
        f.details.insert(
            "\"main\".\"t\"".into(),
            Ok(TableDetails {
                columns: vec![column("id", true), column("u_id", false)],
                indexes: Vec::new(),
                foreign_keys: vec![ForeignKeyInfo {
                    name: "fk".into(),
                    columns: vec!["u_id".into()],
                    ref_table: "u".into(),
                    ref_columns: vec!["id".into()],
                }],
                ..TableDetails::default()
            }),
        );
        f.loading.insert("\"main\".\"u\"".into(), 1);
        let flat = flatten(&[f.view("a")], "", opened(&ids));
        let at = TableAt {
            conn: 0,
            schema: 0,
            table: 0,
        };
        assert_eq!(
            kinds(&flat)[3..9],
            [
                RowKind::Group {
                    at,
                    group: Group::Columns,
                    count: 2,
                    open: false
                },
                RowKind::Group {
                    at,
                    group: Group::Keys,
                    count: 2,
                    open: true
                },
                RowKind::Item {
                    at,
                    group: Group::Keys,
                    index: 0
                },
                RowKind::Item {
                    at,
                    group: Group::Keys,
                    index: 1
                },
                RowKind::Group {
                    at,
                    group: Group::Indexes,
                    count: 0,
                    open: false
                },
                RowKind::Table {
                    at: TableAt { table: 1, ..at },
                    open: true
                },
            ]
        );
        assert_eq!(flat.rows[3].depth, 3);
        assert_eq!(flat.rows[5].depth, 4);
        assert!(flat.needs_details.is_empty(), "loaded or loading");

        f.details
            .insert("\"main\".\"u\"".into(), Err("boom".into()));
        let flat = flatten(&[f.view("a")], "", opened(&ids));
        assert_eq!(
            flat.rows.last().map(|r| r.kind),
            Some(RowKind::DetailsError {
                at: TableAt { table: 1, ..at }
            })
        );
    }

    #[test]
    fn empty_groups_that_failed_to_load_say_so() {
        let mut f = Fixture::new(vec![schema("main", &["t"])]);
        let ids = [
            connection_id("a"),
            table_id("a", "main", "t"),
            group_id("a", "main", "t", Group::Keys),
            group_id("a", "main", "t", Group::Indexes),
        ];
        f.details.insert(
            "\"main\".\"t\"".into(),
            Ok(TableDetails {
                indexes_error: Some("relation pg_index does not exist".into()),
                foreign_keys_error: Some("no fk".into()),
                ..TableDetails::default()
            }),
        );
        let flat = flatten(&[f.view("a")], "", opened(&ids));
        let at = TableAt {
            conn: 0,
            schema: 0,
            table: 0,
        };
        assert_eq!(
            kinds(&flat)[4..],
            [
                RowKind::Group {
                    at,
                    group: Group::Keys,
                    count: 0,
                    open: true
                },
                RowKind::GroupError {
                    at,
                    group: Group::Keys
                },
                RowKind::Group {
                    at,
                    group: Group::Indexes,
                    count: 0,
                    open: true
                },
                RowKind::GroupError {
                    at,
                    group: Group::Indexes
                },
            ]
        );
        assert_eq!(flat.rows[5].depth, 4);
        // A closed group shows no note.
        let flat = flatten(&[f.view("a")], "", opened(&ids[..2]));
        assert!(!kinds(&flat)
            .iter()
            .any(|k| matches!(k, RowKind::GroupError { .. })));
    }

    #[test]
    fn failed_foreign_keys_show_under_the_primary_key() {
        let mut f = Fixture::new(vec![schema("main", &["t"])]);
        let ids = [
            connection_id("a"),
            table_id("a", "main", "t"),
            group_id("a", "main", "t", Group::Keys),
        ];
        f.details.insert(
            "\"main\".\"t\"".into(),
            Ok(TableDetails {
                columns: vec![Column {
                    name: "id".into(),
                    type_name: "INTEGER".into(),
                    nullable: false,
                    is_primary_key: true,
                }],
                foreign_keys_error: Some("no fk".into()),
                ..TableDetails::default()
            }),
        );
        let flat = flatten(&[f.view("a")], "", opened(&ids));
        let at = TableAt {
            conn: 0,
            schema: 0,
            table: 0,
        };
        assert_eq!(
            kinds(&flat)[4..7],
            [
                RowKind::Group {
                    at,
                    group: Group::Keys,
                    count: 1,
                    open: true
                },
                RowKind::Item {
                    at,
                    group: Group::Keys,
                    index: 0
                },
                RowKind::GroupError {
                    at,
                    group: Group::Keys
                },
            ]
        );
    }

    #[test]
    fn rows_are_rebuilt_only_when_their_inputs_change() {
        let mut tree = Tree::default();
        let mut builds = 0;
        for key in [1, 1, 2, 2, 1] {
            tree.rows(key, |_| {
                builds += 1;
                Flat::default()
            });
        }
        assert_eq!(builds, 3);
        let generation = tree.generation;
        tree.set_open(connection_id("a"), true);
        assert_ne!(tree.generation, generation);
        tree.rows(3, |open| {
            assert_eq!(open.get(&connection_id("a")), Some(&true));
            Flat::default()
        });
    }
}
