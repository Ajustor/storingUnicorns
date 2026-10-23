//! MongoDB and Redis in the TUI (plan 13, Task 5): the Tables panel
//! (databases → collections, Redis key tree), the console (`nosql::run_script`
//! with its `(o/N)` confirmation), editing the rows of a result that has a
//! `Source`, the JSON view, the indexes of a collection, small forms (new
//! index, new key, SCAN pattern) and the JSON / JSON Lines / CSV transfers
//! (`transfer`). The SQL paths (`ops::query`, `ops::rows`…) are never used
//! for these engines.

pub mod transfer;

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use bson::{Bson, Document};
use crossterm::event::KeyCode;

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{is_null, DatabaseType, QueryResult};
use crate::engine::nosql::mongo::{self, CollectionRef, DocChange, DocPage, MongoIndex};
use crate::engine::nosql::redis::{self, KeyEdit, KeyInfo, KeyNode, RedisType, RedisValue};
use crate::engine::nosql::{self, is_absent, NoSqlOutcome, NoSqlScope, Source};
use crate::engine::ops::rows::RowChanges;
use crate::engine::sql::completion::{Suggestion, TableRef};
use crate::tui::{AppState, DialogMode};

/// Rows a console command returns at most.
pub const MAX_ROWS: usize = 1000;
/// Keys of a Redis database loaded into the Tables panel at most.
pub const MAX_KEYS: usize = 5000;
/// `COUNT` of each SCAN step.
const SCAN_COUNT: usize = 500;
/// Separator of the Redis key groups.
const KEY_SEPARATOR: char = ':';

/// The NoSQL part of the TUI state.
#[derive(Default)]
pub struct NoSqlUi {
    /// Keys loaded per Redis database (index).
    pub redis: HashMap<u8, RedisKeys>,
    /// Keys per Redis database (`INFO keyspace`, empty ones absent), once
    /// read.
    pub key_counts: Option<std::collections::BTreeMap<u8, u64>>,
    /// Where the rows of the shown result come from: editable when set.
    pub source: Option<Source>,
    /// Why the shown result is read-only (a projection…), when known.
    pub read_only: Option<String>,
    /// A question waiting for its `(o/N)` answer.
    pub confirm: Option<Confirm>,
    pub json: Option<JsonView>,
    pub indexes: Option<IndexesView>,
    pub form: Option<Form>,
    pub transfer: Option<transfer::Transfer>,
}

impl NoSqlUi {
    /// Close every NoSQL dialog (the loaded keys and the result stay).
    pub fn close_dialogs(&mut self) {
        self.confirm = None;
        self.json = None;
        self.indexes = None;
        self.form = None;
        self.transfer = None;
    }
}

/// The keys of a Redis database, as the Tables panel shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RedisKeys {
    /// SCAN pattern they match.
    pub pattern: String,
    pub keys: Vec<KeyInfo>,
    pub tree: Vec<KeyNode>,
    /// Prefixes of the unfolded groups.
    pub expanded: HashSet<String>,
    /// The lines shown under the database (`visible_rows`).
    pub rows: Vec<KeyRow>,
    /// More keys match than `MAX_KEYS`.
    pub truncated: bool,
}

/// One line of the Redis key tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyRow {
    Group {
        depth: usize,
        label: String,
        prefix: String,
        count: usize,
        open: bool,
    },
    Key {
        depth: usize,
        label: String,
        /// Index in `RedisKeys::keys`.
        index: usize,
    },
    /// « … » when the keys were cut at `MAX_KEYS`.
    More,
}

/// The engine of the open connection, when it is MongoDB or Redis.
pub fn engine(state: &AppState) -> Option<DatabaseType> {
    let db = match &state.connection {
        Some(conn) => conn.db_type(),
        None => state.current_connection_config.as_ref()?.db_type.clone(),
    };
    (!db.is_sql()).then_some(db)
}

/// Whether the open connection is Redis.
pub fn is_redis(state: &AppState) -> bool {
    engine(state) == Some(DatabaseType::Redis)
}

/// The index of a Redis database listed as `db3`.
pub fn redis_db(schema: &str) -> Option<u8> {
    redis::db_index(schema)
}

// ---------------------------------------------------------------- commands

/// The console command emptying a collection (`deleteMany({})`).
pub fn clear_command(coll: &str) -> String {
    mongo::shell::command_to_shell(&mongo::shell::MongoCommand::Delete {
        coll: coll.to_string(),
        filter: Document::new(),
        many: true,
    })
}

/// The console command Enter on a collection runs.
pub fn collection_command(coll: &str) -> String {
    let name = serde_json::to_string(coll).unwrap_or_else(|_| format!("\"{coll}\""));
    format!("db.getCollection({name}).find({{}}).limit(100)")
}

/// The console command reading a key of type `kind`; `None` for a module
/// type (no generic read).
pub fn key_read_command(key: &[u8], kind: &RedisType) -> Option<String> {
    let k = redis::cli::quote_arg(key);
    Some(match kind {
        RedisType::String => format!("GET {k}"),
        RedisType::Hash => format!("HGETALL {k}"),
        RedisType::List => format!("LRANGE {k} 0 99"),
        RedisType::Set => format!("SSCAN {k} 0 COUNT 100"),
        RedisType::ZSet => format!("ZRANGE {k} 0 99 WITHSCORES"),
        RedisType::Stream => format!("XRANGE {k} - + COUNT 100"),
        RedisType::Other(_) => return None,
    })
}

