# Engine extraction — Implementation Plan (1/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganise the crate into `engine/` (UI-agnostic) and `tui/` (ratatui), and extract the business operations buried in `main.rs` into `engine::ops`, without changing TUI behaviour.

**Architecture:** Pure file moves first (one commit, build green), then extract the SQL helpers and the async operations one module at a time, each with tests, and rewire the TUI handlers to call them. The TUI keeps all its UI state (`AppState`), status messages and progress redraws; `engine::ops` only takes a `&DatabaseConnection` plus explicit parameters and returns typed results.

**Tech Stack:** Rust 2021, tokio, sqlx (SQLite in-memory for tests), ratatui (unchanged).

Spec: `docs/superpowers/specs/2026-10-08-gui-distribution-design.md`. Plans 2–4 build on this one.

Conventions for every task:
- Prefix shell commands with `rtk` (user's global CLAUDE.md).
- `rtk cargo test` must be green and `rtk cargo clippy` must not add warnings before each commit.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

## Target file structure (end of this plan)

```
src/
├── main.rs                  # mod declarations + #[tokio::main] calling tui::run (plan 2 adds the CLI)
├── engine/
│   ├── mod.rs               # pub mod config, db, models, services, sql, ops
│   ├── config/mod.rs        # moved from src/config
│   ├── db/…                 # moved from src/db
│   ├── models/…             # moved from src/models
│   ├── services/            # moved from src/services, minus app_state.rs
│   │   ├── mod.rs
│   │   ├── export_import.rs
│   │   ├── query_tabs.rs
│   │   ├── schema_service.rs
│   │   └── table_cache.rs
│   ├── sql/
│   │   ├── mod.rs           # pub mod lexer, statements
│   │   ├── lexer.rs         # tokenizer + completions (from ui/sql_highlight.rs, no ratatui)
│   │   └── statements.rs    # split_statements, ExecutionUnit, extract_table_from_query, quote_chars
│   └── ops/
│       ├── mod.rs           # pub mod query, rows, schema, transfer
│       ├── query.rs         # run_query, run_unit, refresh_schemas
│       ├── rows.rs          # update/insert/delete row, truncate, detect_system_columns
│       ├── schema.rs        # fetch_columns, apply_modification
│       └── transfer.rs      # import_csv, import_tables, export_tables
└── tui/
    ├── mod.rs               # run(), run_app, handlers (body of the old main.rs)
    ├── app_state.rs         # moved from services/app_state.rs
    ├── key_handlers/…       # moved from src/key_handlers
    └── ui/…                 # moved from src/ui (sql_highlight.rs keeps only ratatui rendering)
```

---

### Task 1: Move modules into `engine/` and `tui/`

Pure move, no logic change.

**Files:**
- Move: `src/config` → `src/engine/config`, `src/db` → `src/engine/db`, `src/models` → `src/engine/models`, `src/services` → `src/engine/services`
- Move: `src/engine/services/app_state.rs` → `src/tui/app_state.rs`
- Move: `src/key_handlers` → `src/tui/key_handlers`, `src/ui` → `src/tui/ui`
- Create: `src/engine/mod.rs`, `src/tui/mod.rs`
- Modify: `src/main.rs`, every file using `crate::…` paths

- [ ] **Step 1: Move the directories with git**

```bash
mkdir -p src/engine src/tui
git mv src/config src/engine/config
git mv src/db src/engine/db
git mv src/models src/engine/models
git mv src/services src/engine/services
git mv src/engine/services/app_state.rs src/tui/app_state.rs
git mv src/key_handlers src/tui/key_handlers
git mv src/ui src/tui/ui
git mv src/main.rs src/tui/mod.rs
```

- [ ] **Step 2: Create `src/engine/mod.rs`**

```rust
//! UI-agnostic core: configuration, database connectors, models and
//! operations shared by the GUI and the TUI.
pub mod config;
pub mod db;
pub mod models;
pub mod services;
```

- [ ] **Step 3: Fix `src/engine/services/mod.rs`** (drop `app_state`)

```rust
pub mod export_import;
pub mod query_tabs;
pub mod schema_service;
pub mod table_cache;

pub use schema_service::*;
```

- [ ] **Step 4: Create the new `src/main.rs`**

```rust
mod engine;
mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tui::run().await
}
```

- [ ] **Step 5: Turn `src/tui/mod.rs` (the old main.rs) into a module**

At the top of `src/tui/mod.rs`, replace the six `mod …;` lines and the
`use config::AppConfig; use db::DatabaseConnection; use services::{…}; use ui::{…};` lines with:

```rust
pub mod app_state;
mod key_handlers;
pub mod ui;

use crate::engine::{config, db, models, services};
pub use app_state::*;

use config::AppConfig;
use db::DatabaseConnection;
use services::ColumnDefinition;
use ui::{
    compute_active_panel_area, compute_modal_area, render_neon_border, render_ui,
    run_splash_screen, ClickableRegistry, ModalAnimation, PanelAnimations,
};
```

Rename `#[tokio::main] async fn main() -> Result<()>` to `pub async fn run() -> Result<()>` (remove the `#[tokio::main]` attribute). The body is unchanged.

- [ ] **Step 6: Rewrite crate paths with sed**

```bash
files=$(git ls-files 'src/*.rs')
sed -i \
  -e 's/crate::models::/crate::engine::models::/g' \
  -e 's/crate::db::/crate::engine::db::/g' \
  -e 's/crate::config::/crate::engine::config::/g' \
  -e 's/crate::services::AppState/crate::tui::AppState/g' \
  -e 's/crate::services::ActivePanel/crate::tui::ActivePanel/g' \
  -e 's/crate::services::DialogMode/crate::tui::DialogMode/g' \
  -e 's/crate::services::ConnectionField/crate::tui::ConnectionField/g' \
  -e 's/crate::services::/crate::engine::services::/g' \
  -e 's/crate::ui::/crate::tui::ui::/g' \
  -e 's/crate::prev_char_boundary/crate::tui::prev_char_boundary/g' \
  -e 's/crate::next_char_boundary/crate::tui::next_char_boundary/g' \
  -e 's/crate::handle_/crate::tui::handle_/g' \
  -e 's/crate::move_cursor_/crate::tui::move_cursor_/g' \
  -e 's/crate::fetch_table_columns/crate::tui::fetch_table_columns/g' \
  -e 's/crate::update_completions_from_context/crate::tui::update_completions_from_context/g' \
  $files
```

In `src/tui/app_state.rs`, replace the `super::…` imports (it no longer lives in `services`):

```rust
use crate::engine::services::export_import::{
```
(keep the imported item list as it was)
```rust
use crate::engine::services::query_tabs::QueryTabsState;
use crate::engine::services::table_cache::{FetchQueue, TableCache};
```

- [ ] **Step 7: Build and fix remaining paths**

Run: `rtk cargo build`
Expected: compiles. Any remaining `unresolved import` comes from a `use crate::services::{A, B}` group mixing TUI state and engine services: split it into `use crate::tui::{…}` for `AppState`/`ActivePanel`/`DialogMode`/`ConnectionField`/`NewConnectionState` and `use crate::engine::services::{…}` for the rest. Visibility errors (`function is private`) on items now reached from another module: change `fn` → `pub(crate) fn`.

- [ ] **Step 8: Run tests**

Run: `rtk cargo test`
Expected: same tests as before (execution_unit_tests, sqlite, sqlserver, sql_highlight) all PASS.

- [ ] **Step 9: Smoke-test the TUI**

Run: `cargo run -- --no-animations`, open the connection list, press `q`.
Expected: identical behaviour to before.

- [ ] **Step 10: Commit**

```bash
rtk git add -A src
rtk git commit -m "refactor: split crate into engine and tui modules"
```

---

### Task 2: `engine::sql::statements` — statement splitting and helpers

**Files:**
- Create: `src/engine/sql/mod.rs`, `src/engine/sql/statements.rs`
- Modify: `src/engine/mod.rs`, `src/tui/mod.rs` (remove the moved functions and tests)

- [ ] **Step 1: Create `src/engine/sql/mod.rs`**

```rust
//! SQL text utilities shared by the frontends.
pub mod lexer;
pub mod statements;
```

Create an empty `src/engine/sql/lexer.rs` for now (filled in Task 3). Add `pub mod sql;` to `src/engine/mod.rs`.

- [ ] **Step 2: Move the code**

Cut from `src/tui/mod.rs` and paste into `src/engine/sql/statements.rs`: `ExecutionUnit`, `split_statements`, `is_transaction_start`, `is_transaction_end`, `get_execution_unit_at_cursor`, `extract_table_from_query`, and the whole `#[cfg(test)] mod execution_unit_tests`. Make them `pub`. Change the test module's import to `use super::{get_execution_unit_at_cursor, ExecutionUnit};` (unchanged text, it now resolves to this file).

Then add at the end of the non-test code:

```rust
use crate::engine::models::DatabaseType;

/// Identifier quote characters `(open, close)` for a database type.
pub fn quote_chars(db_type: &DatabaseType) -> (char, char) {
    match db_type {
        DatabaseType::Postgres | DatabaseType::SQLite => ('"', '"'),
        DatabaseType::MySQL => ('`', '`'),
        DatabaseType::SQLServer | DatabaseType::Azure => ('[', ']'),
    }
}
```

- [ ] **Step 3: Add tests for the new and previously untested helpers**

Append inside a new test module in `statements.rs`:

```rust
#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn split_ignores_semicolons_in_comments() {
        let stmts = split_statements("SELECT 1; -- a;b\nSELECT 2; /* x; */ SELECT 3");
        let texts: Vec<_> = stmts.iter().map(|s| s.2.as_str()).collect();
        assert_eq!(texts, ["SELECT 1", "-- a;b\nSELECT 2", "/* x; */ SELECT 3"]);
    }

    #[test]
    fn extract_table_handles_schema_and_quotes() {
        assert_eq!(
            extract_table_from_query("select * from dbo.[Users] where 1=1"),
            Some("dbo.[Users]".into())
        );
        assert_eq!(extract_table_from_query("SELECT 1"), None);
    }

    #[test]
    fn quote_chars_per_db() {
        assert_eq!(quote_chars(&DatabaseType::MySQL), ('`', '`'));
        assert_eq!(quote_chars(&DatabaseType::Azure), ('[', ']'));
        assert_eq!(quote_chars(&DatabaseType::Postgres), ('"', '"'));
    }
}
```

- [ ] **Step 4: Rewire the TUI**

In `src/tui/mod.rs`:
- add `use crate::engine::sql::statements::{extract_table_from_query, get_execution_unit_at_cursor, quote_chars, ExecutionUnit};`
- replace the body of `get_quote_chars`:

```rust
fn get_quote_chars(state: &AppState) -> (char, char) {
    state
        .current_connection_config
        .as_ref()
        .map(|c| quote_chars(&c.db_type))
        .unwrap_or(('"', '"'))
}
```

- [ ] **Step 5: Run tests**

Run: `rtk cargo test statements`
Expected: the 10 moved tests + 3 new ones PASS.

- [ ] **Step 6: Commit**

```bash
rtk git add -A src
rtk git commit -m "refactor: move SQL statement helpers to engine::sql"
```

---

### Task 3: `engine::sql::lexer` — tokenizer and completions without ratatui

**Files:**
- Modify: `src/engine/sql/lexer.rs`, `src/tui/ui/sql_highlight.rs`, `src/tui/app_state.rs`, `src/tui/mod.rs`

- [ ] **Step 1: Move the UI-free code**

Move from `src/tui/ui/sql_highlight.rs` into `src/engine/sql/lexer.rs` everything **except** the two ratatui imports, `tokens_to_spans` and `highlight_sql`: that is `SQL_KEYWORDS`, `SQL_FUNCTIONS`, `SQL_OPERATORS`, `SqlToken`, `tokenize_sql`, `last_keyword_pos`, `extract_token` and every other private helper, `extract_schema_qualifier`, `format_identifier`, `get_completions`, `extract_table_from_query`, and the `#[cfg(test)]` module. Make `extract_token` `pub` (it is used by `app_state.rs`).

- [ ] **Step 2: Keep only rendering in `sql_highlight.rs`**

The top of `src/tui/ui/sql_highlight.rs` becomes:

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub use crate::engine::sql::lexer::*;
```

followed by the unchanged `tokens_to_spans` and `highlight_sql`. The re-export keeps every existing `sql_highlight::…` call site compiling.

- [ ] **Step 3: Verify no ratatui in the engine**

Run: `rtk grep "ratatui\|crossterm" src/engine`
Expected: no matches.

- [ ] **Step 4: Run tests**

Run: `rtk cargo test lexer`
Expected: the moved sql_highlight tests PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add -A src
rtk git commit -m "refactor: move SQL tokenizer and completions to engine::sql::lexer"
```

---

### Task 4: Test fixture — in-memory SQLite `DatabaseConnection`

**Files:**
- Create: `src/engine/ops/mod.rs`, `src/engine/ops/test_support.rs`
- Modify: `src/engine/mod.rs`

- [ ] **Step 1: Create `src/engine/ops/mod.rs`**

```rust
//! Business operations shared by the GUI and the TUI. Each function takes a
//! connection and explicit parameters and returns a typed result: no UI state.
pub mod query;
pub mod rows;
pub mod schema;
pub mod transfer;

#[cfg(test)]
pub(crate) mod test_support;
```

Create empty `query.rs`, `rows.rs`, `schema.rs`, `transfer.rs`. Add `pub mod ops;` to `src/engine/mod.rs`.

- [ ] **Step 2: Write `src/engine/ops/test_support.rs`**

```rust
use sqlx::sqlite::SqlitePoolOptions;

use crate::engine::db::DatabaseConnection;

/// In-memory SQLite connection. `max_connections(1)` makes every query reuse
/// the same connection, hence the same in-memory database.
pub async fn sqlite_mem(setup: &[&str]) -> DatabaseConnection {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for stmt in setup {
        sqlx::query(stmt).execute(&pool).await.unwrap();
    }
    DatabaseConnection::SQLite(pool)
}

pub async fn count(conn: &DatabaseConnection, table: &str) -> String {
    let r = conn
        .execute_query(&format!("SELECT COUNT(*) AS n FROM {table}"))
        .await
        .unwrap();
    r.rows[0][0].clone()
}
```

- [ ] **Step 3: Build**

Run: `rtk cargo test --no-run`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
rtk git add -A src
rtk git commit -m "test: in-memory SQLite fixture for engine ops"
```

---

### Task 5: `engine::ops::query`

**Files:**
- Modify: `src/engine/ops/query.rs`, `src/tui/mod.rs`

- [ ] **Step 1: Write the failing tests** in `src/engine/ops/query.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ops::test_support::{count, sqlite_mem};

    const SETUP: &[&str] = &[
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)",
        "INSERT INTO users (name, email) VALUES ('Alice', 'a@x'), ('Bob', NULL)",
    ];

    #[tokio::test]
    async fn run_query_enriches_columns_with_pk_and_nullability() {
        let conn = sqlite_mem(SETUP).await;
        let r = run_query(&conn, "SELECT * FROM users").await.unwrap();
        assert_eq!(r.rows.len(), 2);
        let id = r.columns.iter().find(|c| c.name == "id").unwrap();
        let name = r.columns.iter().find(|c| c.name == "name").unwrap();
        assert!(id.is_primary_key);
        assert!(!name.nullable);
    }

    #[tokio::test]
    async fn run_unit_single_and_transaction() {
        let conn = sqlite_mem(SETUP).await;
        let sql = "BEGIN;\nINSERT INTO users (name) VALUES ('C');\nCOMMIT;\nSELECT * FROM users";
        let out = run_at_cursor(&conn, sql, 10).await.unwrap();
        assert!(matches!(out, Executed::Transaction { statements: 3, .. }));
        assert_eq!(count(&conn, "users").await, "3");

        let cursor = sql.find("SELECT").unwrap();
        let out = run_at_cursor(&conn, sql, cursor).await.unwrap();
        let Executed::Query(r) = out else { panic!("expected a query") };
        assert_eq!(r.rows.len(), 3);
    }

    #[tokio::test]
    async fn failing_transaction_rolls_back() {
        let conn = sqlite_mem(SETUP).await;
        let sql = "BEGIN;\nINSERT INTO users (name) VALUES ('C');\nINSERT INTO nope VALUES (1);\nCOMMIT;";
        assert!(run_at_cursor(&conn, sql, 0).await.is_err());
        assert_eq!(count(&conn, "users").await, "2");
    }

    #[tokio::test]
    async fn unterminated_transaction_and_empty_are_errors() {
        let conn = sqlite_mem(SETUP).await;
        let err = run_at_cursor(&conn, "BEGIN; SELECT 1;", 0).await.unwrap_err();
        assert!(err.to_string().contains("COMMIT or ROLLBACK"));
        assert!(run_at_cursor(&conn, "   ", 0).await.is_err());
    }

    #[tokio::test]
    async fn refresh_schemas_lists_tables() {
        let conn = sqlite_mem(SETUP).await;
        let schemas = refresh_schemas(&conn).await.unwrap();
        assert!(schemas.iter().any(|s| s.tables.iter().any(|t| t == "users")));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test ops::query`
Expected: FAIL — `run_query`, `run_at_cursor`, `Executed`, `refresh_schemas` not found.

- [ ] **Step 3: Implement** (top of `src/engine/ops/query.rs`)

```rust
use anyhow::{anyhow, bail, Result};

use crate::engine::db::DatabaseConnection;
use crate::engine::models::{QueryResult, SchemaInfo};
use crate::engine::sql::statements::{
    extract_table_from_query, get_execution_unit_at_cursor, ExecutionUnit,
};

/// Outcome of executing the SQL at the cursor.
#[derive(Debug)]
pub enum Executed {
    Query(QueryResult),
    /// A whole `BEGIN … COMMIT/ROLLBACK` block, committed.
    Transaction { result: QueryResult, statements: usize },
}

/// Execute one query and, when it reads from a single table, annotate the
/// result columns with nullability and primary-key flags (used by row editing).
pub async fn run_query(conn: &DatabaseConnection, sql: &str) -> Result<QueryResult> {
    let mut result = conn.execute_query(sql).await?;
    if let Some(table) = extract_table_from_query(sql) {
        if let Ok(nullability) = conn.get_column_nullability(&table).await {
            for col in &mut result.columns {
                if let Some(&nullable) = nullability.get(&col.name) {
                    col.nullable = nullable;
                }
            }
        }
        if let Ok(pks) = conn.get_primary_keys(&table).await {
            for col in &mut result.columns {
                col.is_primary_key = pks.contains(&col.name);
            }
        }
    }
    Ok(result)
}

/// Execute the statement (or transaction block) under `cursor` in `text`.
pub async fn run_at_cursor(conn: &DatabaseConnection, text: &str, cursor: usize) -> Result<Executed> {
    match get_execution_unit_at_cursor(text, cursor) {
        ExecutionUnit::UnterminatedTransaction => {
            bail!("Transaction not terminated: add COMMIT or ROLLBACK")
        }
        ExecutionUnit::Transaction(statements) => {
            let result = conn
                .execute_transaction(&statements)
                .await
                .map_err(|e| anyhow!("Transaction rolled back: {e}"))?;
            Ok(Executed::Transaction { result, statements: statements.len() })
        }
        ExecutionUnit::Single(sql) if sql.trim().is_empty() => bail!("No query at cursor position"),
        ExecutionUnit::Single(sql) => Ok(Executed::Query(run_query(conn, &sql).await?)),
    }
}

/// Execute the whole editor content as one query (F5).
pub async fn run_all(conn: &DatabaseConnection, text: &str) -> Result<QueryResult> {
    if text.trim().is_empty() {
        bail!("Query is empty");
    }
    run_query(conn, text).await
}

pub async fn refresh_schemas(conn: &DatabaseConnection) -> Result<Vec<SchemaInfo>> {
    conn.get_tables_by_schema().await
}
```

- [ ] **Step 4: Run tests**

Run: `rtk cargo test ops::query`
Expected: 5 PASS.

- [ ] **Step 5: Rewire the TUI handlers** in `src/tui/mod.rs`

Replace `handle_execute_query` and `handle_execute_current_query` with:

```rust
use crate::engine::ops::{self, query::Executed};

pub(crate) async fn handle_execute_query(state: &mut AppState) {
    let Some(conn) = state.connection.as_ref() else {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    };
    let query = state.query_input().to_string();
    state.set_status("Executing query...");
    state.is_loading = true;
    let result = ops::query::run_all(conn, &query).await;
    state.is_loading = false;
    match result {
        Ok(result) => {
            let msg = format!(
                "Query executed: {} rows in {}ms",
                result.rows.len(),
                result.execution_time_ms
            );
            show_result(state, result);
            state.set_status(msg);
        }
        Err(e) => state.set_status(format!("Query error: {e}")),
    }
}

/// Execute only the SQL statement (or transaction block) at the cursor.
pub(crate) async fn handle_execute_current_query(state: &mut AppState) {
    let Some(conn) = state.connection.as_ref() else {
        state.set_status("Not connected. Select a connection and press Enter.");
        return;
    };
    let text = state.query_input().to_string();
    let cursor = state.cursor_position();
    state.set_status("Executing query...");
    state.is_loading = true;
    let outcome = ops::query::run_at_cursor(conn, &text, cursor).await;
    state.is_loading = false;
    match outcome {
        Ok(Executed::Query(result)) => {
            let msg = format!(
                "Query executed: {} rows in {}ms",
                result.rows.len(),
                result.execution_time_ms
            );
            show_result(state, result);
            state.set_status(msg);
        }
        Ok(Executed::Transaction { result, statements }) => {
            let msg = if result.rows.is_empty() {
                format!("Transaction committed: {statements} statements in {}ms", result.execution_time_ms)
            } else {
                format!("Transaction committed: {} rows in {}ms", result.rows.len(), result.execution_time_ms)
            };
            show_result(state, result);
            state.set_status(msg);
        }
        Err(e) => state.set_status(e.to_string()),
    }
}

fn show_result(state: &mut AppState, result: crate::engine::models::QueryResult) {
    state.query_result = Some(result);
    state.update_known_columns();
    state.compute_col_widths();
    state.selected_row = 0;
    state.results_scroll_x = 0;
    state.active_panel = ActivePanel::Results;
}
```

Note: the error strings change slightly for empty queries ("Query error: Query is empty" instead of "Query is empty"); that is acceptable.

In `handle_refresh_tables`, replace `conn.get_tables_by_schema().await` with `ops::query::refresh_schemas(conn).await`.

- [ ] **Step 6: Run all tests + smoke test**

Run: `rtk cargo test` → all PASS. Then `cargo run -- --no-animations` against a SQLite file: `F5` and `Ctrl+Enter` give the same results and status messages as before.

- [ ] **Step 7: Commit**

```bash
rtk git add -A src
rtk git commit -m "refactor: extract query execution into engine::ops::query"
```

---

### Task 6: `engine::ops::rows`

**Files:**
- Modify: `src/engine/ops/rows.rs`, `src/tui/mod.rs`, `src/tui/app_state.rs`

- [ ] **Step 1: Write the failing tests** in `src/engine/ops/rows.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::Column;
    use crate::engine::ops::test_support::{count, sqlite_mem};

    const SETUP: &[&str] = &[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b')",
        "CREATE TABLE u (x INTEGER)",
        "INSERT INTO u VALUES (1), (2), (3)",
    ];

    fn cols() -> Vec<Column> {
        ["id", "name"]
            .iter()
            .map(|n| Column {
                name: n.to_string(),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: *n == "id",
            })
            .collect()
    }

    fn row(a: &str, b: &str) -> Vec<String> {
        vec![a.into(), b.into()]
    }

    #[tokio::test]
    async fn update_insert_delete_roundtrip() {
        let conn = sqlite_mem(SETUP).await;
        let n = update_row(&conn, "t", &cols(), &row("1", "a"), &row("1", "z")).await.unwrap();
        assert_eq!(n, 1);
        let n = insert_row(&conn, "t", &cols(), &row("", "c"), &[0]).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(count(&conn, "t").await, "3");
        let n = delete_row(&conn, "t", &cols(), &row("2", "b"), ('"', '"')).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(count(&conn, "t").await, "2");
    }

    #[tokio::test]
    async fn update_without_changes_is_noop() {
        let conn = sqlite_mem(SETUP).await;
        let n = update_row(&conn, "t", &cols(), &row("1", "a"), &row("1", "a")).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn truncate_tables_reports_per_table() {
        let conn = sqlite_mem(SETUP).await;
        let report = truncate_tables(&conn, &["t".into(), "missing".into(), "u".into()], |_, _, _| {}).await;
        assert_eq!(report.succeeded, 2);
        assert_eq!(report.rows_affected, 5);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].starts_with("missing:"));
    }

    #[test]
    fn detects_system_columns() {
        let mk = |n: &str, t: &str| Column {
            name: n.into(),
            type_name: t.into(),
            nullable: true,
            is_primary_key: false,
        };
        let cols = vec![mk("id", "int"), mk("label", "text"), mk("created_at", "timestamp"), mk("seq", "serial")];
        assert_eq!(detect_system_columns(&cols), vec![0, 2, 3]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test ops::rows`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement** (top of `src/engine/ops/rows.rs`)

```rust
use anyhow::Result;

use crate::engine::db::{utils::build_delete_query, DatabaseConnection};
use crate::engine::models::Column;

/// Update one row identified by its original values. Returns 0 without
/// touching the database when nothing changed.
pub async fn update_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    original: &[String],
    new: &[String],
) -> Result<u64> {
    if original == new {
        return Ok(0);
    }
    conn.update_row(table, columns, original, new).await
}

/// Insert a row, skipping the auto-generated `system_columns` indices.
pub async fn insert_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    values: &[String],
    system_columns: &[usize],
) -> Result<u64> {
    conn.insert_row(table, columns, values, system_columns).await
}

