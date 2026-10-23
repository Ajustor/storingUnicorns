//! The explorer's tree as a flat list of one-line rows (see the parent
//! module), and the open states the user set.

use std::collections::{BTreeSet, HashMap};

use egui::Id;

use crate::engine::config::folders::{self, is_within, FolderNode, SEP};
use crate::engine::config::AppConfig;
use crate::engine::models::{ConnectionConfig, Family, SchemaInfo, TableDetails};
use crate::engine::nosql::redis::{self, KeyInfo, KeyNode};
use crate::engine::ops::transfer::qualified;

use crate::gui::sessions::{redis_db, KeySet, Sessions};
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
    /// `folder` indexes `Flat::folders`; `count` is the number of
    /// connections it holds, subfolders included.
    Folder {
        folder: usize,
        count: usize,
        open: bool,
    },
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
        /// Hidden by the user, shown because « Afficher les schémas
        /// masqués » is on.
        hidden: bool,
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
    /// A group of Redis keys sharing a prefix (`node` indexes
    /// `Flat::key_rows`) in database `schema` of `conn`.
    KeyGroup {
        conn: usize,
        schema: usize,
        node: usize,
        open: bool,
    },
    /// A Redis key (`node` indexes `Flat::key_rows`).
    Key {
        conn: usize,
        schema: usize,
        node: usize,
    },
    /// The keys of a Redis database couldn't be listed.
    KeysError {
        conn: usize,
        schema: usize,
    },
    /// The explorer lists a database's first keys only.
    KeysMore {
        conn: usize,
        schema: usize,
    },
}

/// A Redis key tree node shown by a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyRow {
    pub label: String,
    /// A group's prefix (`user:42:`), a key's full name.
    pub prefix: String,
    /// Keys under a group.
    pub count: usize,
    /// A key: its index in the database's `KeySet::keys`.
    pub key: Option<usize>,
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
    /// MongoDB: no « Clés » group, fields sampled; Redis: databases hold
    /// keys, not tables.
    pub family: Family,
    pub redis_keys: &'a HashMap<u8, Result<KeySet, String>>,
    pub keys_loading: &'a HashMap<u8, RunId>,
}

/// What the tree needs from a configured connection.
pub struct ConnView<'a> {
    pub name: &'a str,
    /// Schemas the user hid.
    pub hidden: &'a [String],
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
            hidden: &c.hidden_schemas,
            session: sessions.get(&c.name).map(|s| SessionView {
                schemas: &s.schemas,
                details: &s.details,
                loading: &s.loading,
                quotes: s.quotes,
                family: s.config.db_type.family(),
                redis_keys: &s.redis_keys,
                keys_loading: &s.keys_loading,
            }),
            connecting: sessions.connecting.contains(&c.name),
            failed: sessions.errors.contains_key(&c.name),
        })
        .collect()
}

pub fn folder_id(path: &str) -> Id {
    Id::new(("explorer_folder", path))
}