/// The text of an argv (debug mode, previews).
pub fn argv_text(argv: &[Vec<u8>]) -> String {
    argv.iter()
        .map(|a| redis::cli::quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- key tree

/// The lines of a key tree: groups (folded unless in `expanded`) then keys.
pub fn visible_rows(nodes: &[KeyNode], expanded: &HashSet<String>) -> Vec<KeyRow> {
    fn walk(nodes: &[KeyNode], depth: usize, expanded: &HashSet<String>, out: &mut Vec<KeyRow>) {
        for node in nodes {
            if !node.children.is_empty() {
                let open = expanded.contains(&node.prefix);
                out.push(KeyRow::Group {
                    depth,
                    label: node.label.clone(),
                    prefix: node.prefix.clone(),
                    count: node.count,
                    open,
                });
                if open {
                    walk(&node.children, depth + 1, expanded, out);
                }
            } else if let Some(index) = node.key {
                out.push(KeyRow::Key {
                    depth,
                    label: node.label.clone(),
                    index,
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, expanded, &mut out);
    out
}

/// Short tag of a key type.
pub fn type_tag(kind: &RedisType) -> &'static str {
    match kind {
        RedisType::String => "[S]",
        RedisType::List => "[L]",
        RedisType::Set => "[E]",
        RedisType::ZSet => "[Z]",
        RedisType::Hash => "[H]",
        RedisType::Stream => "[X]",
        RedisType::Other(_) => "[?]",
    }
}

/// The text of a line of the key tree in the Tables panel.
pub fn row_text(row: &KeyRow, keys: &[KeyInfo]) -> String {
    match row {
        KeyRow::Group {
            depth,
            label,
            count,
            open,
            ..
        } => {
            let icon = if *open { "▼" } else { "▶" };
            format!("{}{icon} {label} ({count})", "  ".repeat(*depth))
        }
        KeyRow::Key {
            depth,
            label,
            index,
        } => {
            let info = keys.get(*index);
            let tag = info.map_or("[?]", |k| type_tag(&k.kind));
            let ttl = info
                .and_then(|k| k.ttl)
                .map(|s| format!(" ⏱{s}s"))
                .unwrap_or_default();
            format!("{}{tag} {label}{ttl}", "  ".repeat(*depth))
        }
        KeyRow::More => format!("… plus de {MAX_KEYS} clés : affiner le motif avec /"),
    }
}

/// Rebuild the lines of the Redis database at `schema_idx` (its `tables`
/// are the visible lines of its key tree).
pub fn sync_redis_schema(state: &mut AppState, schema_idx: usize) {
    let Some(schema) = state.schemas.get_mut(schema_idx) else {
        return;
    };
    let Some(keys) = redis_db(&schema.name).and_then(|db| state.nosql.redis.get_mut(&db)) else {
        schema.tables.clear();
        return;
    };
    keys.rows = visible_rows(&keys.tree, &keys.expanded);
    if keys.truncated {
        keys.rows.push(KeyRow::More);
    }
    schema.tables = keys.rows.iter().map(|r| row_text(r, &keys.keys)).collect();
}

/// Set the keys loaded for the database at `schema_idx`.
pub fn set_redis_keys(
    state: &mut AppState,
    schema_idx: usize,
    pattern: &str,
    mut keys: Vec<KeyInfo>,
    truncated: bool,
) {
    let Some(db) = state
        .schemas
        .get(schema_idx)
        .and_then(|s| redis_db(&s.name))
    else {
        return;
    };
    keys.truncate(MAX_KEYS);
    let tree = redis::key_tree(&keys, KEY_SEPARATOR);
    // The groups unfolded stay so.
    let expanded = state
        .nosql
        .redis
        .remove(&db)
        .map(|k| k.expanded)
        .unwrap_or_default();
    state.nosql.redis.insert(
        db,
        RedisKeys {
            pattern: pattern.to_string(),
            keys,
            tree,
            expanded,
            rows: Vec::new(),
            truncated,
        },
    );
    sync_redis_schema(state, schema_idx);
}

/// The line of the key tree under the cursor of the Tables panel.
pub fn selected_key_row(state: &AppState) -> Option<(u8, KeyRow)> {
    let schema = state.schemas.get(state.selected_schema)?;
    let db = redis_db(&schema.name)?;
    let row = state
        .nosql
        .redis
        .get(&db)?
        .rows
        .get(state.selected_table.checked_sub(1)?)?
        .clone();
    Some((db, row))
}

/// Fold or unfold the group `prefix` of database `db`.
pub fn toggle_group(state: &mut AppState, db: u8, prefix: &str) {
    if let Some(keys) = state.nosql.redis.get_mut(&db) {
        if !keys.expanded.remove(prefix) {
            keys.expanded.insert(prefix.to_string());
        }
    }
    let idx = state.selected_schema;
    sync_redis_schema(state, idx);
}

/// Read how many keys each Redis database holds (shown before their keys
/// are scanned); unknown when the server refuses `INFO`.
pub async fn load_key_counts(state: &mut AppState) {
    let Some(conn) = state.connection.clone() else {
        return;
    };
    state.nosql.key_counts = match conn.redis() {
        Ok(c) => redis::key_counts(c).await.ok(),
        Err(_) => None,
    };
}

/// The number of keys shown next to Redis database `db`: the keys loaded,
/// else its `INFO keyspace` count.
pub fn key_count(state: &AppState, db: u8) -> Option<u64> {
    if let Some(k) = state.nosql.redis.get(&db) {
        return Some(k.keys.len() as u64);
    }
    state
        .nosql
        .key_counts
        .as_ref()
        .map(|c| c.get(&db).copied().unwrap_or(0))
}

/// SCAN `pattern` in database `db` until done or `MAX_KEYS` keys.
async fn scan_keys(
    conn: &DatabaseConnection,
    db: u8,
    pattern: &str,
) -> anyhow::Result<(Vec<KeyInfo>, bool)> {
    let c = conn.redis()?;
    let mut keys = Vec::new();
    let mut cursor = 0;
    loop {
        let page = redis::scan(c, db, pattern, None, cursor, SCAN_COUNT).await?;
        keys.extend(page.keys);
        cursor = page.cursor;
        if cursor == 0 || keys.len() >= MAX_KEYS {
            break;
        }
    }
    Ok((keys, cursor != 0))
}

/// Load the keys of the Redis database at `schema_idx` matching `pattern`.
pub async fn load_keys(state: &mut AppState, schema_idx: usize, pattern: &str) {
    let Some(conn) = state.connection.clone() else {
        return;
    };
    let Some(name) = state.schemas.get(schema_idx).map(|s| s.name.clone()) else {
        return;
    };
    let Some(db) = redis_db(&name) else {
        return;
    };
    let pattern = if pattern.trim().is_empty() {
        "*"
    } else {
        pattern.trim()
    };
    state.set_status(format!("Lecture des clés de {name} ({pattern})…"));
    match scan_keys(&conn, db, pattern).await {
        Ok((keys, truncated)) => {
            let n = keys.len();
            set_redis_keys(state, schema_idx, pattern, keys, truncated);
            if let Some(schema) = state.schemas.get_mut(schema_idx) {
                schema.expanded = true;
            }
            let more = if truncated {
                format!(" (limite de {MAX_KEYS} atteinte)")
            } else {
                String::new()
            };
            state.set_status(format!("{n} clé(s) dans {name} pour {pattern}{more}"));
        }
        Err(e) => state.set_status(format!("Clés de {name} : {e}")),
    }
}

// ---------------------------------------------------------------- Tables panel

/// Enter (or a double click) in the Tables panel of a NoSQL connection.
pub async fn tables_enter(state: &mut AppState) {
    if state.selected_table == 0 {
        state.toggle_schema();
        let idx = state.selected_schema;
        let expanded = state.schemas.get(idx).is_some_and(|s| s.expanded);
        let unloaded = state
            .schemas
            .get(idx)
            .and_then(|s| redis_db(&s.name))
            .is_some_and(|db| !state.nosql.redis.contains_key(&db));
        if is_redis(state) && expanded && unloaded {
            load_keys(state, idx, "*").await;
        }
        return;
    }
    match engine(state) {
        Some(DatabaseType::MongoDB) => open_collection(state).await,
        Some(DatabaseType::Redis) => match selected_key_row(state) {
            Some((db, KeyRow::Group { prefix, .. })) => toggle_group(state, db, &prefix),
            Some((db, KeyRow::Key { index, .. })) => open_key(state, db, index).await,
            Some((_, KeyRow::More)) => {
                state.set_status("Trop de clés : affiner le motif avec /");
            }
            None => {}
        },
        _ => {}
    }
}

/// The collection under the cursor of the Tables panel.
pub fn selected_collection(state: &AppState) -> Option<CollectionRef> {
    let table = state.selected_table_ref()?;
    Some(CollectionRef {
        db: table.schema?,
        coll: table.name,
    })
}

/// Set the database of the current console.
fn set_scope(state: &mut AppState, database: String) {
    state.query_tabs.current_tab_mut().nosql_scope = Some(database);
}

/// Enter on a collection: its first documents, from the console.
pub async fn open_collection(state: &mut AppState) {
    let Some(coll) = selected_collection(state) else {
        return;
    };
    // Its fields for the completion (sampled in the background).
    crate::tui::columns::request(
        state,
        &TableRef {
            schema: Some(coll.db.clone()),
            name: coll.coll.clone(),
            alias: None,
        },
    );
    set_scope(state, coll.db.clone());
    let command = collection_command(&coll.coll);
    state.set_query(command.clone());
    run_now(state, command, None).await;
}

/// Enter on a key: the read command of its type, from the console.
pub async fn open_key(state: &mut AppState, db: u8, index: usize) {
    let Some(info) = state
        .nosql
        .redis
        .get(&db)
        .and_then(|k| k.keys.get(index))
        .cloned()
    else {
        return;
    };
    let Some(command) = key_read_command(&info.key, &info.kind) else {
        let kind = match &info.kind {
            RedisType::Other(t) => t.clone(),
            _ => String::new(),
        };
        state.set_status(format!("Type {kind} : pas de lecture générique"));
        return;
    };
    set_scope(state, db.to_string());
    state.set_query(command.clone());
    run_now(state, command, None).await;
}

/// `s` on a NoSQL connection: the indexes of a collection (MongoDB).
pub async fn tables_schema_key(state: &mut AppState) {
    match engine(state) {
        Some(DatabaseType::MongoDB) => match selected_collection(state) {
            Some(coll) => open_indexes(state, coll).await,
            None => state.set_status("Sélectionner une collection (pas une base)"),
        },
        Some(db) => state.set_status(format!("Pas de schéma ni d'index pour {db}")),
        None => {}
    }
}

/// `t` on a NoSQL connection: empty the collection (MongoDB) or the
/// database (Redis, FLUSHDB), after an `(o/N)` confirmation.
pub fn tables_truncate_key(state: &mut AppState) {
    match engine(state) {
        Some(DatabaseType::MongoDB) => match selected_collection(state) {
            Some(coll) => ask(
                state,
                format!("Vider la collection {coll} ?"),
                vec!["Supprime tous ses documents (deleteMany({}))".into()],
                ConfirmAction::ClearCollection(coll),
            ),
            None => state.set_status("Sélectionner une collection"),
        },
        Some(DatabaseType::Redis) => {
            let Some(schema) = state.schemas.get(state.selected_schema) else {
                return;
            };
            match (state.selected_table, redis_db(&schema.name)) {
                (0, Some(db)) => ask(
                    state,
                    format!("Vider la base {} (FLUSHDB) ?", schema.name),
                    vec!["Supprime toutes les clés de la base".into()],
                    ConfirmAction::FlushDb(db),
                ),
                _ => state.set_status("Sur une base (db0…) : t la vide (FLUSHDB)"),
            }
        }
        _ => {}
    }
}

/// The message of a SQL-only action asked of a NoSQL connection.
pub fn sql_only(state: &mut AppState, action: &str) {
    if let Some(db) = engine(state) {
        state.set_status(format!("{action} : indisponible pour {db}"));
    }
}

/// `n` in the Tables panel of Redis: a new key in the database under the
/// cursor.
pub fn tables_new_key(state: &mut AppState) {
    let Some(db) = state
        .schemas
        .get(state.selected_schema)
        .and_then(|s| redis_db(&s.name))
    else {
        state.set_status("Sélectionner une base (db0…)");
        return;
    };
    open_form(state, Form::new_key(db));
}

/// `/` in the Tables panel of Redis: the SCAN pattern of the database under
/// the cursor.
pub fn tables_pattern(state: &mut AppState) {
    let idx = state.selected_schema;
    let Some(db) = state.schemas.get(idx).and_then(|s| redis_db(&s.name)) else {
        return;
    };
    let pattern = state
        .nosql
        .redis
        .get(&db)
        .map_or_else(|| "*".to_string(), |k| k.pattern.clone());
    open_form(state, Form::pattern(idx, db, &pattern));
}

// ---------------------------------------------------------------- console

/// The database of the current console: the tab's when it suits the
/// engine, else the connection's default.
pub fn current_scope(state: &AppState) -> NoSqlScope {
    let initial = state
        .current_connection_config
        .as_ref()
        .map(NoSqlScope::initial)
        .unwrap_or(NoSqlScope {
            database: String::new(),
        });
    let tab = state.query_tabs.current_tab().nosql_scope.clone();
    match (engine(state), tab) {
        (Some(DatabaseType::Redis), Some(db)) if db.parse::<u8>().is_ok() => {
            NoSqlScope { database: db }
        }
        (Some(DatabaseType::MongoDB), Some(db)) => NoSqlScope { database: db },
        _ => initial,
    }
}

/// The unit to run for the cursor (`nosql::unit_at`: the one holding it,
/// or ending right before it); `None` when the text has none (blank,
/// comments).
pub fn unit_range(db: &DatabaseType, text: &str, cursor: usize) -> Option<Range<usize>> {
    nosql::unit_at(db, text, cursor.min(text.len()))
}

/// What F5 / Ctrl+Enter run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunWhat {
    All,
    AtCursor,
}

/// F5 / Ctrl+Enter on a NoSQL connection: the destructive commands are
/// confirmed first `(o/N)`.
pub async fn execute(state: &mut AppState, what: RunWhat) {
    let Some(db) = engine(state) else {
        return;
    };
    if state.connection.is_none() {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    }
    let text = state.query_input().to_string();
    let range = match what {
        RunWhat::All => None,
        RunWhat::AtCursor => match unit_range(&db, &text, state.cursor_position()) {
            Some(r) => Some(r),
            None => {
                state.set_status("Rien à exécuter au curseur");
                return;
            }
        },
    };
    if text.trim().is_empty() {
        state.set_status("Rien à exécuter");
        return;
    }
    if ask_warnings(state, &db, &text, range.clone()) {
        return;
    }
    run_now(state, text, range).await;
}

/// Open the confirmation of the destructive commands of `range`; returns
/// whether there were some.
pub fn ask_warnings(
    state: &mut AppState,
    db: &DatabaseType,
    text: &str,
    range: Option<Range<usize>>,
) -> bool {
    let warnings = nosql::warnings(db, text, range.clone());
    if warnings.is_empty() {
        return false;
    }
    let lines = warnings
        .iter()
        .map(|w| format!("{} : {}", w.reason, w.unit.trim()))
        .collect();
    ask(
        state,
        format!("{} commande(s) destructive(s) : exécuter ?", warnings.len()),
        lines,
        ConfirmAction::Run {
            text: text.to_string(),
            range,
        },
    );
    true
}

/// Run `range` of `text` (all when `None`) in the current console's
/// database, which follows `use` / `SELECT`.
pub async fn run_now(state: &mut AppState, text: String, range: Option<Range<usize>>) {
    let Some(conn) = state.connection.clone() else {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    };
    let mut scope = current_scope(state);
    state.set_status("Exécution…");
    state.is_loading = true;
    let outcomes = nosql::run_script(&conn, &text, range, &mut scope, MAX_ROWS).await;
    state.is_loading = false;
    set_scope(state, scope.database);
    apply_outcomes(state, outcomes);
}

/// Show the outcomes of a run: the last result (editable with its
/// `Source`), or the first error.
pub fn apply_outcomes(state: &mut AppState, outcomes: Vec<NoSqlOutcome>) {
    let total = outcomes.len();
    let mut shown = None;
    let mut error = None;
    for (i, o) in outcomes.into_iter().enumerate() {
        match o.outcome.result {
            Ok(result) => shown = Some((result, (o.source, o.read_only), o.outcome.elapsed_ms)),
            Err(e) => {
                error = Some((i, e));
                break;
            }
        }
    }
    let mut status = match &shown {
        Some((result, _, ms)) => {
            let what = if result.columns.is_empty() {
                format!("{} élément(s) modifié(s)", result.rows_affected)
            } else {
                let more = if result.truncated {
                    format!(" (limité à {MAX_ROWS})")
                } else {
                    String::new()
                };
                format!("{} ligne(s){more}", result.rows.len())
            };
            format!("{total} commande(s) exécutée(s) : {what} en {ms} ms")
        }
        None => String::new(),
    };
    if let Some((result, (source, read_only), _)) = shown {
        super::show_result(state, result, None);
        state.nosql.source = source;
        state.nosql.read_only = read_only;
    }
    if let Some((i, e)) = error {
        status = if total > 1 {
            format!("Erreur (commande {}/{total}) : {e}", i + 1)
        } else {
            format!("Erreur : {e}")
        };
    }
    if total == 0 {
        status = "Rien à exécuter".into();
    }
    state.set_status(status);
}

/// NoSQL completion of the editor (`nosql::complete`); the sampled fields
/// of the collections cited are loaded in the background (MongoDB).
pub fn completions(state: &mut AppState) -> Vec<Suggestion> {
    let Some(db) = engine(state) else {
        return Vec::new();
    };
    let text = state.query_input().to_string();
    let cursor = state.cursor_position();
    let scope = current_scope(state);
    let keys: Vec<String> = scope
        .database
        .parse::<u8>()
        .ok()
        .and_then(|n| state.nosql.redis.get(&n))
        .map(|k| {
            k.keys
                .iter()
                .map(|i| redis::display_bytes(&i.key))
                .collect()
        })
        .unwrap_or_default();
    let out = {
        let store = &state.completion_columns;
        let columns = |t: &TableRef| store.get(t);
        let cx = nosql::CompletionContext {
            scope: &scope,
            schemas: &state.schemas,
            columns: &columns,
            keys: &keys,
        };
        nosql::complete(&db, &text, cursor, &cx)
    };
    if db == DatabaseType::MongoDB {
        for table in &out.missing {
            crate::tui::columns::request(state, table);
        }
    }
    out.items
}

/// Placeholder of the empty editor.
pub fn editor_placeholder(db: &DatabaseType) -> &'static str {
    match db {
        DatabaseType::MongoDB => "// db.collection.find({}) — use <base> pour changer de base",
        _ => "# GET clé — SELECT n pour changer de base",
    }
}

// ---------------------------------------------------------------- confirmations

/// An `(o/N)` question.
#[derive(Clone, Debug, PartialEq)]
pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    pub action: ConfirmAction,
}

/// What a confirmed question does.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmAction {
    /// Run console commands with warnings.
    Run {
        text: String,
        range: Option<Range<usize>>,
    },
    /// Delete the selected row of the result (document, element, key).
    DeleteRow,
    ClearCollection(CollectionRef),
    FlushDb(u8),
    DropIndex {
        coll: CollectionRef,
        name: String,
    },
}

/// Ask `title` `(o/N)`.
pub fn ask(state: &mut AppState, title: String, lines: Vec<String>, action: ConfirmAction) {
    state.nosql.confirm = Some(Confirm {
        title,
        lines,
        action,
    });
    state.dialog_mode = DialogMode::NoSqlConfirm;
}

/// The answer a key gives to an `(o/N)` question: `o` / `y` yes; `n`, Esc
/// and Enter no (the default); `None` for the other keys.
pub fn confirm_answer(key: KeyCode) -> Option<bool> {
    match key {
        KeyCode::Char('o' | 'O' | 'y' | 'Y') => Some(true),
        KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => Some(false),
        _ => None,
    }
}

/// A question answered no.
pub fn declined(state: &mut AppState) {
    let back_to_indexes = matches!(
        state.nosql.confirm.as_ref().map(|c| &c.action),
        Some(ConfirmAction::DropIndex { .. })
    );
    state.nosql.confirm = None;
    if back_to_indexes && state.nosql.indexes.is_some() {
        state.dialog_mode = DialogMode::Indexes;
    } else {
        state.close_dialog();
    }
    state.set_status("Annulé");
}

/// A question answered yes: do it.
pub async fn confirmed(state: &mut AppState) {
    let Some(confirm) = state.nosql.confirm.take() else {
        state.close_dialog();
        return;
    };
    match confirm.action {
        ConfirmAction::Run { text, range } => {
            state.close_dialog();
            run_now(state, text, range).await;
        }
        ConfirmAction::DeleteRow => {
            state.close_dialog();
            delete_row(state).await;
        }
        ConfirmAction::ClearCollection(coll) => {
            state.close_dialog();
            let Some(conn) = state.connection.clone() else {
                return;
            };
            let command = clear_command(&coll.coll);
            let mut scope = NoSqlScope {
                database: coll.db.clone(),
            };
            let out = nosql::run_script(&conn, &command, None, &mut scope, MAX_ROWS).await;
            match out.into_iter().next().map(|o| o.outcome.result) {
                Some(Ok(r)) => state.set_status(format!(
                    "Collection {coll} vidée ({} document(s) supprimé(s))",
                    r.rows_affected
                )),
                Some(Err(e)) => state.set_status(format!("Vider {coll} : {e}")),
                None => {}
            }
        }
        ConfirmAction::FlushDb(db) => {
            state.close_dialog();
            let Some(conn) = state.connection.clone() else {
                return;
            };
            let flushed = match conn.redis() {
                Ok(c) => redis::flush_db(c, db).await,
                Err(e) => Err(e),
            };
            match flushed {
                Ok(()) => {
                    state.nosql.redis.remove(&db);
                    let idx = state.selected_schema;
                    sync_redis_schema(state, idx);
                    state.set_status(format!("Base db{db} vidée"));
                }
                Err(e) => state.set_status(format!("FLUSHDB db{db} : {e}")),
            }
        }
        ConfirmAction::DropIndex { coll, name } => {
            state.dialog_mode = DialogMode::Indexes;
            let Some(conn) = state.connection.clone() else {
                return;
            };
            let dropped = match conn.mongo() {
                Ok(c) => mongo::drop_index(c, &coll, &name).await,
                Err(e) => Err(e),
            };
            match dropped {
                Ok(()) => state.set_status(format!("Index {name} supprimé")),
                Err(e) => state.set_status(format!("Suppression de l'index {name} : {e}")),
            }
            reload_indexes(state).await;
        }
    }
}

// ---------------------------------------------------------------- row edits

/// Text of a cell in the row editor: a missing field is empty, null is
/// `null` (shell syntax).
pub fn edit_text(cell: &str) -> String {
    if is_absent(cell) {
        String::new()
    } else if is_null(cell) {
        "null".into()
    } else {
        cell.to_string()
    }
}

/// Cell from the editor's `text`: unchanged text keeps `original` (a
/// missing field stays missing, null stays null).
pub fn cell_from_text(text: &str, original: &str) -> String {
    if text == edit_text(original) {
        original.to_string()
    } else {
        text.to_string()
    }
}

/// The pending edit of one row as `RowChanges` (`mongo::doc_changes`):
/// `old` → `new` (an update), `new` alone (an insert: empty cells are left
/// out) or `old` alone (a delete).
pub fn row_changes(old: Option<&[String]>, new: Option<&[String]>) -> RowChanges {
    let mut changes = RowChanges::default();
    match (old, new) {
        (Some(o), Some(n)) => changes.updates.push((o.to_vec(), n.to_vec())),
        (None, Some(n)) => changes.inserts.push(
            n.iter()
                .map(|t| (!t.is_empty()).then(|| t.clone()))
                .collect(),
        ),
        (Some(o), None) => changes.deletes.push(o.to_vec()),
        (None, None) => {}
    }
    changes
}

/// The edits of one row of a Redis value shown by the console (see
/// `row_changes`), by `redis::console_edits`; deleting the row of a string
/// deletes the key.
pub fn key_edits(
    key: &[u8],
    kind: &RedisType,
    result: &QueryResult,
    old: Option<&[String]>,
    new: Option<&[String]>,
) -> Result<Vec<KeyEdit>, String> {
    redis::console_edits(key, kind, result, &row_changes(old, new))
}

/// The text of an edit in debug mode: its commands, one per line.
pub fn edit_preview(key: &[u8], edit: &KeyEdit) -> String {
    redis::edit_to_commands(key, edit)
        .iter()
        .map(|argv| argv_text(argv))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of a document change in debug mode.
pub fn change_preview(coll: &CollectionRef, change: &DocChange) -> String {
    mongo::change_to_shell(coll, change)
}

/// The selected row of the result and its source, when it can be edited.
fn editable(state: &mut AppState) -> Option<(Source, QueryResult)> {
    let result = state.query_result.clone()?;
    match state.nosql.source.clone() {
        Some(source) => Some((source, result)),
        None => {
            let why = state.nosql.read_only.clone().unwrap_or_else(|| {
                "Résultat en lecture seule : ouvrir une collection ou une clé (Entrée dans le panneau)"
                    .into()
            });
            state.set_status(why);
            None
        }
    }
}

/// Enter / `a` in the Results panel of a NoSQL connection: the row editor
/// on the selected row, or on a new one.
pub fn open_row_editor(state: &mut AppState, add: bool) {
    let Some((source, result)) = editable(state) else {
        return;
    };
    if let Source::Key { kind, .. } = &source {
        if let RedisType::Other(t) = kind {
            state.set_status(format!("Type {t} en lecture seule"));
            return;
        }
        if add && *kind == RedisType::String {
            state.set_status("Une chaîne n'a qu'une valeur : Entrée pour la modifier");
            return;
        }
    }
    // The `#` of a list is its position: not typed.
    state.system_columns = result
        .columns
        .iter()
        .position(|c| c.name == "#")
        .into_iter()
        .collect();
    let label = match &source {
        Source::Collection { coll, .. } => coll.to_string(),
        Source::Key { db, key, .. } => format!("db{db} {}", redis::display_bytes(key)),
    };
    if add {
        state.editing_row = Some(vec![String::new(); result.columns.len()]);
        state.original_editing_row = None;
        state.editing_column = (0..result.columns.len())
            .find(|i| !state.system_columns.contains(i))
            .unwrap_or(0);
        state.editing_cursor = 0;
        state.editing_table_name = Some(label);
        state.dialog_mode = DialogMode::AddRow;
    } else {
        let Some(row) = result.rows.get(state.selected_row) else {
            return;
        };
        state.editing_row = Some(row.iter().map(|c| edit_text(c)).collect());
        state.original_editing_row = Some(row.clone());
        state.editing_column = 0;
        state.editing_cursor = state
            .editing_row
            .as_ref()
            .and_then(|r| r.first())
            .map_or(0, String::len);
        state.editing_table_name = Some(label);
        state.dialog_mode = DialogMode::EditRow;
    }
}

/// `d` in the Results panel of a NoSQL connection: confirm the deletion.
pub fn ask_delete_row(state: &mut AppState) {
    let Some((source, result)) = editable(state) else {
        return;
    };
    if result.rows.get(state.selected_row).is_none() {
        return;
    }
    let what = match &source {
        Source::Collection { coll, .. } => format!("Supprimer ce document de {coll} ?"),
        Source::Key {
            key,
            kind: RedisType::String,
            ..
        } => format!("Supprimer la clé {} ?", redis::display_bytes(key)),
        Source::Key { key, .. } => {
            format!("Supprimer cet élément de {} ?", redis::display_bytes(key))
        }
    };
    ask(state, what, Vec::new(), ConfirmAction::DeleteRow);
}

/// Where a row edit is written.
enum Target {
    Docs {
        coll: CollectionRef,
        page: DocPage,
    },
    Key {
        db: u8,
        key: Vec<u8>,
        kind: RedisType,
        /// The grid shown (`redis::value_result` shape).
        result: QueryResult,
    },
}

fn target(state: &AppState) -> Option<Target> {
    let result = state.query_result.clone()?;
    Some(match state.nosql.source.clone()? {
        Source::Collection { coll, docs } => Target::Docs {
            coll,
            page: DocPage { result, docs },
        },
        Source::Key { db, key, kind } => Target::Key {
            db,
            key,
            kind,
            result,
        },
    })
}

/// Write `old` → `new` (see `row_changes`); returns whether it was
/// applied (not in debug mode, which shows the commands in the editor).
async fn write_row(
    state: &mut AppState,
    old: Option<Vec<String>>,
    new: Option<Vec<String>>,
) -> bool {
    let Some(target) = target(state) else {
        return false;
    };
    let Some(conn) = state.connection.clone() else {
        state.set_status("Not connected");
        return false;
    };
    match target {
        Target::Docs { coll, page } => {
            let changes = row_changes(old.as_deref(), new.as_deref());
            let changes = match mongo::doc_changes(&page, &changes) {
                Ok(c) => c,
                Err(e) => {
                    state.set_status(format!("Modification refusée : {e}"));
                    return false;
                }
            };
            if changes.is_empty() {
                state.set_status("Aucune modification");
                return false;
            }
            if state.debug_mode {
                let text = changes
                    .iter()
                    .map(|c| change_preview(&coll, c))
                    .collect::<Vec<_>>()
                    .join(";\n");
                state.set_query(text);
                state.set_status("Debug : commandes copiées dans l'éditeur (non exécutées)");
                return false;
            }
            let applied = match conn.mongo() {
                Ok(c) => mongo::apply_changes(c, &coll, &changes).await,
                Err(e) => Err(e),
            };
            match applied {
                Ok(n) => {
                    state.set_status(format!("{n} modification(s) appliquée(s) à {coll}"));
                    true
                }
                Err(e) => {
                    state.set_status(format!("Échec : {e}"));
                    false
                }
            }
        }
        Target::Key {
            db,
            key,
            kind,
            result,
        } => {
            let edits = match key_edits(&key, &kind, &result, old.as_deref(), new.as_deref()) {
                Ok(e) => e,
                Err(e) => {
                    state.set_status(format!("Modification refusée : {e}"));
                    return false;
                }
            };
            if edits.is_empty() {
                state.set_status("Aucune modification");
                return false;
            }
            if state.debug_mode {
                let text = edits
                    .iter()
                    .map(|e| edit_preview(&key, e))
                    .collect::<Vec<_>>()
                    .join("\n");
                state.set_query(text);
                state.set_status("Debug : commandes copiées dans l'éditeur (non exécutées)");
                return false;
            }
            let applied = match conn.redis() {
                Ok(c) => redis::apply_edits(c, db, &key, &edits).await,
                Err(e) => Err(e),
            };
            match applied {
                Ok(n) => {
                    state.set_status(format!(
                        "{n} modification(s) appliquée(s) à {}",
                        redis::display_bytes(&key)
                    ));
                    true
                }
                Err(e) => {
                    state.set_status(format!("Échec : {e}"));
                    false
                }
            }
        }
    }
}

/// Enter in the row editor of a NoSQL result.
pub async fn save_row(state: &mut AppState) {
    let (Some(old), Some(texts)) = (
        state.original_editing_row.clone(),
        state.editing_row.clone(),
    ) else {
        state.close_dialog();
        return;
    };
    let new: Vec<String> = texts
        .iter()
        .zip(&old)
        .map(|(t, o)| cell_from_text(t, o))
        .collect();
    state.close_dialog();
    if new == old {
        state.set_status("Aucune modification");
        return;
    }
    if write_row(state, Some(old), Some(new.clone())).await {
        let status = state.status_message.clone();
        if let Some(row) = state
            .query_result
            .as_mut()
            .and_then(|r| r.rows.get_mut(state.selected_row))
        {
            *row = new;
        }
        state.set_status(format!("{status} — F5 pour recharger"));
    }
}

/// Enter in the add-row editor of a NoSQL result.
pub async fn insert_row(state: &mut AppState) {
    let Some(new) = state.editing_row.clone() else {
        state.close_dialog();
        return;
    };
    state.close_dialog();
    if write_row(state, None, Some(new)).await {
        let status = state.status_message.clone();
        state.set_status(format!("{status} — F5 pour recharger"));
    }
}

/// Delete the selected row (confirmed).
pub async fn delete_row(state: &mut AppState) {
    let Some(old) = state
        .query_result
        .as_ref()
        .and_then(|r| r.rows.get(state.selected_row))
        .cloned()
    else {
        return;
    };
    if write_row(state, Some(old), None).await {
        let row = state.selected_row;
        if let Some(r) = state.query_result.as_mut() {
            if row < r.rows.len() {
                r.rows.remove(row);
            }
            state.selected_row = row.min(r.rows.len().saturating_sub(1));
        }
        if let Some(Source::Collection { docs, .. }) = state.nosql.source.as_mut() {
            if row < docs.len() {
                docs.remove(row);
            }
        }
    }
}

// ---------------------------------------------------------------- JSON view

/// A read-only text shown in a modal (`v`).
#[derive(Clone, Debug, PartialEq)]
pub struct JsonView {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

/// A document in shell syntax, indented.
pub fn document_text(doc: &Document) -> String {
    mongo::shell::to_shell(&Bson::Document(doc.clone()), true)
}

/// A cell as JSON: null, a JSON object / array it holds, else its text.
fn cell_json(cell: &str) -> serde_json::Value {
    if is_null(cell) {
        return serde_json::Value::Null;
    }
    let t = cell.trim();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            return v;
        }
    }
    serde_json::Value::String(cell.to_string())
}

/// A row as a JSON object (missing fields left out).
pub fn row_object(result: &QueryResult, row: &[String]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (col, cell) in result.columns.iter().zip(row) {
        if !is_absent(cell) {
            map.insert(col.name.clone(), cell_json(cell));
        }
    }
    serde_json::Value::Object(map)
}

/// The text `v` shows for row `row`: the whole document of a collection,
/// else the row as a JSON object.
pub fn row_text_view(result: &QueryResult, source: Option<&Source>, row: usize) -> Option<String> {
    let cells = result.rows.get(row)?;
    if let Some(Source::Collection { docs, .. }) = source {
        if let Some(doc) = docs.get(row) {
            return Some(document_text(doc));
        }
    }
    serde_json::to_string_pretty(&row_object(result, cells)).ok()
}

/// `v` in the Results panel: the selected document / row, indented.
pub fn open_json_view(state: &mut AppState) {
    let Some(result) = state.query_result.as_ref() else {
        state.set_status("Aucun résultat");
        return;
    };
    let Some(text) = row_text_view(result, state.nosql.source.as_ref(), state.selected_row) else {
        return;
    };
    let title = match &state.nosql.source {
        Some(Source::Collection { coll, .. }) => format!("Document de {coll}"),
        Some(Source::Key { key, .. }) => format!("Valeur de {}", redis::display_bytes(key)),
        None => format!("Ligne {}", state.selected_row + 1),
    };
    state.nosql.json = Some(JsonView {
        title,
        lines: text.lines().map(str::to_string).collect(),
        scroll: 0,
    });
    state.dialog_mode = DialogMode::JsonView;
}

/// Keys of the JSON view: scrolling, Esc / q / v close it.
pub fn json_view_key(state: &mut AppState, key: KeyCode) {
    let Some(view) = state.nosql.json.as_mut() else {
        state.close_dialog();
        return;
    };
    let max = view.lines.len().saturating_sub(1);
    match key {
        KeyCode::Esc | KeyCode::Char('q' | 'v') | KeyCode::Enter => state.close_dialog(),
        KeyCode::Down | KeyCode::Char('j') => view.scroll = (view.scroll + 1).min(max),
        KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
        KeyCode::PageDown => view.scroll = (view.scroll + 20).min(max),
        KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(20),
        KeyCode::Home => view.scroll = 0,
        KeyCode::End => view.scroll = max,
        _ => {}
    }
}

// ---------------------------------------------------------------- indexes

/// The indexes of a collection (`s`).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexesView {
    pub coll: CollectionRef,
    pub indexes: Vec<MongoIndex>,
    pub selected: usize,
    pub error: Option<String>,
}