pub async fn delete_row(
    conn: &DatabaseConnection,
    table: &str,
    columns: &[Column],
    values: &[String],
    quotes: (char, char),
) -> Result<u64> {
    let sql = build_delete_query(table, columns, values, quotes.0, quotes.1);
    Ok(conn.execute_query(&sql).await?.rows_affected)
}

/// Result of an operation applied to several tables.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BatchReport {
    pub total: usize,
    pub succeeded: usize,
    pub rows_affected: u64,
    /// `"<table>: <error>"`, one per failed table.
    pub errors: Vec<String>,
}

/// `DELETE FROM` each (already quoted) table name. `progress(done, total, table)`
/// is called before each table.
pub async fn truncate_tables(
    conn: &DatabaseConnection,
    tables: &[String],
    mut progress: impl FnMut(usize, usize, &str),
) -> BatchReport {
    let mut report = BatchReport { total: tables.len(), ..Default::default() };
    for (i, table) in tables.iter().enumerate() {
        progress(i, tables.len(), table);
        match conn.execute_query(&format!("DELETE FROM {table}")).await {
            Ok(r) => {
                report.succeeded += 1;
                report.rows_affected += r.rows_affected;
            }
            Err(e) => report.errors.push(format!("{table}: {e}")),
        }
    }
    report
}

