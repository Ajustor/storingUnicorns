//! Columns of the tables cited by the query being typed, for the editor's
//! completion: loaded in the background (the event loop keeps running),
//! the suggestions are refreshed once they arrive.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::task::JoinHandle;

use crate::engine::db::DatabaseConnection;
use crate::engine::ops;
use crate::engine::services::ColumnDefinition;
use crate::engine::sql::completion::{ColumnInfo, TableRef};
use crate::engine::sql::statements::quote_ident;

use super::AppState;

/// Columns loaded for completion, keyed by `schema.table` (or `table`),
/// unquoted.
#[derive(Default)]
pub struct ColumnStore {
    loaded: HashMap<String, Vec<ColumnInfo>>,
    /// Tables being loaded.
    pending: Vec<(String, JoinHandle<anyhow::Result<Vec<ColumnDefinition>>>)>,
    /// Tables whose loading failed: not asked again for this connection.
    failed: HashSet<String>,
}

fn key(table: &TableRef) -> String {
    match &table.schema {
        Some(s) => format!("{s}.{}", table.name),
        None => table.name.clone(),
    }
}

impl ColumnStore {
    /// Columns of `table`, `None` when not loaded (yet). A table cited
    /// without its schema matches the one loaded with it.
    pub fn get(&self, table: &TableRef) -> Option<Vec<ColumnInfo>> {
        let k = key(table);
        let find = |matches: &dyn Fn(&str) -> bool| {
            self.loaded
                .iter()
                .find(|(name, _)| matches(name))
                .map(|(_, cols)| cols)
        };
        self.loaded
            .get(&k)
            .or_else(|| find(&|name| name.eq_ignore_ascii_case(&k)))
            .or_else(|| {
                table.schema.is_none().then_some(()).and_then(|()| {
                    find(&|name| {
                        name.rsplit_once('.')
                            .is_some_and(|(_, n)| n.eq_ignore_ascii_case(&table.name))
                    })
                })
            })
            .cloned()
    }

    /// Store the columns of `table` (unquoted `schema.table` or `table`).
    pub fn insert(&mut self, table: &str, columns: Vec<ColumnInfo>) {
        self.loaded.insert(table.to_string(), columns);
    }

    /// Forget everything (another connection, a schema changed).
    pub fn clear(&mut self) {
        for (_, task) in self.pending.drain(..) {
            task.abort();
        }
        self.loaded.clear();
        self.failed.clear();
    }
}

/// Load the columns of `table` in the background, unless they are loaded,
/// being loaded, failed before, or there is no connection.
pub fn request(state: &mut AppState, table: &TableRef) {
    // Cited without its schema: the one of the known table of that name.
    let mut table = table.clone();
    if table.schema.is_none() {
        table.schema = state
            .schemas
            .iter()
            .find(|s| s.tables.iter().any(|t| t.eq_ignore_ascii_case(&table.name)))
            .map(|s| s.name.clone());
    }
    let table = &table;
    let k = key(table);
    let store = &state.completion_columns;
    if store.get(table).is_some()
        || store.failed.contains(&k)
        || store.pending.iter().any(|(p, _)| *p == k)
    {
        return;
    }
    let Some(conn) = state.connection.clone() else {
        return;
    };
    let quotes = state.get_quote_chars();
    let name = match &table.schema {
        Some(s) => format!(
            "{}.{}",
            quote_ident(s, quotes),
            quote_ident(&table.name, quotes)
        ),
        None => quote_ident(&table.name, quotes),
    };
    let cache = state.table_cache.clone();
    let task = tokio::spawn(async move { ops::schema::fetch_columns(&conn, &cache, &name).await });
    state.completion_columns.pending.push((k, task));
}

/// Take the columns loaded since the last call; the suggestions shown (or
/// asked for) are refreshed with them.
pub async fn poll(state: &mut AppState) {
    let store = &mut state.completion_columns;
    if !store.pending.iter().any(|(_, task)| task.is_finished()) {
        return;
    }
    let (done, still): (Vec<_>, Vec<_>) = store
        .pending
        .drain(..)
        .partition(|(_, task)| task.is_finished());
    store.pending = still;
    for (k, task) in done {
        match task.await {
            Ok(Ok(cols)) => {
                let cols = cols
                    .into_iter()
                    .map(|c| ColumnInfo {
                        name: c.name,
                        type_name: c.data_type,
                        primary_key: c.is_primary_key,
                    })
                    .collect();
                state.completion_columns.insert(&k, cols);
            }
            Ok(Err(e)) => {
                tracing::warn!("Failed to load the columns of {k}: {e:#}");
                state.completion_columns.failed.insert(k);
            }
            Err(e) => {
                tracing::warn!("Loading the columns of {k} stopped: {e}");
                state.completion_columns.failed.insert(k);
            }
        }
    }
    if state.completion_wanted {
        state.refresh_completions();
    }
}