/// One index as a line: name, keys and options.
pub fn index_line(i: &MongoIndex) -> String {
    let keys = compact_document(&i.keys);
    let mut flags = Vec::new();
    if i.unique {
        flags.push("unique".to_string());
    }
    if i.sparse {
        flags.push("sparse".to_string());
    }
    if let Some(ttl) = i.ttl {
        flags.push(format!("TTL {ttl} s"));
    }
    if let Some(p) = &i.partial {
        flags.push(format!("partiel {}", compact_document(p)));
    }
    if i.hidden {
        flags.push("caché".to_string());
    }
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!("  [{}]", flags.join(", "))
    };
    format!("{}  {keys}{flags}", i.name)
}

/// A document on one line, in shell syntax.
pub fn compact_document(doc: &Document) -> String {
    mongo::shell::to_shell(&Bson::Document(doc.clone()), false)
}

/// Open the indexes of `coll`.
pub async fn open_indexes(state: &mut AppState, coll: CollectionRef) {
    state.nosql.indexes = Some(IndexesView {
        coll,
        indexes: Vec::new(),
        selected: 0,
        error: None,
    });
    state.dialog_mode = DialogMode::Indexes;
    reload_indexes(state).await;
}

/// Read the indexes of the open view again.
pub async fn reload_indexes(state: &mut AppState) {
    let Some(conn) = state.connection.clone() else {
        return;
    };
    let Some(coll) = state.nosql.indexes.as_ref().map(|v| v.coll.clone()) else {
        return;
    };
    let listed = match conn.mongo() {
        Ok(c) => mongo::list_indexes(c, &coll).await,
        Err(e) => Err(e),
    };
    if let Some(view) = state.nosql.indexes.as_mut() {
        match listed {
            Ok(indexes) => {
                view.selected = view.selected.min(indexes.len().saturating_sub(1));
                view.indexes = indexes;
                view.error = None;
            }
            Err(e) => {
                view.indexes.clear();
                view.error = Some(e.to_string());
            }
        }
    }
}