/// Indices of columns the database fills itself (ids, serials, timestamps),
/// left out of INSERT statements by default.
pub fn detect_system_columns(columns: &[Column]) -> Vec<usize> {
    let first = columns.first().map(|c| c.name.to_lowercase()).unwrap_or_default();
    columns
        .iter()
        .enumerate()
        .filter_map(|(idx, col)| {
            let name = col.name.to_lowercase();
            let ty = col.type_name.to_lowercase();
            let is_auto_id = name == "id"
                || name.ends_with("_id") && name.starts_with(&first)
                || ty.contains("serial")
                || ty.contains("identity")
                || ty.contains("auto_increment");
            let is_timestamp = [
                "created_at", "updated_at", "createdat", "updatedat",
                "created_on", "updated_on", "inserted_at", "modified_at",
            ]
            .iter()
            .any(|p| name.contains(p));
            (is_auto_id || is_timestamp).then_some(idx)
        })
        .collect()
}
```

- [ ] **Step 4: Run tests**

Run: `rtk cargo test ops::rows`
Expected: 4 PASS. If `update_row` against SQLite returns an error because `build_update_clauses` quoting differs, read `src/engine/db/sqlite.rs::update_row` and adapt the test's table/column names (not the implementation).

- [ ] **Step 5: Rewire the TUI**

In `src/tui/app_state.rs::open_add_row_dialog`, replace the inline `filter_map` computing `self.system_columns` with:

```rust
self.system_columns = crate::engine::ops::rows::detect_system_columns(&result.columns);
```

In `src/tui/mod.rs`:
- `handle_save_row`: replace `state.connection.as_ref().unwrap().update_row(&table_name, &columns, &original_values, &new_values).await` with `ops::rows::update_row(state.connection.as_ref().unwrap(), &table_name, &columns, &original_values, &new_values).await`.
- `handle_insert_row`: same with `ops::rows::insert_row(…, &system_cols)`.
- `handle_delete_row`: replace the `build_delete_query` + `execute_query` pair with

```rust
let result = ops::rows::delete_row(
    state.connection.as_ref().unwrap(),
    &table_name,
    &columns,
    &row_values,
    quote_chars,
)
.await;
```

  and in the `Ok` arm use the returned `u64` instead of `result.rows_affected`. The debug-mode branch keeps calling `db::utils::build_delete_query` to show the query.
- `handle_batch_truncate`: replace the `for` loop and counters with

```rust
let conn = state.connection.as_ref().unwrap();
let mut progress_updates = Vec::new();
let report = ops::rows::truncate_tables(conn, &selected_tables, |i, n, t| {
    progress_updates.push((i + 1, n, t.to_string()))
})
.await;
```

  then set `bs.progress = Some((total, total, "Done".into()))`, redraw once, and build the status from `report` (`report.succeeded`, `report.total`, `report.rows_affected`, `report.errors.last()`), keeping the three existing message formats. (The per-table redraw during truncation is dropped: truncates are fast and the closure cannot borrow the terminal and state at the same time.) Remove `progress_updates` if clippy flags it as unused; pass `|_, _, _| {}` instead.

- [ ] **Step 6: Run all tests**

Run: `rtk cargo test`
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
rtk git add -A src
rtk git commit -m "refactor: extract row operations into engine::ops::rows"
```