/// Close `conn`; when a load still holds it, the last holder closes it on
/// drop.
pub async fn close(conn: Arc<DatabaseConnection>) {
    if let Ok(conn) = Arc::try_unwrap(conn) {
        conn.close().await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::engine::config::AppConfig;
    use crate::engine::models::{ConnectionConfig, DatabaseType, SchemaInfo};
    use crate::engine::ops::test_support::sqlite_mem;
    use crate::tui::ActivePanel;

    fn col(name: &str, ty: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            type_name: ty.into(),
            primary_key: pk,
        }
    }

    /// An SQLite editor on `query` (cursor at `|`), the tables `main.users`
    /// and `main.orders` known.
    pub(crate) fn editor(query: &str) -> AppState {
        let mut state = AppState::new(AppConfig::default(), false, true);
        state.current_connection_config = Some(ConnectionConfig {
            db_type: DatabaseType::SQLite,
            ..Default::default()
        });
        state.schemas = vec![SchemaInfo {
            name: "main".into(),
            tables: vec!["users".into(), "orders".into()],
            expanded: true,
        }];
        state.active_panel = ActivePanel::QueryEditor;
        let cursor = query.find('|').unwrap();
        state.set_query(query.replace('|', ""));
        state.set_cursor_position(cursor);
        state
    }

    pub(crate) fn users() -> Vec<ColumnInfo> {
        vec![col("id", "int", true), col("email", "varchar(255)", false)]
    }

    fn shown(state: &AppState) -> Vec<(String, String)> {
        state
            .completion_suggestions
            .iter()
            .map(|s| (s.label.clone(), s.detail.clone()))
            .collect()
    }

    /// Poll until every load is over.
    async fn settle(state: &mut AppState) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !state.completion_columns.pending.is_empty() {
            assert!(Instant::now() < deadline, "columns never arrived");
            poll(state).await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[test]
    fn suggestions_come_from_the_cited_tables() {
        let mut state = editor("SELECT u.| FROM users u");
        state.completion_columns.insert("main.users", users());
        state.update_completions();
        assert!(state.show_completion);
        assert_eq!(
            shown(&state),
            [
                ("id".to_string(), "PK int".to_string()),
                ("email".to_string(), "varchar(255)".to_string()),
            ]
        );
        assert!(state.completion_columns.pending.is_empty());
    }

    #[test]
    fn applying_replaces_the_typed_token() {
        let mut state = editor("SELECT u.em| FROM users u");
        state.completion_columns.insert("main.users", users());
        state.update_completions();
        assert_eq!(shown(&state).len(), 1);
        state.apply_completion();
        assert_eq!(state.query_input(), "SELECT u.email FROM users u");
        assert_eq!(state.cursor_position(), "SELECT u.email".len());
        assert!(!state.show_completion && !state.completion_wanted);
    }

    #[test]
    fn without_a_cited_table_the_last_results_columns_are_offered() {
        let mut state = editor("SELECT na|");
        state.known_columns = vec!["name".into(), "id".into()];
        state.update_completions();
        assert!(
            shown(&state).iter().any(|(l, _)| l == "name"),
            "{:?}",
            shown(&state)
        );
    }

    #[tokio::test]
    async fn missing_columns_load_in_the_background_then_refresh() {
        let mut state = editor("SELECT | FROM users");
        state.connection = Some(Arc::new(
            sqlite_mem(&["CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT)"]).await,
        ));
        state.update_completions();
        // Not loaded yet: keywords only, the load runs.
        assert!(!shown(&state).iter().any(|(l, _)| l == "email"));
        assert_eq!(state.completion_columns.pending.len(), 1);
        // Asked again meanwhile: still a single load.
        state.update_completions();
        assert_eq!(state.completion_columns.pending.len(), 1);

        settle(&mut state).await;
        let items = shown(&state);
        assert!(
            items.contains(&("email".into(), "TEXT".into())),
            "{items:?}"
        );
        assert!(
            items.contains(&("id".into(), "PK INTEGER".into())),
            "{items:?}"
        );
        assert!(state.show_completion);
    }

    #[tokio::test]
    async fn arrivals_after_a_dismissal_stay_hidden() {
        let mut state = editor("SELECT | FROM users");
        state.connection = Some(Arc::new(
            sqlite_mem(&["CREATE TABLE users (id INTEGER PRIMARY KEY)"]).await,
        ));
        state.update_completions();
        state.hide_completion();
        settle(&mut state).await;
        assert!(!state.show_completion);
        assert!(state.completion_suggestions.is_empty());
        // Loaded for the next time.
        let users = TableRef {
            schema: Some("main".into()),
            name: "users".into(),
            alias: None,
        };
        assert!(state.completion_columns.get(&users).is_some());
    }
}