/// What a key of the indexes view asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexKey {
    None,
    Reload,
}

/// Keys of the indexes view: ↑/↓, `n` new index, `d` drop (confirmed),
/// `r` reload, Esc close.
pub fn indexes_key(state: &mut AppState, key: KeyCode) -> IndexKey {
    let Some(view) = state.nosql.indexes.as_mut() else {
        state.close_dialog();
        return IndexKey::None;
    };
    match key {
        KeyCode::Esc | KeyCode::Char('q') => state.close_dialog(),
        KeyCode::Down | KeyCode::Char('j') if !view.indexes.is_empty() => {
            view.selected = (view.selected + 1) % view.indexes.len();
        }
        KeyCode::Up | KeyCode::Char('k') if !view.indexes.is_empty() => {
            view.selected = view
                .selected
                .checked_sub(1)
                .unwrap_or(view.indexes.len() - 1);
        }
        KeyCode::Char('n') => {
            let coll = view.coll.clone();
            open_form(state, Form::new_index(coll));
        }
        KeyCode::Char('d') | KeyCode::Delete => {
            let Some(index) = view.indexes.get(view.selected) else {
                return IndexKey::None;
            };
            if index.name == "_id_" {
                state.set_status("L'index _id_ ne se supprime pas");
                return IndexKey::None;
            }
            let (coll, name) = (view.coll.clone(), index.name.clone());
            ask(
                state,
                format!("Supprimer l'index {name} de {coll} ?"),
                Vec::new(),
                ConfirmAction::DropIndex { coll, name },
            );
        }
        KeyCode::Char('r') => return IndexKey::Reload,
        _ => {}
    }
    IndexKey::None
}