---

### Task 7: `engine::ops::schema`

**Files:**
- Modify: `src/engine/ops/schema.rs`, `src/tui/mod.rs`

- [ ] **Step 1: Write the failing tests** in `src/engine/ops/schema.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::DatabaseType;
    use crate::engine::ops::test_support::sqlite_mem;
    use crate::engine::services::{table_cache::TableCache, ColumnDefinition, SchemaModification};

    #[tokio::test]
    async fn fetch_columns_uses_and_fills_cache() {
        let conn = sqlite_mem(&["CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)"]).await;
        let cache = TableCache::default();
        let cols = fetch_columns(&conn, &cache, "t").await.unwrap();
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name"]);
        assert!(cols[0].is_primary_key);
        assert!(cache.get_column_details("t").await.is_some());
    }

    #[tokio::test]
    async fn add_column_executes_and_invalidates_cache() {
        let conn = sqlite_mem(&["CREATE TABLE t (id INTEGER PRIMARY KEY)"]).await;
        let cache = TableCache::default();
        fetch_columns(&conn, &cache, "t").await.unwrap();
        let m = SchemaModification::AddColumn {
            table_name: "t".into(),
            column: ColumnDefinition { name: "age".into(), data_type: "INTEGER".into(), ..Default::default() },
        };
        let sql = apply_modification(&conn, &cache, &m, &DatabaseType::SQLite).await.unwrap();
        assert!(sql.to_uppercase().contains("ALTER TABLE"));
        assert!(cache.get_column_details("t").await.is_none());
        let cols = fetch_columns(&conn, &cache, "t").await.unwrap();
        assert!(cols.iter().any(|c| c.name == "age"));
    }

    #[test]
    fn modification_table_name() {
        let m = SchemaModification::DropColumn { table_name: "t".into(), column_name: "c".into() };
        assert_eq!(table_of(&m), Some("t"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test ops::schema`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement** (top of `src/engine/ops/schema.rs`)