/// Open state of « Afficher les schémas masqués » of `conn`.
pub fn show_hidden_id(conn: &str) -> Id {
    Id::new(("explorer_show_hidden", conn))
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

/// Open state of the Redis key group `prefix` of database `schema`.
pub fn key_group_id(conn: &str, schema: &str, prefix: &str) -> Id {
    Id::new(("explorer_key_group", conn, schema, prefix))
}

/// Rows of the tree, and the tables shown open whose details are neither
/// loaded nor loading (`(connection index, qualified table)`).
#[derive(Default)]
pub struct Flat {
    pub rows: Vec<Row>,
    pub needs_details: Vec<(usize, String)>,
    /// Paths of the folders the rows show (`RowKind::Folder::folder`).
    pub folders: Vec<String>,
    /// Every folder, depth first: the targets of « Déplacer vers… ».
    pub all_folders: Vec<String>,
    /// Redis databases shown open whose keys are neither listed nor being
    /// listed (`(connection index, database)`).
    pub needs_keys: Vec<(usize, u8)>,
    /// The Redis key tree nodes the rows show (`RowKind::KeyGroup` /
    /// `RowKind::Key`).
    pub key_rows: Vec<KeyRow>,
}

/// Number of connections in `node`, subfolders included.
fn connection_count(node: &FolderNode) -> usize {
    node.connections.len() + node.folders.iter().map(connection_count).sum::<usize>()
}

fn all_folders(node: &FolderNode, out: &mut Vec<String>) {
    for f in &node.folders {
        out.push(f.path.clone());
        all_folders(f, out);
    }
}

/// Flatten the tree of `root` (its connection indexes index `conns`).
/// `open(id)` is the state the user gave a node, if any. Folders are open
/// by default. Connections are closed by default and can only stay open
/// while connected or connecting; schemas are open by default when their
/// connection shows only one. Hidden schemas are left out, unless the
/// connection's « Afficher les schémas masqués » is on. While filtering,
/// the folders, connections and schemas with matching tables are forced
/// open (their state is kept) and the others hidden.
pub fn flatten(
    root: &FolderNode,
    conns: &[ConnView],
    filter: &str,
    open: impl Fn(Id) -> Option<bool>,
) -> Flat {
    let mut flat = Flat::default();
    all_folders(root, &mut flat.all_folders);
    flat.node(root, 0, conns, filter, &open);
    flat
}

impl Flat {
    fn push(&mut self, depth: u8, kind: RowKind) {
        self.rows.push(Row { depth, kind });
    }

    /// The content of folder `node` at `depth`: its subfolders, then its
    /// connections. Whether it showed anything.
    fn node(
        &mut self,
        node: &FolderNode,
        depth: u8,
        conns: &[ConnView],
        filter: &str,
        open: &impl Fn(Id) -> Option<bool>,
    ) -> bool {
        let mut any = false;
        for f in &node.folders {
            let (rows, folders) = (self.rows.len(), self.folders.len());
            let is_open = !filter.is_empty() || open(folder_id(&f.path)).unwrap_or(true);
            self.folders.push(f.path.clone());
            self.push(
                depth,
                RowKind::Folder {
                    folder: folders,
                    count: connection_count(f),
                    open: is_open,
                },
            );
            let shown = is_open && self.node(f, depth.saturating_add(1), conns, filter, open);
            if !filter.is_empty() && !shown {
                // No match inside: the folder goes too.
                self.rows.truncate(rows);
                self.folders.truncate(folders);
                continue;
            }
            any = true;
        }
        for &ci in &node.connections {
            if let Some(c) = conns.get(ci) {
                any |= self.connection(ci, c, depth, filter, open);
            }
        }
        any
    }

    /// Connection `ci` and, when open, its schemas. Whether it showed.
    fn connection(
        &mut self,
        ci: usize,
        c: &ConnView,
        depth: u8,
        filter: &str,
        open: &impl Fn(Id) -> Option<bool>,
    ) -> bool {
        let show_hidden = open(show_hidden_id(c.name)).unwrap_or(false);
        let shown = |s: &SchemaInfo| show_hidden || !c.hidden.contains(&s.name);
        let groups = match (&c.session, filter.is_empty()) {
            (_, true) => None,
            (None, false) => return false,
            // Redis: the databases with a listed key matching.
            (Some(s), false) if s.family == Family::KeyValue => {
                let needle = filter.to_lowercase();
                let groups: Vec<(usize, Vec<usize>)> = (0..s.schemas.len())
                    .filter(|&si| shown(&s.schemas[si]))
                    .filter(|&si| !matching_keys(s, si, &needle).is_empty())
                    .map(|si| (si, Vec::new()))
                    .collect();
                if groups.is_empty() {
                    return false;
                }
                Some(groups)
            }
            (Some(s), false) => {
                let groups: Vec<_> = filter_tables(s.schemas, filter)
                    .into_iter()
                    .filter(|(si, _)| shown(&s.schemas[*si]))
                    .collect();
                if groups.is_empty() {
                    return false;
                }
                Some(groups)
            }
        };
        let is_open = groups.is_some()
            || (open(connection_id(c.name)).unwrap_or(false)
                && (c.session.is_some() || c.connecting));
        self.push(
            depth,
            RowKind::Connection {
                conn: ci,
                open: is_open,
            },
        );
        let inner = depth.saturating_add(1);
        if is_open {
            match &c.session {
                None => self.push(inner, RowKind::Note("Connexion…")),
                Some(s) => match groups {
                    Some(groups) => {
                        let lone = groups.len() == 1;
                        for (si, tables) in groups {
                            if s.family == Family::KeyValue {
                                self.redis_db(c, ci, s, si, inner, true, filter, open);
                            } else {
                                self.schema(c, ci, s, si, inner, lone, Some(&tables), open);
                            }
                        }
                    }
                    None if s.schemas.is_empty() => self.push(inner, RowKind::Note("Aucune table")),
                    None => {
                        let visible: Vec<usize> = (0..s.schemas.len())
                            .filter(|&si| shown(&s.schemas[si]))
                            .collect();
                        if visible.is_empty() {
                            self.push(inner, RowKind::Note("Tous les schémas sont masqués"));
                        }
                        let lone = visible.len() == 1;
                        for si in visible {
                            if s.family == Family::KeyValue {
                                self.redis_db(c, ci, s, si, inner, lone, filter, open);
                            } else {
                                self.schema(c, ci, s, si, inner, lone, None, open);
                            }
                        }
                    }
                },
            }
        }
        if c.session.is_none() && c.failed {
            self.push(inner, RowKind::ConnError { conn: ci });
        }
        true
    }

    /// Schema `si` of `s` at `depth`, with only `tables` (forced open) when
    /// filtering; `lone`: the only schema shown (open by default).
    #[allow(clippy::too_many_arguments)]
    fn schema(
        &mut self,
        c: &ConnView,
        ci: usize,
        s: &SessionView,
        si: usize,
        depth: u8,
        lone: bool,
        tables: Option<&[usize]>,
        open: &impl Fn(Id) -> Option<bool>,
    ) {
        let schema = &s.schemas[si];
        let count = tables.map_or(schema.tables.len(), <[usize]>::len);
        let is_open = tables.is_some() || open(schema_id(c.name, &schema.name)).unwrap_or(lone);
        self.push(
            depth,
            RowKind::Schema {
                conn: ci,
                schema: si,
                count,
                open: is_open,
                hidden: c.hidden.contains(&schema.name),
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
            self.push(depth + 1, RowKind::Table { at, open: is_open });
            if is_open {
                self.details(c, s, at, depth + 2, &schema.name, name, open);
            }
        };
        match tables {
            Some(tables) => tables.iter().copied().for_each(&mut table),
            None => (0..schema.tables.len()).for_each(table),
        }
    }

    /// Redis database `si` of `s` at `depth` and, when open, its keys
    /// grouped by prefix (only those containing `filter`, forced open).
    #[allow(clippy::too_many_arguments)]
    fn redis_db(
        &mut self,
        c: &ConnView,
        ci: usize,
        s: &SessionView,
        si: usize,
        depth: u8,
        lone: bool,
        filter: &str,
        open: &impl Fn(Id) -> Option<bool>,
    ) {
        let schema = &s.schemas[si];
        let filtering = !filter.is_empty();
        let is_open = filtering || open(schema_id(c.name, &schema.name)).unwrap_or(lone);
        let count = match redis_db(&schema.name).and_then(|db| s.redis_keys.get(&db)) {
            Some(Ok(set)) => set.keys.len(),
            _ => 0,
        };
        self.push(
            depth,
            RowKind::Schema {
                conn: ci,
                schema: si,
                count,
                open: is_open,
                hidden: c.hidden.contains(&schema.name),
            },
        );
        let Some(db) = redis_db(&schema.name).filter(|_| is_open) else {
            return;
        };
        let inner = depth.saturating_add(1);
        let set = match s.redis_keys.get(&db) {
            None => {
                self.push(inner, RowKind::DetailsLoading);
                if !s.keys_loading.contains_key(&db) {
                    self.needs_keys.push((ci, db));
                }
                return;
            }
            Some(Err(_)) => {
                self.push(
                    inner,
                    RowKind::KeysError {
                        conn: ci,
                        schema: si,
                    },
                );
                return;
            }
            Some(Ok(set)) => set,
        };
        if set.keys.is_empty() {
            self.push(inner, RowKind::Note("Aucune clé"));
        }
        let shown = matching_keys(s, si, &filter.to_lowercase());
        let keys: Vec<KeyInfo> = shown.iter().map(|&i| set.keys[i].clone()).collect();
        let mut nodes = redis::key_tree(&keys, ':');
        if nodes.is_empty() && !keys.is_empty() {
            // No grouping available: the keys one by one.
            nodes = keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let name = redis::display_bytes(&k.key);
                    KeyNode {
                        label: name.clone(),
                        prefix: name,
                        children: Vec::new(),
                        key: Some(i),
                        count: 1,
                    }
                })
                .collect();
        }
        let ids = KeyTreeIds {
            conn: c.name,
            schema: &schema.name,
            ci,
            si,
            filtering,
        };
        self.key_nodes(&nodes, &shown, &ids, inner, open);
        if set.more {
            self.push(
                inner,
                RowKind::KeysMore {
                    conn: ci,
                    schema: si,
                },
            );
        }
    }

    /// Rows of the key tree `nodes`; `shown[k]` is the index in the
    /// database's keys of key `k` of the tree.
    fn key_nodes(
        &mut self,
        nodes: &[KeyNode],
        shown: &[usize],
        ids: &KeyTreeIds,
        depth: u8,
        open: &impl Fn(Id) -> Option<bool>,
    ) {
        for n in nodes {
            let node = self.key_rows.len();
            self.key_rows.push(KeyRow {
                label: n.label.clone(),
                prefix: n.prefix.clone(),
                count: n.count,
                key: n.key.and_then(|k| shown.get(k).copied()),
            });
            match n.key {
                Some(_) if n.children.is_empty() => self.push(
                    depth,
                    RowKind::Key {
                        conn: ids.ci,
                        schema: ids.si,
                        node,
                    },
                ),
                _ => {
                    let is_open = ids.filtering
                        || open(key_group_id(ids.conn, ids.schema, &n.prefix)).unwrap_or(false);
                    self.push(
                        depth,
                        RowKind::KeyGroup {
                            conn: ids.ci,
                            schema: ids.si,
                            node,
                            open: is_open,
                        },
                    );
                    if is_open {
                        self.key_nodes(&n.children, shown, ids, depth.saturating_add(1), open);
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn details(
        &mut self,
        c: &ConnView,
        s: &SessionView,
        at: TableAt,
        depth: u8,
        schema: &str,
        table: &str,
        open: &impl Fn(Id) -> Option<bool>,
    ) {
        let key = qualified(schema, table, s.quotes);
        let d = match s.details.get(&key) {
            None => {
                self.push(depth, RowKind::DetailsLoading);
                if !s.loading.contains_key(&key) {
                    self.needs_details.push((at.conn, key));
                }
                return;
            }
            Some(Err(_)) => {
                self.push(depth, RowKind::DetailsError { at });
                return;
            }
            Some(Ok(d)) => d,
        };
        // MongoDB has no keys to list (`_id` is the key of every collection).
        let groups: &[Group] = if s.family == Family::Document {
            &[Group::Columns, Group::Indexes]
        } else {
            &[Group::Columns, Group::Keys, Group::Indexes]
        };
        for &group in groups {
            let count = group_len(d, group);
            let is_open = open(group_id(c.name, schema, table, group)).unwrap_or(false);
            self.push(
                depth,
                RowKind::Group {
                    at,
                    group,
                    count,
                    open: is_open,
                },
            );
            if is_open {
                for index in 0..count {
                    self.push(depth + 1, RowKind::Item { at, group, index });
                }
                // After the lines it has: keys still list the primary key
                // when the foreign keys couldn't be read.
                if group_error(d, group).is_some() {
                    self.push(depth + 1, RowKind::GroupError { at, group });
                }
            }
        }
    }
}

/// Names a key tree's rows build their open-state ids from.
struct KeyTreeIds<'a> {
    conn: &'a str,
    schema: &'a str,
    ci: usize,
    si: usize,
    filtering: bool,
}

/// Indexes of the listed keys of Redis database `si` of `s` whose name
/// contains `needle` (lowercase; empty: all of them).
fn matching_keys(s: &SessionView, si: usize, needle: &str) -> Vec<usize> {
    let Some(Ok(set)) = redis_db(&s.schemas[si].name).and_then(|db| s.redis_keys.get(&db)) else {
        return Vec::new();
    };
    (0..set.keys.len())
        .filter(|&i| {
            needle.is_empty()
                || redis::display_bytes(&set.keys[i].key)
                    .to_lowercase()
                    .contains(needle)
        })
        .collect()
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

    /// The state the user gave `id`, if any.
    pub(super) fn is_open(&self, id: Id) -> Option<bool> {
        self.open.get(&id).copied()
    }

    /// Folder `from` (and the folders under it, among `folders`) became
    /// `to`: they keep the states the user gave them.
    pub(super) fn folder_moved(&mut self, folders: &[String], from: &str, to: &str) {
        self.remap_folders(folders, |p| moved_under(p, from, to));
    }

    /// Folder `deleted` was deleted: the folders under it (among `folders`)
    /// moved up one level and keep the states the user gave them.
    pub(super) fn folder_deleted(&mut self, folders: &[String], deleted: &str) {
        self.remap_folders(folders, |p| lifted_out(p, deleted));
    }

    /// Connection `old` was renamed `new`: its « Afficher les schémas
    /// masqués » follows it.
    pub fn connection_renamed(&mut self, old: &str, new: &str) {
        if let Some(show) = self.open.remove(&show_hidden_id(old)) {
            self.open.insert(show_hidden_id(new), show);
        }
        self.generation += 1;
    }

    /// Move the states of `folders` to the paths `map` gives them (`None`:
    /// unchanged).
    fn remap_folders(&mut self, folders: &[String], map: impl Fn(&str) -> Option<String>) {
        let moved: Vec<(Id, Id)> = folders
            .iter()
            .filter_map(|p| Some((folder_id(p), folder_id(&map(p)?))))
            .collect();
        let states: Vec<(Id, bool)> = moved
            .iter()
            .filter_map(|(old, new)| Some((*new, self.open.remove(old)?)))
            .collect();
        self.open.extend(states);
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
pub(super) fn inputs_key(filter: &str, config: &AppConfig, sessions: &Sessions, tree: u64) -> u64 {
    egui::util::hash((
        filter,
        tree,
        sessions.generation(),
        &config.folders,
        config
            .connections
            .iter()
            .map(|c| (&c.name, &c.folder, &c.hidden_schemas))
            .collect::<Vec<_>>(),
        sessions.open.keys().collect::<Vec<_>>(),
        &sessions.connecting,
        sessions.errors.keys().collect::<BTreeSet<_>>(),
    ))
}

/// `path` once folder `from` became `to`, if it is `from` or under it.
pub fn moved_under(path: &str, from: &str, to: &str) -> Option<String> {
    (!from.is_empty() && is_within(path, from)).then(|| format!("{to}{}", &path[from.len()..]))
}

/// `path` once folder `deleted` is deleted, if it lies under it: its
/// content moves up one level.
pub fn lifted_out(path: &str, deleted: &str) -> Option<String> {
    let rest = path.strip_prefix(deleted)?.strip_prefix(SEP)?;
    (!deleted.is_empty()).then(|| join_folder(folders::parent(deleted), rest))
}

/// A connection goes into `folder` (`None`: the root), typed in the form:
/// when it doesn't exist yet it is created explicitly, as « Nouveau
/// dossier… » would, so that it stays once empty.
pub fn keep_new_folder(config: &mut AppConfig, folder: Option<&str>) {
    let Some(folder) = folder.filter(|f| !f.trim().is_empty()) else {
        return;
    };
    if !config.folder_exists(folder) {
        // Refused only for an invalid path, which the move then reports.
        let _ = config.create_folder(folder);
    }
}

/// Folder `path`, just moved by a drag, stays once empty (it may have
/// existed only through its connections).
pub fn keep_moved_folder(config: &mut AppConfig, path: &str) {
    if !config.folders.iter().any(|f| f == path) {
        config.folders.push(path.to_string());
    }
}

/// `name` in folder `parent` (`""`: the root).
pub fn join_folder(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}{SEP}{name}")
    }
}

/// What the explorer drags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragItem {
    /// A connection, by name.
    Connection(String),
    /// A folder, by path.
    Folder(String),
}

impl DragItem {
    /// Its folder once dropped into `target` (`""`: the root).
    pub fn destination(&self, target: &str) -> String {
        match self {
            DragItem::Connection(_) => target.to_string(),
            DragItem::Folder(path) => join_folder(target, folders::name(path)),
        }
    }
}

/// Whether dropping `item` into folder `target` (`""`: the root) moves it:
/// not where it already is, and never a folder into itself or one of its
/// subfolders. `folder_of` gives a connection's folder (`""`: the root).
pub fn can_drop(item: &DragItem, target: &str, folder_of: impl Fn(&str) -> String) -> bool {
    match item {
        DragItem::Connection(name) => folder_of(name) != target,
        DragItem::Folder(path) => !is_within(target, path) && folders::parent(path) != target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::folders::tree;
    use crate::engine::models::{Column, ForeignKeyInfo};

    /// A config with connections `(name, folder)` and explicit `folders`.
    fn config(conns: &[(&str, Option<&str>)], folders: &[&str]) -> AppConfig {
        AppConfig {
            connections: conns
                .iter()
                .map(|(name, folder)| ConnectionConfig {
                    name: name.to_string(),
                    folder: folder.map(Into::into),
                    ..Default::default()
                })
                .collect(),
            folders: folders.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        }
    }

    fn views(config: &AppConfig) -> Vec<ConnView<'_>> {
        config
            .connections
            .iter()
            .map(|c| ConnView {
                name: &c.name,
                hidden: &c.hidden_schemas,
                session: None,
                connecting: false,
                failed: false,
            })
            .collect()
    }

    /// Each row as `depth name`, folders as `[path]`.
    fn outline(flat: &Flat, config: &AppConfig) -> Vec<String> {
        flat.rows
            .iter()
            .map(|r| {
                let label = match r.kind {
                    RowKind::Folder { folder, count, .. } => {
                        format!("[{}] {count}", flat.folders[folder])
                    }
                    RowKind::Connection { conn, .. } => config.connections[conn].name.clone(),
                    RowKind::Schema { schema, hidden, .. } => {
                        format!("schema {schema}{}", if hidden { " (masqué)" } else { "" })
                    }
                    RowKind::Note(n) => n.to_string(),
                    other => format!("{other:?}"),
                };
                format!("{} {label}", r.depth)
            })
            .collect()
    }

    #[test]
    fn folders_come_first_nested_and_open_by_default() {
        let c = config(
            &[
                ("prod", Some("Clients/ACME")),
                ("local", None),
                ("beta", Some("Clients")),
            ],
            &["Vide"],
        );
        let flat = flatten(&tree(&c), &views(&c), "", |_| None);
        assert_eq!(
            outline(&flat, &c),
            [
                "0 [Clients] 2",
                "1 [Clients/ACME] 1",
                "2 prod",
                "1 beta",
                "0 [Vide] 0",
                "0 local",
            ]
        );
        assert_eq!(flat.all_folders, ["Clients", "Clients/ACME", "Vide"]);
        // A folder the user closed hides its content.
        let closed = |id| (id == folder_id("Clients/ACME")).then_some(false);
        let flat = flatten(&tree(&c), &views(&c), "", closed);
        assert_eq!(
            outline(&flat, &c),
            [
                "0 [Clients] 2",
                "1 [Clients/ACME] 1",
                "1 beta",
                "0 [Vide] 0",
                "0 local",
            ]
        );
        assert!(matches!(
            flat.rows[1].kind,
            RowKind::Folder { open: false, .. }
        ));
    }

    #[test]
    fn filtering_keeps_only_the_folders_holding_a_match() {
        let c = config(
            &[("a", Some("X/Y")), ("b", Some("Z")), ("c", None)],
            &["Vide"],
        );
        let with_users = Fixture::new(vec![schema("main", &["users"])]);
        let without = Fixture::new(vec![schema("main", &["items"])]);
        let conns = [
            with_users.view("a"),
            without.view("b"),
            offline("c", false, false),
        ];
        // Closed folders are forced open while filtering.
        let flat = flatten(&tree(&c), &conns, "user", |_| Some(false));
        assert_eq!(
            outline(&flat, &c)[..4],
            ["0 [X] 1", "1 [X/Y] 1", "2 a", "3 schema 0"]
        );
        assert_eq!(flat.rows.len(), 5, "the table too");
        assert_eq!(flat.folders, ["X", "X/Y"]);
        assert!(flatten(&tree(&c), &conns, "zzz", |_| None).rows.is_empty());
    }

    #[test]
    fn hidden_schemas_are_left_out_unless_shown_greyed() {
        let f = Fixture::new(vec![
            schema("public", &["users"]),
            schema("audit", &["logs"]),
        ]);
        let hidden = ["audit".to_string()];
        let view = || ConnView {
            hidden: &hidden,
            ..f.view("a")
        };
        let ids = [connection_id("a")];
        let flat_of = |open: &dyn Fn(Id) -> Option<bool>| at_root(&[view()], "", open);
        let rows = kinds(&flat_of(&opened(&ids)));
        // The lone schema left is open by default.
        assert_eq!(
            rows[1],
            RowKind::Schema {
                conn: 0,
                schema: 0,
                count: 1,
                open: true,
                hidden: false
            }
        );
        assert_eq!(rows.len(), 3, "connection, public, users");
        let show = [connection_id("a"), show_hidden_id("a")];
        let rows = kinds(&flat_of(&opened(&show)));
        assert_eq!(
            rows[2],
            RowKind::Schema {
                conn: 0,
                schema: 1,
                count: 1,
                open: false,
                hidden: true
            }
        );
        // Filtering skips them too, unless shown.
        assert!(at_root(&[view()], "logs", |_| None).rows.is_empty());
        assert_eq!(at_root(&[view()], "logs", opened(&show)).rows.len(), 3);
        // Everything hidden: a note says so.
        let all = ["public".to_string(), "audit".to_string()];
        let v = ConnView {
            hidden: &all,
            ..f.view("a")
        };
        assert_eq!(
            kinds(&at_root(&[v], "", opened(&ids)))[1],
            RowKind::Note("Tous les schémas sont masqués")
        );
    }

    #[test]
    fn drops_never_put_a_folder_inside_itself() {
        let folder_of = |name: &str| {
            if name == "prod" {
                "A".into()
            } else {
                String::new()
            }
        };
        let conn = DragItem::Connection("prod".into());
        assert!(can_drop(&conn, "", folder_of));
        assert!(can_drop(&conn, "A/B", folder_of));
        assert!(!can_drop(&conn, "A", folder_of), "already there");
        let local = DragItem::Connection("local".into());
        assert!(!can_drop(&local, "", folder_of));
        let folder = DragItem::Folder("A/B".into());
        assert!(!can_drop(&folder, "A/B", folder_of), "itself");
        assert!(!can_drop(&folder, "A/B/C", folder_of), "a descendant");
        assert!(!can_drop(&folder, "A", folder_of), "already there");
        assert!(can_drop(&folder, "", folder_of));
        assert!(can_drop(&folder, "A/BC", folder_of), "a sibling's prefix");
        assert_eq!(folder.destination(""), "B");
        assert_eq!(folder.destination("X/Y"), "X/Y/B");
        assert_eq!(conn.destination("X"), "X");
    }

    #[test]
    fn moved_folders_keep_their_open_states() {
        assert_eq!(moved_under("A/B/C", "A/B", "Z"), Some("Z/C".into()));
        assert_eq!(moved_under("A/B", "A/B", "Z"), Some("Z".into()));
        assert_eq!(moved_under("A/BC", "A/B", "Z"), None);
        let mut t = Tree::default();
        t.set_open(folder_id("A/B"), false);
        t.set_open(folder_id("A/B/C"), false);
        t.set_open(folder_id("A"), false);
        let all = ["A".to_string(), "A/B".into(), "A/B/C".into()];
        t.folder_moved(&all, "A/B", "Z");
        assert_eq!(t.is_open(folder_id("Z")), Some(false));
        assert_eq!(t.is_open(folder_id("Z/C")), Some(false));
        assert_eq!(t.is_open(folder_id("A/B")), None);
        assert_eq!(t.is_open(folder_id("A")), Some(false));
    }

    #[test]
    fn deleted_folders_leave_their_states_to_the_lifted_subfolders() {
        assert_eq!(lifted_out("A/B/C", "A/B"), Some("A/C".into()));
        assert_eq!(lifted_out("A/B/C/D", "A/B"), Some("A/C/D".into()));
        assert_eq!(lifted_out("A/C", "A"), Some("C".into()));
        assert_eq!(lifted_out("A/B", "A/B"), None, "the deleted one");
        assert_eq!(lifted_out("A/BC", "A/B"), None);
        let mut t = Tree::default();
        t.set_open(folder_id("A"), true);
        t.set_open(folder_id("A/B"), false);
        t.set_open(folder_id("A/B/C"), false);
        let all = ["A".to_string(), "A/B".into(), "A/B/C".into()];
        t.folder_deleted(&all, "A/B");
        assert_eq!(t.is_open(folder_id("A/C")), Some(false));
        assert_eq!(t.is_open(folder_id("A")), Some(true), "the parent's own");
        assert_eq!(t.is_open(folder_id("A/B/C")), None);
    }

    #[test]
    fn the_show_hidden_toggle_follows_a_renamed_connection() {
        let mut t = Tree::default();
        t.set_open(show_hidden_id("old"), true);
        t.connection_renamed("old", "new");
        assert_eq!(t.is_open(show_hidden_id("new")), Some(true));
        assert_eq!(t.is_open(show_hidden_id("old")), None);
    }

    #[test]
    fn folders_typed_or_dragged_to_stay_once_empty() {
        let mut c = config(&[("a", None), ("b", Some("Implicit"))], &[]);
        keep_new_folder(&mut c, Some("New/Sub"));
        c.move_connection("a", Some("New/Sub")).unwrap();
        c.move_connection("a", None).unwrap();
        assert!(c.folder_exists("New/Sub"), "kept once empty");
        // Existing folders are left as they are, the root needs none.
        keep_new_folder(&mut c, Some("Implicit"));
        keep_new_folder(&mut c, Some("New/Sub"));
        keep_new_folder(&mut c, None);
        keep_new_folder(&mut c, Some("  "));
        assert_eq!(c.folders, ["New", "New/Sub"]);
        // A folder that existed only through its connection, dragged
        // elsewhere, stays there once emptied.
        c.rename_folder("Implicit", "New/Moved").unwrap();
        keep_moved_folder(&mut c, "New/Moved");
        keep_moved_folder(&mut c, "New/Moved");
        c.move_connection("b", None).unwrap();
        assert!(c.folder_exists("New/Moved"));
        assert_eq!(c.folders, ["New", "New/Sub", "New/Moved"]);
    }

    #[test]
    fn the_cache_key_follows_folders_and_hidden_schemas() {
        let sessions = Sessions::default();
        let mut c = config(&[("a", None)], &[]);
        let key = |c: &AppConfig| inputs_key("", c, &sessions, 0);
        let k0 = key(&c);
        c.folders.push("F".into());
        let k1 = key(&c);
        c.connections[0].folder = Some("F".into());
        let k2 = key(&c);
        c.connections[0].hidden_schemas.push("s".into());
        let k3 = key(&c);
        assert!(k0 != k1 && k1 != k2 && k2 != k3);
    }

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
        family: Family,
        keys: HashMap<u8, Result<KeySet, String>>,
        keys_loading: HashMap<u8, RunId>,
    }

    impl Fixture {
        fn new(schemas: Vec<SchemaInfo>) -> Self {
            Self {
                schemas,
                details: HashMap::new(),
                loading: HashMap::new(),
                family: Family::Sql,
                keys: HashMap::new(),
                keys_loading: HashMap::new(),
            }
        }

        fn view<'a>(&'a self, name: &'a str) -> ConnView<'a> {
            ConnView {
                name,
                hidden: &[],
                session: Some(SessionView {
                    schemas: &self.schemas,
                    details: &self.details,
                    loading: &self.loading,
                    quotes: ('"', '"'),
                    family: self.family,
                    redis_keys: &self.keys,
                    keys_loading: &self.keys_loading,
                }),
                connecting: false,
                failed: false,
            }
        }
    }

    fn offline(name: &str, connecting: bool, failed: bool) -> ConnView<'_> {
        ConnView {
            name,
            hidden: &[],
            session: None,
            connecting,
            failed,
        }
    }

    /// `conns` all at the root.
    fn at_root(conns: &[ConnView], filter: &str, open: impl Fn(Id) -> Option<bool>) -> Flat {
        let root = FolderNode {
            connections: (0..conns.len()).collect(),
            ..Default::default()
        };
        flatten(&root, conns, filter, open)
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
        let flat = at_root(&[f.view("a")], "", |_| None);
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
        let flat = at_root(&[one.view("one"), two.view("two")], "", opened(&ids));
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
                    open: true,
                    hidden: false
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
                    open: false,
                    hidden: false
                },
                RowKind::Schema {
                    conn: 1,
                    schema: 1,
                    count: 1,
                    open: false,
                    hidden: false
                },
            ]
        );
        let depths: Vec<u8> = flat.rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 2, 0, 1, 1]);
        // The user's choice wins over the default.
        let closed = |id| (id == schema_id("one", "main")).then_some(false);
        let flat = at_root(&[one.view("one")], "", |id| closed(id).or(opened(&ids)(id)));
        assert_eq!(flat.rows.len(), 2);
    }

    #[test]
    fn offline_connections_only_stay_open_while_connecting() {
        let ids = [connection_id("a"), connection_id("b")];
        let flat = at_root(
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
        let flat = at_root(&[empty.view("e")], "", opened(&ids));
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
        let flat = at_root(&conns, "USER", |_| Some(false));
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
                    open: true,
                    hidden: false
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
        assert!(at_root(&conns, "zzz", |_| None).rows.is_empty());
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
        let flat = at_root(&[f.view("a")], "", opened(&ids));
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
        let flat = at_root(&[f.view("a")], "", opened(&ids));
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
        let flat = at_root(&[f.view("a")], "", opened(&ids));
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
        let flat = at_root(&[f.view("a")], "", opened(&ids));
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
        let flat = at_root(&[f.view("a")], "", opened(&ids[..2]));
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
        let flat = at_root(&[f.view("a")], "", opened(&ids));
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
    fn mongo_collections_show_sampled_fields_and_indexes_but_no_keys() {
        let mut f = Fixture::new(vec![schema("shop", &["users"])]);
        f.family = Family::Document;
        let ids = [connection_id("m"), table_id("m", "shop", "users")];
        f.details
            .insert("\"shop\".\"users\"".into(), Ok(TableDetails::default()));
        let flat = at_root(&[f.view("m")], "", opened(&ids));
        let groups: Vec<Group> = flat
            .rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Group { group, .. } => Some(group),
                _ => None,
            })
            .collect();
        assert_eq!(groups, [Group::Columns, Group::Indexes]);
    }

    fn redis_key(name: &str) -> KeyInfo {
        KeyInfo {
            key: name.as_bytes().to_vec(),
            kind: crate::engine::nosql::redis::RedisType::String,
            ttl: None,
            size: None,
        }
    }

    fn redis_fixture() -> Fixture {
        let mut f = Fixture::new(vec![schema("db0", &[]), schema("db1", &[])]);
        f.family = Family::KeyValue;
        f
    }

    #[test]
    fn redis_databases_list_their_keys_when_opened() {
        let mut f = redis_fixture();
        let ids = [connection_id("r"), schema_id("r", "db0")];
        // Not listed yet: the explorer asks for them.
        let flat = at_root(&[f.view("r")], "", opened(&ids));
        assert_eq!(flat.needs_keys, vec![(0, 0)]);
        assert!(kinds(&flat).contains(&RowKind::DetailsLoading));
        // Being listed: not asked twice.
        f.keys_loading.insert(0, 1);
        assert!(at_root(&[f.view("r")], "", opened(&ids))
            .needs_keys
            .is_empty());
        f.keys_loading.clear();
        f.keys.insert(
            0,
            Ok(KeySet {
                keys: vec![redis_key("user:1"), redis_key("user:2")],
                more: true,
            }),
        );
        let flat = at_root(&[f.view("r")], "", opened(&ids));
        assert!(flat.needs_keys.is_empty());
        // The listed keys show (grouped by the engine's key tree, else one
        // by one), then the note of the explorer's limit.
        let keys: Vec<Option<usize>> = flat
            .rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Key { node, .. } => Some(flat.key_rows[node].key),
                _ => None,
            })
            .collect();
        let groups = flat
            .rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::KeyGroup { .. }))
            .count();
        assert!(groups > 0 || keys == [Some(0), Some(1)], "{keys:?}");
        let more = kinds(&flat)
            .iter()
            .position(|k| *k == RowKind::KeysMore { conn: 0, schema: 0 });
        let db1 = kinds(&flat)
            .iter()
            .position(|k| matches!(k, RowKind::Schema { schema: 1, .. }));
        assert!(more.is_some() && more < db1, "after db0's keys");
        // The database shows its key count.
        assert!(flat.rows.iter().any(|r| matches!(
            r.kind,
            RowKind::Schema {
                schema: 0,
                count: 2,
                ..
            }
        )));
        f.keys.insert(0, Err("NOPERM".into()));
        let flat = at_root(&[f.view("r")], "", opened(&ids));
        assert!(kinds(&flat).contains(&RowKind::KeysError { conn: 0, schema: 0 }));
    }

    #[test]
    fn filtering_a_redis_connection_keeps_databases_with_matching_keys() {
        let mut f = redis_fixture();
        f.keys.insert(
            1,
            Ok(KeySet {
                keys: vec![redis_key("session:abc"), redis_key("user:1")],
                more: false,
            }),
        );
        let flat = at_root(&[f.view("r")], "SESSION", |_| None);
        let schemas: Vec<usize> = flat
            .rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Schema { schema, open, .. } => {
                    assert!(open, "forced open");
                    Some(schema)
                }
                _ => None,
            })
            .collect();
        assert_eq!(schemas, [1]);
        assert!(at_root(&[f.view("r")], "nothing", |_| None).rows.is_empty());
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