/// Whether a choice field says yes.
fn yes(text: &str) -> bool {
    matches!(
        text.trim().to_lowercase().as_str(),
        "oui" | "o" | "yes" | "y" | "true" | "1"
    )
}

/// Keys and options of a new index from the form's values (keys, name,
/// unique, sparse, TTL, partial filter); `parse` reads a document in shell
/// syntax (`mongo::shell::parse_document`).
pub fn index_spec(
    values: &[String],
    parse: &dyn Fn(&str) -> Result<Document, String>,
) -> Result<(Document, Document), String> {
    let get = |i: usize| values.get(i).map_or("", |s| s.trim());
    if get(0).is_empty() {
        return Err("Clés de l'index requises, par exemple {champ: 1}".into());
    }
    let keys = parse(get(0))?;
    if keys.is_empty() {
        return Err("Clés de l'index requises, par exemple {champ: 1}".into());
    }
    let mut options = Document::new();
    if !get(1).is_empty() {
        options.insert("name", get(1));
    }
    if yes(get(2)) {
        options.insert("unique", true);
    }
    if yes(get(3)) {
        options.insert("sparse", true);
    }
    if !get(4).is_empty() {
        let ttl: i64 = get(4)
            .parse()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| format!("TTL invalide : {} (secondes)", get(4)))?;
        options.insert("expireAfterSeconds", ttl);
    }
    if !get(5).is_empty() {
        options.insert("partialFilterExpression", parse(get(5))?);
    }
    Ok((keys, options))
}