```rust
use anyhow::Result;

use crate::engine::db::DatabaseConnection;
use crate::engine::models::DatabaseType;
use crate::engine::services::{table_cache::TableCache, ColumnDefinition, SchemaModification, SchemaService};

/// Column definitions of `table`, served from `cache` when fresh.
pub async fn fetch_columns(
    conn: &DatabaseConnection,
    cache: &TableCache,
    table: &str,
) -> Result<Vec<ColumnDefinition>> {
    let columns = match cache.get_column_details(table).await {
        Some(cols) => cols,
        None => {
            let cols = conn.get_table_column_details(table).await?;
            cache.set(table.to_string(), cols.clone()).await;
            cols
        }
    };
    Ok(columns
        .into_iter()
        .map(|c| ColumnDefinition {
            name: c.name,
            data_type: c.type_name,
            nullable: c.nullable,
            is_primary_key: c.is_primary_key,
            default_value: None,
        })
        .collect())
}

/// The table a modification applies to.
pub fn table_of(m: &SchemaModification) -> Option<&str> {
    match m {
        SchemaModification::AddColumn { table_name, .. }
        | SchemaModification::DropColumn { table_name, .. }
        | SchemaModification::RenameColumn { table_name, .. }
        | SchemaModification::ModifyColumn { table_name, .. }
        | SchemaModification::CreateTable { table_name, .. }
        | SchemaModification::DropTable { table_name }
        | SchemaModification::AddIndex { table_name, .. }
        | SchemaModification::DropIndex { table_name, .. } => Some(table_name),
        SchemaModification::RenameTable { old_name, .. } => Some(old_name),
    }
}

/// Generate and execute the SQL for `m`; returns the SQL that ran.
pub async fn apply_modification(
    conn: &DatabaseConnection,
    cache: &TableCache,
    m: &SchemaModification,
    db_type: &DatabaseType,
) -> Result<String> {
    let sql = SchemaService::generate_sql(m, db_type);
    conn.execute_query(&sql).await?;
    if let Some(table) = table_of(m) {
        cache.invalidate(table).await;
    }
    Ok(sql)
}
```