// ---------------------------------------------------------------- forms

/// What a form is for.
#[derive(Clone, Debug, PartialEq)]
pub enum FormPurpose {
    NewIndex(CollectionRef),
    NewKey(u8),
    /// SCAN pattern of the database at this index of `schemas`.
    Pattern {
        schema: usize,
    },
}

/// A field of a form; with `choices`, ←/→ cycle its value.
#[derive(Clone, Debug, PartialEq)]
pub struct FormField {
    pub label: &'static str,
    pub value: String,
    pub hint: &'static str,
    pub choices: &'static [&'static str],
}

impl FormField {
    fn text(label: &'static str, hint: &'static str) -> Self {
        Self {
            label,
            value: String::new(),
            hint,
            choices: &[],
        }
    }

    fn choice(label: &'static str, choices: &'static [&'static str]) -> Self {
        Self {
            label,
            value: choices[0].to_string(),
            hint: "",
            choices,
        }
    }
}

/// Types offered for a new key.
pub const KEY_TYPES: &[&str] = &["string", "list", "set", "zset", "hash", "stream"];
const NO_YES: &[&str] = &["non", "oui"];

/// A small form (new index, new key, SCAN pattern).
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub title: String,
    pub fields: Vec<FormField>,
    pub active: usize,
    pub cursor: usize,
    pub purpose: FormPurpose,
}