If `SchemaService::generate_sql` takes different parameter types, match its real signature in `src/engine/services/schema_service.rs:72`.

- [ ] **Step 4: Run tests**

Run: `rtk cargo test ops::schema`
Expected: 3 PASS.

- [ ] **Step 5: Rewire the TUI**

In `src/tui/mod.rs`:
- `fetch_table_columns` body becomes:

```rust
pub(crate) async fn fetch_table_columns(
    state: &mut AppState,
    table_name: &str,
) -> Option<Vec<ColumnDefinition>> {
    let conn = state.connection.as_ref()?;
    match ops::schema::fetch_columns(conn, &state.table_cache, table_name).await {
        Ok(cols) => Some(cols),
        Err(e) => {
            tracing::error!("Failed to fetch columns for {}: {}", table_name, e);
            state.set_status(format!("Failed to fetch columns: {}", e));
            None
        }
    }
}
```

- In `handle_schema_action`, each of the four arms (`AddColumn`, `ModifyColumn`, `DropColumn`, `RenameColumn`) currently does `generate_sql` → debug check → `conn.execute_query(&sql)` → `table_cache.invalidate`. Keep the `generate_sql` call for the debug branch; replace the execution block with:

```rust
if let Some(ref conn) = state.connection {
    match ops::schema::apply_modification(conn, &state.table_cache, &modification, &db_type).await {
        Ok(_) => {
            state.set_status(/* the arm's existing success message */);
            state.current_table_context = None;
            if let Some(cols) = fetch_table_columns(state, &table_name).await {
                state.known_columns = cols.iter().map(|c| c.name.clone()).collect();
            }
        }
        Err(e) => state.set_status(/* the arm's existing failure message, with {e} */),
    }
}
```

(The explicit `state.table_cache.invalidate(&table_name).await` line is removed: `apply_modification` does it.)

- [ ] **Step 6: Run all tests**

Run: `rtk cargo test` → all PASS.

- [ ] **Step 7: Commit**

```bash
rtk git add -A src
rtk git commit -m "refactor: extract schema operations into engine::ops::schema"
```

---

### Task 8: `engine::ops::transfer` — import and batch export/import

**Files:**
- Modify: `src/engine/ops/transfer.rs`, `src/tui/mod.rs`
- Modify: `Cargo.toml` (dev-dependency `tempfile = "3"`)

- [ ] **Step 1: Add the dev-dependency**

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write the failing tests** in `src/engine/ops/transfer.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ops::test_support::{count, sqlite_mem};
    use crate::engine::services::export_import::ExportFormat;

    const SETUP: &[&str] = &[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
        "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b')",
    ];

    #[tokio::test]
    async fn import_csv_upserts_by_id() {
        let conn = sqlite_mem(SETUP).await;
        let csv = "id,name\n1,z\n,new\n";
        let stats = import_csv(&conn, "t", csv, ('"', '"'), |_, _| {}).await.unwrap();
        assert_eq!(stats, ImportStats { total: 2, updated: 1, inserted: 1, errors: vec![] });
        assert_eq!(count(&conn, "t").await, "3");
    }

    #[tokio::test]
    async fn import_csv_rejects_empty_file() {
        let conn = sqlite_mem(SETUP).await;
        assert!(import_csv(&conn, "t", "", ('"', '"'), |_, _| {}).await.is_err());
    }

    #[tokio::test]
    async fn export_then_import_tables_roundtrip() {
        let conn = sqlite_mem(SETUP).await;
        let dir = tempfile::tempdir().unwrap();
        let tables = vec![("main".to_string(), "t".to_string())];
        let report = export_tables(&conn, &tables, dir.path(), ExportFormat::Csv, ('"', '"'), |_, _, _| {}).await;
        assert_eq!(report.succeeded, 1, "{:?}", report.errors);
        assert!(dir.path().join("t.csv").exists());

        conn.execute_query("DELETE FROM t").await.unwrap();
        let report = import_tables(&conn, &tables, dir.path(), ('"', '"'), |_, _, _| {}).await;
        assert_eq!(report.succeeded, 1, "{:?}", report.errors);
        assert_eq!(report.rows_affected, 2);
        assert_eq!(count(&conn, "t").await, "2");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `rtk cargo test ops::transfer`
Expected: FAIL — functions not found.

- [ ] **Step 4: Implement** (top of `src/engine/ops/transfer.rs`)

```rust
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::engine::db::DatabaseConnection;
use crate::engine::ops::rows::BatchReport;
use crate::engine::services::export_import::{
    build_upsert_import_actions, export_to_file, parse_csv, BatchExportState, ExportFormat,
    ImportAction,
};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportStats {
    pub total: usize,
    pub updated: usize,
    pub inserted: usize,
    pub errors: Vec<String>,
}

impl ImportStats {
    pub fn succeeded(&self) -> usize {
        self.updated + self.inserted
    }
}

/// Run one import action: UPDATE then INSERT when nothing matched (upsert),
/// or a plain INSERT. Returns `true` when the row was updated.
async fn apply_action(conn: &DatabaseConnection, action: &ImportAction) -> Result<bool> {
    match action {
        ImportAction::Upsert { update_query, insert_query } => {
            if conn.execute_query(update_query).await?.rows_affected > 0 {
                Ok(true)
            } else {
                conn.execute_query(insert_query).await?;
                Ok(false)
            }
        }
        ImportAction::InsertOnly { query } => {
            conn.execute_query(query).await?;
            Ok(false)
        }
    }
}

/// Import CSV `content` into `table`, row by row. Row failures are collected,
/// not fatal. `progress(done, total)` is called after each row.
pub async fn import_csv(
    conn: &DatabaseConnection,
    table: &str,
    content: &str,
    quotes: (char, char),
    mut progress: impl FnMut(usize, usize),
) -> Result<ImportStats> {
    let (columns, rows) = parse_csv(content).map_err(|e| anyhow!("CSV parse error: {e}"))?;
    let actions = build_upsert_import_actions(table, &columns, &rows, quotes.0, quotes.1);
    let mut stats = ImportStats { total: actions.len(), ..Default::default() };
    for (i, action) in actions.iter().enumerate() {
        match apply_action(conn, action).await {
            Ok(true) => stats.updated += 1,
            Ok(false) => stats.inserted += 1,
            Err(e) => stats.errors.push(e.to_string()),
        }
        progress(i + 1, stats.total);
    }
    Ok(stats)
}