impl Form {
    pub fn new_index(coll: CollectionRef) -> Self {
        Self {
            title: format!("Nouvel index sur {coll}"),
            fields: vec![
                FormField::text("Clés", "{champ: 1}"),
                FormField::text("Nom", "automatique"),
                FormField::choice("Unique", NO_YES),
                FormField::choice("Sparse", NO_YES),
                FormField::text("TTL (s)", "aucun"),
                FormField::text("Filtre partiel", "{champ: {$exists: true}}"),
            ],
            active: 0,
            cursor: 0,
            purpose: FormPurpose::NewIndex(coll),
        }
    }

    pub fn new_key(db: u8) -> Self {
        Self {
            title: format!("Nouvelle clé dans db{db}"),
            fields: vec![
                FormField::text("Clé", "user:42"),
                FormField::choice("Type", KEY_TYPES),
                FormField::text("Valeur", "texte ; JSON pour les autres types"),
                FormField::text("TTL (s)", "persistante"),
            ],
            active: 0,
            cursor: 0,
            purpose: FormPurpose::NewKey(db),
        }
    }

    pub fn pattern(schema: usize, db: u8, pattern: &str) -> Self {
        let mut field = FormField::text("Motif", "*");
        field.value = pattern.to_string();
        Self {
            title: format!("Clés de db{db} (motif SCAN)"),
            cursor: field.value.len(),
            fields: vec![field],
            active: 0,
            purpose: FormPurpose::Pattern { schema },
        }
    }

    pub fn values(&self) -> Vec<String> {
        self.fields.iter().map(|f| f.value.clone()).collect()
    }

    /// Hint of the value field of a new key, by type.
    pub fn value_hint(&self) -> &'static str {
        match self.fields.get(1).map(|f| f.value.as_str()) {
            _ if !matches!(self.purpose, FormPurpose::NewKey(_)) => "",
            Some("list") | Some("set") => r#"["a", "b"]"#,
            Some("zset") => r#"{"membre": 1.5}"#,
            Some("hash") => r#"{"champ": "valeur"}"#,
            Some("stream") => r#"{"champ": "valeur"} (une entrée)"#,
            _ => "texte",
        }
    }

    /// One key; returns `Some(true)` to submit, `Some(false)` to cancel.
    pub fn key(&mut self, key: KeyCode) -> Option<bool> {
        let n = self.fields.len();
        let field = &mut self.fields[self.active];
        match key {
            KeyCode::Esc => return Some(false),
            KeyCode::Enter => return Some(true),
            KeyCode::Tab | KeyCode::Down => {
                self.active = (self.active + 1) % n;
                self.cursor = self.fields[self.active].value.len();
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.active = self.active.checked_sub(1).unwrap_or(n - 1);
                self.cursor = self.fields[self.active].value.len();
            }
            KeyCode::Left | KeyCode::Right if !field.choices.is_empty() => {
                let len = field.choices.len();
                let i = field
                    .choices
                    .iter()
                    .position(|c| *c == field.value)
                    .unwrap_or(0);
                let i = if key == KeyCode::Right {
                    (i + 1) % len
                } else {
                    (i + len - 1) % len
                };
                field.value = field.choices[i].to_string();
            }
            _ if !field.choices.is_empty() => {}
            KeyCode::Char(c) => {
                field.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let prev = crate::tui::prev_char_boundary(&field.value, self.cursor);
                field.value.remove(prev);
                self.cursor = prev;
            }
            KeyCode::Delete if self.cursor < field.value.len() => {
                field.value.remove(self.cursor);
            }
            KeyCode::Left => {
                self.cursor = crate::tui::prev_char_boundary(&field.value, self.cursor)
            }
            KeyCode::Right => {
                self.cursor = crate::tui::next_char_boundary(&field.value, self.cursor)
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = field.value.len(),
            _ => {}
        }
        None
    }
}

/// Open `form` over the current dialog.
pub fn open_form(state: &mut AppState, form: Form) {
    state.nosql.form = Some(form);
    state.dialog_mode = DialogMode::NoSqlForm;
}

/// Leave a form: back to the indexes it was opened from, else closed.
fn leave_form(state: &mut AppState) {
    let form = state.nosql.form.take();
    if matches!(form.map(|f| f.purpose), Some(FormPurpose::NewIndex(_)))
        && state.nosql.indexes.is_some()
    {
        state.dialog_mode = DialogMode::Indexes;
    } else {
        state.close_dialog();
    }
}

/// Keys of a form; returns whether to submit it.
pub fn form_key(state: &mut AppState, key: KeyCode) -> bool {
    let Some(form) = state.nosql.form.as_mut() else {
        state.close_dialog();
        return false;
    };
    match form.key(key) {
        Some(true) => true,
        Some(false) => {
            leave_form(state);
            state.set_status("Annulé");
            false
        }
        None => false,
    }
}