/// `"schema"."table"` with the database's identifier quotes.
pub fn qualified(schema: &str, table: &str, (q0, q1): (char, char)) -> String {
    format!("{q0}{schema}{q1}.{q0}{table}{q1}")
}

/// Export each `(schema, table)` to `<dir>/<table>.<ext>`.
pub async fn export_tables(
    conn: &DatabaseConnection,
    tables: &[(String, String)],
    dir: &Path,
    format: ExportFormat,
    quotes: (char, char),
    mut progress: impl FnMut(usize, usize, &str),
) -> BatchReport {
    let mut report = BatchReport { total: tables.len(), ..Default::default() };
    if let Err(e) = std::fs::create_dir_all(dir) {
        report.errors.push(format!("{}: {e}", dir.display()));
        return report;
    }
    for (i, (schema, table)) in tables.iter().enumerate() {
        progress(i, tables.len(), table);
        let full = qualified(schema, table, quotes);
        let file = dir.join(format!(
            "{}.{}",
            BatchExportState::clean_table_name(table),
            format.extension()
        ));
        let outcome = match conn.execute_query(&format!("SELECT * FROM {full}")).await {
            Ok(result) => export_to_file(&result, format, &file.to_string_lossy(), &full, quotes.0, quotes.1)
                .map_err(|e| anyhow!(e)),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(rows) => {
                report.succeeded += 1;
                report.rows_affected += rows as u64;
            }
            Err(e) => report.errors.push(format!("{table}: {e}")),
        }
    }
    report
}

/// Import `<dir>/<table>.csv` into each `(schema, table)`. A table stops at
/// its first failing row and counts as failed.
pub async fn import_tables(
    conn: &DatabaseConnection,
    tables: &[(String, String)],
    dir: &Path,
    quotes: (char, char),
    mut progress: impl FnMut(usize, usize, &str),
) -> BatchReport {
    let mut report = BatchReport { total: tables.len(), ..Default::default() };
    'tables: for (i, (schema, table)) in tables.iter().enumerate() {
        progress(i, tables.len(), table);
        let path = dir.join(format!("{}.csv", BatchExportState::clean_table_name(table)));
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                report.errors.push(format!("{table}: {e}"));
                continue;
            }
        };
        let (columns, rows) = match parse_csv(&content) {
            Ok(d) => d,
            Err(e) => {
                report.errors.push(format!("{table}: CSV parse error: {e}"));
                continue;
            }
        };
        let full = qualified(schema, table, quotes);
        for action in build_upsert_import_actions(&full, &columns, &rows, quotes.0, quotes.1) {
            match apply_action(conn, &action).await {
                Ok(_) => report.rows_affected += 1,
                Err(e) => {
                    report.errors.push(format!("{table}: {e}"));
                    continue 'tables;
                }
            }
        }
        report.succeeded += 1;
    }
    report
}
```

Note: the TUI used to show "N updated, M inserted" totals for batch import; `BatchReport` only keeps `rows_affected`. The TUI message becomes "Batch import complete: X tables, N rows from DIR" — acceptable simplification.

- [ ] **Step 5: Run tests**

Run: `rtk cargo test ops::transfer`
Expected: 3 PASS. If SQLite rejects the `"main"."t"` qualified name in the roundtrip test, that's a real behaviour of the existing code path: use the schema name returned by `refresh_schemas` for SQLite (check `src/engine/db/sqlite.rs::get_tables_by_schema`) in the test.

- [ ] **Step 6: Rewire the TUI**

In `src/tui/mod.rs`:
- `handle_import`: after the existing validations and `read_to_string`, replace parsing + the action loop with

```rust
let quotes = get_quote_chars(state);
let conn = state.connection.as_ref().unwrap();
let stats = match ops::transfer::import_csv(conn, &import_state.target_table, &content, quotes, |_, _| {}).await {
    Ok(s) => s,
    Err(e) => {
        state.set_status(e.to_string());
        state.close_dialog();
        return;
    }
};
```

  and build the three status messages from `stats.succeeded()`, `stats.total`, `stats.updated`, `stats.inserted`, `stats.errors.last()`. The `terminal` parameter stays (used for the initial "Importing…" redraw).
- `handle_batch_export`: replace the loop with `ops::transfer::export_tables(conn, &selected_tables, &dir, batch.format, quotes, |_, _, _| {}).await` and derive the messages from the `BatchReport`.
- `handle_batch_import`: same with `ops::transfer::import_tables(conn, &selected_tables, Path::new(&batch.directory), quotes, |_, _, _| {}).await`.

- [ ] **Step 7: Run all tests + smoke test**

Run: `rtk cargo test` → all PASS. Then in the TUI on a SQLite file: export a table to CSV, truncate it, re-import it; batch export two tables, batch import them.

- [ ] **Step 8: Commit**

```bash
rtk git add -A src Cargo.toml Cargo.lock
rtk git commit -m "refactor: extract import/export into engine::ops::transfer"
```

---

### Task 9: Cleanup

- [ ] **Step 1: Confirm the engine is UI-free**

Run: `rtk grep "ratatui\|crossterm\|crate::tui" src/engine`
Expected: no matches.

- [ ] **Step 2: Clippy**

Run: `rtk cargo clippy --all-targets`
Expected: no new warnings compared to `master` (fix unused imports left by the extraction).

- [ ] **Step 3: Update README project structure** — replace the "Project Structure" block with the tree from the top of this plan (engine/ and tui/ only; gui/ and updater/ are added by plans 2–3).

- [ ] **Step 4: Commit**

```bash
rtk git add -A
rtk git commit -m "chore: tidy after engine extraction"
```