/// The value of a new key from its type and the typed text: the text
/// itself for a string; JSON for the others (list / set: array; zset:
/// object member → score; hash: object; stream: object, one entry).
pub fn new_key_value(kind: &str, text: &str) -> Result<RedisValue, String> {
    let json = || -> Result<serde_json::Value, String> {
        serde_json::from_str(text.trim()).map_err(|e| format!("Valeur JSON invalide : {e}"))
    };
    let string = |v: &serde_json::Value| v.as_str().map_or_else(|| v.to_string(), str::to_string);
    let items = |v: serde_json::Value| -> Result<Vec<Vec<u8>>, String> {
        match v {
            serde_json::Value::Array(a) => Ok(a.iter().map(|x| string(x).into_bytes()).collect()),
            _ => Err("Un tableau JSON est attendu : [\"a\", \"b\"]".into()),
        }
    };
    let object =
        |v: serde_json::Value| -> Result<serde_json::Map<String, serde_json::Value>, String> {
            match v {
                serde_json::Value::Object(o) => Ok(o),
                _ => Err("Un objet JSON est attendu : {\"champ\": \"valeur\"}".into()),
            }
        };
    let value = match kind {
        "string" => RedisValue::String(text.as_bytes().to_vec()),
        "list" => RedisValue::List(items(json()?)?),
        "set" => RedisValue::Set(items(json()?)?),
        "zset" => RedisValue::ZSet(
            object(json()?)?
                .into_iter()
                .map(|(m, s)| {
                    s.as_f64()
                        .map(|s| (m.clone().into_bytes(), s))
                        .ok_or_else(|| format!("Score invalide pour {m}"))
                })
                .collect::<Result<_, _>>()?,
        ),
        "hash" => RedisValue::Hash(
            object(json()?)?
                .into_iter()
                .map(|(k, v)| (k.into_bytes(), string(&v).into_bytes()))
                .collect(),
        ),
        "stream" => RedisValue::Stream(vec![redis::StreamEntry {
            id: "*".into(),
            fields: object(json()?)?
                .into_iter()
                .map(|(k, v)| (k.into_bytes(), string(&v).into_bytes()))
                .collect(),
        }]),
        other => return Err(format!("Type inconnu : {other}")),
    };
    let empty = match &value {
        RedisValue::String(_) => false,
        RedisValue::List(v) | RedisValue::Set(v) => v.is_empty(),
        RedisValue::ZSet(v) => v.is_empty(),
        RedisValue::Hash(v) => v.is_empty(),
        RedisValue::Stream(v) => v.iter().all(|e| e.fields.is_empty()),
    };
    if empty {
        return Err("Une clé vide n'existe pas pour Redis : donner au moins un élément".into());
    }
    Ok(value)
}

/// A TTL typed in seconds: empty = persistent.
pub fn parse_ttl(text: &str) -> Result<Option<i64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .map(Some)
        .ok_or_else(|| format!("TTL invalide : {t} (secondes)"))
}

/// Enter in a form.
pub async fn submit_form(state: &mut AppState) {
    let Some(form) = state.nosql.form.clone() else {
        state.close_dialog();
        return;
    };
    let values = form.values();
    let Some(conn) = state.connection.clone() else {
        leave_form(state);
        return;
    };
    match form.purpose {
        FormPurpose::Pattern { schema } => {
            state.close_dialog();
            load_keys(state, schema, &values[0]).await;
        }
        FormPurpose::NewKey(db) => {
            let key = values[0].trim();
            if key.is_empty() {
                state.set_status("Nom de clé requis");
                return;
            }
            let spec = new_key_value(&values[1], &values[2])
                .and_then(|v| parse_ttl(&values[3]).map(|ttl| (v, ttl)));
            let (value, ttl) = match spec {
                Ok(s) => s,
                Err(e) => {
                    state.set_status(e);
                    return;
                }
            };
            let created = match conn.redis() {
                Ok(c) => redis::create_key(c, db, key.as_bytes(), &value, ttl).await,
                Err(e) => Err(e),
            };
            match created {
                Ok(()) => {
                    state.close_dialog();
                    state.set_status(format!("Clé {key} créée dans db{db}"));
                    if let Some(idx) = state
                        .schemas
                        .iter()
                        .position(|s| redis_db(&s.name) == Some(db))
                    {
                        let pattern = state
                            .nosql
                            .redis
                            .get(&db)
                            .map_or_else(|| "*".to_string(), |k| k.pattern.clone());
                        let status = state.status_message.clone();
                        load_keys(state, idx, &pattern).await;
                        state.set_status(status);
                    }
                }
                Err(e) => state.set_status(format!("Création de {key} : {e}")),
            }
        }
        FormPurpose::NewIndex(coll) => {
            let parse = |t: &str| mongo::shell::parse_document(t).map_err(|e| e.to_string());
            let (keys, options) = match index_spec(&values, &parse) {
                Ok(s) => s,
                Err(e) => {
                    state.set_status(e);
                    return;
                }
            };
            let created = match conn.mongo() {
                Ok(c) => mongo::create_index(c, &coll, keys, options).await,
                Err(e) => Err(e),
            };
            match created {
                Ok(name) => {
                    leave_form(state);
                    state.set_status(format!("Index {name} créé sur {coll}"));
                    reload_indexes(state).await;
                }
                Err(e) => state.set_status(format!("Création de l'index : {e}")),
            }
        }
    }
}

// ---------------------------------------------------------------- help

/// Help entries of the Tables panel of a NoSQL connection.
pub fn tables_help(db: &DatabaseType) -> Vec<(&'static str, &'static str)> {
    match db {
        DatabaseType::MongoDB => vec![
            ("Enter", "Documents"),
            ("Space", "Déplier"),
            ("s", "Index"),
            ("t", "Vider"),
            ("h/H", "Masquer/Voir"),
            ("E/I", "Exp/Imp JSON/JSONL/CSV"),
        ],
        _ => vec![
            ("Enter", "Ouvrir/Déplier"),
            ("/", "Motif SCAN"),
            ("n", "Nouvelle clé"),
            ("t", "FLUSHDB"),
            ("h/H", "Masquer/Voir"),
            ("E/I", "Exp/Imp JSON/CSV"),
        ],
    }
}

/// Help entries of the Results panel of a NoSQL connection.
pub fn results_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("/", "Filtre"),
        ("Enter", "Modifier"),
        ("a", "Ajouter"),
        ("d", "Supprimer"),
        ("v", "JSON"),
        ("x/F6", "Exporter"),
    ]
}

/// Help entries of the editor of a NoSQL connection.
pub fn editor_help() -> Vec<(&'static str, &'static str)> {
    vec![
        ("F5/Ctrl+G", "Tout exécuter"),
        ("Ctrl+↵/E", "Commande"),
        ("Ctrl+Space/K", "Compléter"),
        ("Ctrl+T", "Onglet"),
        ("Shift+F6/F7", "Exp/Imp"),
    ]
}

/// The `?` help of a NoSQL connection.
pub fn help_text(db: &DatabaseType) -> &'static str {
    match db {
        DatabaseType::MongoDB => {
            "Aide MongoDB : Entrée=documents d'une collection, s=index, t=vider la collection, \
             F5=tout exécuter, Ctrl+Entrée=commande au curseur, v=document JSON, \
             Entrée/a/d=modifier/ajouter/supprimer, E/I=export/import JSON, JSON Lines, CSV,              q=quitter"
        }
        _ => {
            "Aide Redis : Entrée=lire la clé / déplier, /=motif SCAN, n=nouvelle clé, \
             t=FLUSHDB, F5=tout exécuter, Ctrl+Entrée=ligne au curseur, v=valeur JSON, \
             Entrée/a/d=modifier/ajouter/supprimer, E/I=export/import JSON/CSV, q=quitter"
        }
    }
}

#[cfg(test)]
mod tests;
