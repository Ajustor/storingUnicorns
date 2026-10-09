# Engine additions for a DataGrip-like GUI — Implementation Plan (3a/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `engine/` everything the DataGrip-like GUI (plan 3b) needs: optional model fields, capped result fetching, multi-result script execution, paged/sorted/filtered table queries, transactional submission of pending edits, index/foreign-key metadata, DDL, query history and SQL formatting.

**Architecture:** Pure functions in `engine::sql::*` (unit-tested without a database), async operations in `engine::ops::*` taking `&DatabaseConnection` (tested on in-memory SQLite via `engine::ops::test_support::sqlite_mem`), connector additions in `engine::db::*` for the four dialects (only SQLite is runnable here: Postgres/MySQL/SQL Server code must compile and follow the same shape). Nothing here depends on egui; the TUI keeps working unchanged.

**Tech Stack:** Rust, tokio, sqlx 0.8 (`fetch_rows_and_result` in `engine/db/utils.rs`), tiberius 0.12, `sqlformat`, serde_json.

Spec: `docs/superpowers/specs/2026-10-08-gui-distribution-design.md`, section "GUI" → "Ajouts au moteur pour la GUI".

Conventions:
- Cargo must be run as `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo <cmd>` on this machine.
- TDD: failing test first, run it, implement, run it. One commit per task, message trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- `cargo test` green and no new `cargo clippy --all-targets` warnings before each commit.
- Existing conventions to respect: a cell value equal to the string `"NULL"` means SQL NULL in `db::utils` builders; identifiers are quoted with `engine::sql::statements::quote_chars(db_type)`; qualified names come from `engine::ops::transfer::qualified(schema, table, quotes)`.

---

### Task 1: Optional model fields

**Files:** `src/engine/models/connection.rs`, `src/engine/services/query_tabs.rs`, every place constructing these structs with literal syntax (grep).

- [ ] **Step 1: Failing tests** — add to a `#[cfg(test)] mod tests` in `src/engine/models/connection.rs`:

```rust
#[test]
fn old_connection_toml_still_parses_without_color() {
    let c: ConnectionConfig = toml::from_str(
        "name = \"x\"\ndb_type = \"SQLite\"\ndatabase = \"a.db\"",
    )
    .unwrap();
    assert_eq!(c.color, None);
}

#[test]
fn query_result_defaults_to_not_truncated() {
    assert!(!QueryResult::default().truncated);
}
```

and in `src/engine/services/query_tabs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_tabs_json_parses_without_connection() {
        let t: QueryTab = serde_json::from_str(r#"{"name":"Q","query":"SELECT 1","cursor_position":0}"#).unwrap();
        assert_eq!(t.connection, None);
    }
}
```

(Check which format `QueryTabsState::save` uses — JSON or TOML — and write the test in that format.)

- [ ] **Step 2: Run** → FAIL (fields missing).

- [ ] **Step 3: Implement**
  - `ConnectionConfig`: `#[serde(default)] pub color: Option<[u8; 3]>,` — RGB of the connection's colour; `Default` impl sets `None`.
  - `QueryTab`: `#[serde(default)] pub connection: Option<String>,` — name of the connection the console is bound to; constructors set `None`.
  - `QueryResult`: `#[serde(skip)]`-free plain field `pub truncated: bool,` (it derives `Default`): true when fetching stopped at a row cap.
  - Fix every struct literal the compiler flags (`..Default::default()` where possible).

- [ ] **Step 4: Run** → PASS. **Commit** `feat(engine): connection colour, console connection and truncated flag`.

---

### Task 2: Row cap when fetching

**Files:** `src/engine/db/utils.rs`, `src/engine/db/{sqlite,postgres,mysql,sqlserver,azure}.rs`, `src/engine/db/connector.rs`

- [ ] **Step 1: Failing test** in `src/engine/db/sqlite.rs` tests:

```rust
#[tokio::test]
async fn row_cap_truncates_and_flags() {
    let pool = mem_pool().await; // existing helper: table t(a INTEGER)
    for i in 0..10 {
        sqlx::query(&format!("INSERT INTO t VALUES ({i})")).execute(&pool).await.unwrap();
    }
    let r = execute_query_limited(&pool, "SELECT a FROM t ORDER BY a", Some(3)).await.unwrap();
    assert_eq!(r.rows.len(), 3);
    assert!(r.truncated);
    let r = execute_query_limited(&pool, "SELECT a FROM t", Some(10)).await.unwrap();
    assert_eq!(r.rows.len(), 10);
    assert!(!r.truncated);
    let r = execute_query_limited(&pool, "SELECT a FROM t", None).await.unwrap();
    assert!(!r.truncated);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**
  - `fetch_rows_and_result(executor, query, max_rows: Option<usize>) -> sqlx::Result<(Vec<Row>, QueryResult, bool /*truncated*/)>`: stop pulling the stream once `rows.len() == max_rows` **and** one more row arrives (read max+1 to know it's truncated, keep max). Dropping the stream cancels the rest of the fetch.
  - Each sqlx connector: `execute_query_limited(pool, query, max_rows)`; `execute_query(pool, q)` becomes `execute_query_limited(pool, q, None)`. Set `truncated` on the `QueryResult`.
  - SQL Server (`sqlserver.rs`, used by Azure too): stop reading the tiberius `QueryStream` after `max_rows` rows (+1 to detect truncation) and drop it. If dropping a partially-read stream leaves the client in a bad state, instead read and discard the remainder but don't store it (still saves memory), and say so in a comment.
  - `DatabaseConnection::execute_query_limited(&self, query, max_rows: Option<usize>)` dispatching like `execute_query`; `execute_query` delegates with `None`. Keep `execution_time_ms` handling.

- [ ] **Step 4: Run** → PASS (plus all existing tests). **Commit** `feat(db): optional row cap with truncated flag`.

---

### Task 3: Multi-result script execution

**Files:** `src/engine/ops/query.rs`

Interface:

```rust
/// One executed unit of a script (a statement, or a whole transaction block).
#[derive(Debug)]
pub struct StatementOutcome {
    /// The SQL as executed (a transaction block is joined with ";\n").
    pub sql: String,
    pub result: Result<QueryResult, String>,
    pub elapsed_ms: u128,
}

/// Execute every statement of `text` in order. Transaction blocks
/// (BEGIN … COMMIT/ROLLBACK/END) run as one unit via `execute_transaction`.
/// Execution stops after the first failing unit (its error is the last outcome).
/// Row-returning results are capped at `max_rows` and enriched with
/// PK/nullability like `run_query` when they read a single table.
pub async fn run_script(conn: &DatabaseConnection, text: &str, max_rows: Option<usize>) -> Vec<StatementOutcome>;

/// Execute the statement or transaction block under the cursor (Ctrl+Enter
/// without selection), with the same outcome shape.
pub async fn run_at_cursor_outcomes(conn: &DatabaseConnection, text: &str, cursor: usize, max_rows: Option<usize>) -> Vec<StatementOutcome>;
```

- [ ] **Step 1: Failing tests** (append to the tests module of `query.rs`, reuse its `SETUP`):

```rust
#[tokio::test]
async fn run_script_returns_one_outcome_per_statement() {
    let conn = sqlite_mem(SETUP).await;
    let out = run_script(&conn, "SELECT * FROM users; UPDATE users SET email = 'z'; SELECT 1 AS one", None).await;
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].result.as_ref().unwrap().rows.len(), 2);
    assert_eq!(out[1].result.as_ref().unwrap().rows_affected, 2);
    assert_eq!(out[2].result.as_ref().unwrap().rows[0][0], "1");
}

#[tokio::test]
async fn run_script_groups_transactions_and_stops_on_error() {
    let conn = sqlite_mem(SETUP).await;
    let sql = "BEGIN; INSERT INTO users (name) VALUES ('C'); COMMIT; SELECT * FROM nope; SELECT 2";
    let out = run_script(&conn, sql, None).await;
    assert_eq!(out.len(), 2, "transaction block + failing select, then stop");
    assert!(out[0].result.is_ok());
    assert!(out[0].sql.contains("INSERT"));
    assert!(out[1].result.is_err());
    assert_eq!(count(&conn, "users").await, "3");
}

#[tokio::test]
async fn run_script_caps_rows_and_enriches_columns() {
    let conn = sqlite_mem(SETUP).await;
    let out = run_script(&conn, "SELECT * FROM users", Some(1)).await;
    let r = out[0].result.as_ref().unwrap();
    assert_eq!(r.rows.len(), 1);
    assert!(r.truncated);
    assert!(r.columns.iter().find(|c| c.name == "id").unwrap().is_primary_key);
}

#[tokio::test]
async fn run_at_cursor_outcomes_reports_unterminated_transaction() {
    let conn = sqlite_mem(SETUP).await;
    let out = run_at_cursor_outcomes(&conn, "BEGIN; SELECT 1;", 0, None).await;
    assert_eq!(out.len(), 1);
    assert!(out[0].result.as_ref().unwrap_err().contains("COMMIT or ROLLBACK"));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** using `split_statements`, `is_transaction_start`, `is_transaction_end` from `engine::sql::statements`, `DatabaseConnection::execute_query_limited` (Task 2) and `execute_transaction`. Factor the PK/nullability enrichment out of `run_query` into a private `enrich(conn, sql, &mut QueryResult)` and reuse it. An unterminated `BEGIN` in `run_script` produces an error outcome "Transaction not terminated: add COMMIT or ROLLBACK" and stops. Time each unit with `std::time::Instant`.

- [ ] **Step 4: Run** → PASS. **Commit** `feat(engine): run scripts as multiple outcomes with row cap`.

---

### Task 4: Paged, sorted, filtered table queries

**Files:** Create `src/engine/sql/paging.rs`; modify `src/engine/sql/mod.rs`.

Interface:

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataQuery {
    /// Already-qualified, already-quoted table name.
    pub table: String,
    /// Raw SQL condition typed by the user (without `WHERE`), may be empty.
    pub filter: String,
    /// Raw SQL ordering typed by the user or built by header clicks (without `ORDER BY`), may be empty.
    pub order_by: String,
    /// 0-based page index.
    pub page: usize,
    pub page_size: usize,
}

pub fn build_select(q: &DataQuery, db: &DatabaseType) -> String;
pub fn build_count(q: &DataQuery) -> String;
/// Header click: none → `col ASC` → `col DESC` → none. `column` is quoted.
pub fn toggle_order(current: &str, column: &str) -> String;
```

- [ ] **Step 1: Failing tests** (in `paging.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn q(filter: &str, order: &str, page: usize) -> DataQuery {
        DataQuery { table: "\"t\"".into(), filter: filter.into(), order_by: order.into(), page, page_size: 500 }
    }

    #[test]
    fn limit_offset_dialects() {
        assert_eq!(build_select(&q("", "", 0), &DatabaseType::Postgres), "SELECT * FROM \"t\" LIMIT 500 OFFSET 0");
        assert_eq!(
            build_select(&q("a > 1", "a DESC", 2), &DatabaseType::SQLite),
            "SELECT * FROM \"t\" WHERE (a > 1) ORDER BY a DESC LIMIT 500 OFFSET 1000"
        );
        assert_eq!(build_select(&q("", "", 1), &DatabaseType::MySQL), "SELECT * FROM \"t\" LIMIT 500 OFFSET 500");
    }

    #[test]
    fn sql_server_needs_order_by_for_offset() {
        assert_eq!(
            build_select(&q("", "", 0), &DatabaseType::SQLServer),
            "SELECT * FROM \"t\" ORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 500 ROWS ONLY"
        );
        assert_eq!(
            build_select(&q("x = 1", "[x]", 1), &DatabaseType::Azure),
            "SELECT * FROM \"t\" WHERE (x = 1) ORDER BY [x] OFFSET 500 ROWS FETCH NEXT 500 ROWS ONLY"
        );
    }

    #[test]
    fn filter_and_order_are_trimmed_and_wrapped() {
        // A user filter containing OR must not escape the WHERE when combined later.
        assert_eq!(build_count(&q("  a = 1 OR b = 2 ", "", 0)), "SELECT COUNT(*) FROM \"t\" WHERE (a = 1 OR b = 2)");
        assert_eq!(build_count(&q("", "x", 0)), "SELECT COUNT(*) FROM \"t\"");
    }

    #[test]
    fn toggle_order_cycles() {
        assert_eq!(toggle_order("", "\"a\""), "\"a\" ASC");
        assert_eq!(toggle_order("\"a\" ASC", "\"a\""), "\"a\" DESC");
        assert_eq!(toggle_order("\"a\" DESC", "\"a\""), "");
        assert_eq!(toggle_order("\"b\" DESC", "\"a\""), "\"a\" ASC");
    }
}
```

The user filter is always wrapped in parentheses (`WHERE (<filter>)`) so an `OR` inside it can't leak; blank filter/order are omitted.

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS. **Commit** `feat(engine): paged/sorted/filtered table queries per dialect`.

---

### Task 5: Submit pending edits in one transaction

**Files:** `src/engine/ops/rows.rs`

Interface:

```rust
/// Pending edits of a data grid, applied together by `submit_changes`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RowChanges {
    /// (original row, edited row)
    pub updates: Vec<(Vec<String>, Vec<String>)>,
    pub inserts: Vec<Vec<String>>,
    /// Original rows to delete.
    pub deletes: Vec<Vec<String>>,
}

impl RowChanges {
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
}

/// `(begin, commit)` statements for the dialect.
pub fn transaction_bounds(db: &DatabaseType) -> (&'static str, &'static str);

/// Build the statements for `changes` (deletes, then updates, then inserts),
/// wrap them in the dialect's BEGIN/COMMIT and run them with
/// `execute_transaction` (rolled back on any error). Returns the number of
/// statements applied (unchanged updates are skipped).
pub async fn submit_changes(
    conn: &DatabaseConnection,
    db: &DatabaseType,
    table: &str,
    columns: &[Column],
    system_columns: &[usize],
    changes: &RowChanges,
) -> Result<usize>;
```

Bounds: Postgres/SQLite `("BEGIN", "COMMIT")`, MySQL `("START TRANSACTION", "COMMIT")`, SQL Server/Azure `("BEGIN TRANSACTION", "COMMIT")`. Statements come from `db::utils::build_delete_query`, `build_update_query` (returns `None` when nothing changed → skip), `build_insert_query` (returns `None` when there is nothing to insert → skip), with `quote_chars(db)`.

- [ ] **Step 1: Failing tests** (rows.rs tests module; reuse `cols()`/`row()` helpers and `SETUP`):

```rust
#[tokio::test]
async fn submit_applies_all_changes_atomically() {
    let conn = sqlite_mem(SETUP).await;
    let changes = RowChanges {
        updates: vec![(row("1", "a"), row("1", "A"))],
        inserts: vec![row("", "c")],
        deletes: vec![row("2", "b")],
    };
    let n = submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &changes).await.unwrap();
    assert_eq!(n, 3);
    let r = conn.execute_query("SELECT name FROM t ORDER BY name").await.unwrap();
    let names: Vec<_> = r.rows.iter().map(|r| r[0].clone()).collect();
    assert_eq!(names, ["A", "c"]);
}

#[tokio::test]
async fn submit_rolls_back_everything_on_error() {
    let conn = sqlite_mem(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "INSERT INTO t (id, name) VALUES (1, 'a')",
    ])
    .await;
    let changes = RowChanges {
        updates: vec![(row("1", "a"), row("1", "z"))],
        inserts: vec![row("", "NULL")], // violates NOT NULL
        deletes: vec![],
    };
    assert!(submit_changes(&conn, &DatabaseType::SQLite, "t", &cols(), &[0], &changes).await.is_err());
    let r = conn.execute_query("SELECT name FROM t").await.unwrap();
    assert_eq!(r.rows[0][0], "a", "update must be rolled back");
}

#[test]
fn bounds_per_dialect() {
    assert_eq!(transaction_bounds(&DatabaseType::MySQL).0, "START TRANSACTION");
    assert_eq!(transaction_bounds(&DatabaseType::Azure).0, "BEGIN TRANSACTION");
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS. **Commit** `feat(engine): submit grid edits in a single transaction`.

---

### Task 6: Index and foreign-key metadata

**Files:** `src/engine/models/connection.rs`, `src/engine/db/{connector,sqlite,postgres,mysql,sqlserver}.rs`, `src/engine/ops/schema.rs`

Models:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo { pub name: String, pub columns: Vec<String>, pub unique: bool, pub primary: bool }

#[derive(Debug, Clone, PartialEq)]
pub struct ForeignKeyInfo { pub name: String, pub columns: Vec<String>, pub ref_table: String, pub ref_columns: Vec<String> }

#[derive(Debug, Clone, Default)]
pub struct TableDetails { pub columns: Vec<Column>, pub indexes: Vec<IndexInfo>, pub foreign_keys: Vec<ForeignKeyInfo> }
```

Connector methods `get_indexes(table) -> Result<Vec<IndexInfo>>`, `get_foreign_keys(table) -> Result<Vec<ForeignKeyInfo>>` on `DatabaseConnection`, implemented per dialect. `table` may arrive qualified/quoted (`"main"."t"`, `[dbo].[t]`, `` `db`.`t` ``): reuse whatever unquoting the existing `get_table_column_details` of that connector does, so behaviour is consistent.
- SQLite: `PRAGMA index_list('t')` (+ `PRAGMA index_info('<index>')` for columns; `origin = 'pk'` → primary) and `PRAGMA foreign_key_list('t')` (group rows by `id`; columns `from`, `table`, `to`).
- Postgres: `pg_index` joined with `pg_class`/`pg_attribute` (`indisunique`, `indisprimary`); FKs from `information_schema.table_constraints` + `key_column_usage` + `constraint_column_usage` where `constraint_type = 'FOREIGN KEY'`.
- MySQL: `information_schema.STATISTICS` (`NON_UNIQUE`, `INDEX_NAME = 'PRIMARY'`, ordered by `SEQ_IN_INDEX`); FKs from `information_schema.KEY_COLUMN_USAGE` where `REFERENCED_TABLE_NAME IS NOT NULL`, `TABLE_SCHEMA = DATABASE()`.
- SQL Server/Azure: `sys.indexes` + `sys.index_columns` + `sys.columns` (`is_unique`, `is_primary_key`); FKs from `sys.foreign_keys` + `sys.foreign_key_columns`.

`ops::schema::table_details(conn, cache, table) -> Result<TableDetails>`: columns via the existing cache path (`fetch_columns` logic but returning `Column`s), indexes and FKs fetched concurrently with `tokio::try_join!`.

- [ ] **Step 1: Failing test** (schema.rs tests):

```rust
#[tokio::test]
async fn table_details_lists_indexes_and_foreign_keys() {
    let conn = sqlite_mem(&[
        "CREATE TABLE a (id INTEGER PRIMARY KEY, code TEXT UNIQUE)",
        "CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), label TEXT)",
        "CREATE INDEX b_label ON b(label)",
    ])
    .await;
    let cache = TableCache::default();
    let d = table_details(&conn, &cache, "b").await.unwrap();
    assert_eq!(d.columns.len(), 3);
    assert!(d.indexes.iter().any(|i| i.name == "b_label" && i.columns == ["label"] && !i.unique));
    assert_eq!(d.foreign_keys.len(), 1);
    assert_eq!(d.foreign_keys[0].ref_table, "a");
    assert_eq!(d.foreign_keys[0].columns, ["a_id"]);
    let da = table_details(&conn, &cache, "a").await.unwrap();
    assert!(da.indexes.iter().any(|i| i.unique && i.columns == ["code"]));
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** (SQLite first until green, then the three other dialects; they must compile). **Step 4: Run** → PASS. **Commit** `feat(db): index and foreign-key metadata`.

---

### Task 7: DDL

**Files:** `src/engine/ops/schema.rs`

```rust
/// `CREATE TABLE` (+ `CREATE INDEX`) for `table`: native for SQLite
/// (`sqlite_master.sql`) and MySQL (`SHOW CREATE TABLE`), generated from
/// metadata for Postgres and SQL Server.
pub async fn table_ddl(conn: &DatabaseConnection, cache: &TableCache, db: &DatabaseType, table: &str) -> Result<String>;

/// Pure generator used for Postgres / SQL Server.
pub fn generate_ddl(table: &str, details: &TableDetails, quotes: (char, char)) -> String;
```

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn generate_ddl_includes_columns_pk_fk_and_indexes() {
    use crate::engine::models::{Column, ForeignKeyInfo, IndexInfo};
    let col = |n: &str, t: &str, null: bool, pk: bool| Column { name: n.into(), type_name: t.into(), nullable: null, is_primary_key: pk };
    let d = TableDetails {
        columns: vec![col("id", "integer", false, true), col("a_id", "integer", true, false)],
        indexes: vec![IndexInfo { name: "ix_a".into(), columns: vec!["a_id".into()], unique: false, primary: false }],
        foreign_keys: vec![ForeignKeyInfo { name: "fk_a".into(), columns: vec!["a_id".into()], ref_table: "a".into(), ref_columns: vec!["id".into()] }],
    };
    let ddl = generate_ddl("\"b\"", &d, ('"', '"'));
    assert!(ddl.starts_with("CREATE TABLE \"b\" (\n"));
    assert!(ddl.contains("  \"id\" integer NOT NULL"));
    assert!(ddl.contains("  \"a_id\" integer,") || ddl.contains("  \"a_id\" integer\n"));
    assert!(ddl.contains("PRIMARY KEY (\"id\")"));
    assert!(ddl.contains("CONSTRAINT \"fk_a\" FOREIGN KEY (\"a_id\") REFERENCES \"a\" (\"id\")"));
    assert!(ddl.contains("CREATE INDEX \"ix_a\" ON \"b\" (\"a_id\");"));
}

#[tokio::test]
async fn sqlite_ddl_is_native() {
    let conn = sqlite_mem(&["CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT)", "CREATE INDEX t_x ON t(x)"]).await;
    let ddl = table_ddl(&conn, &TableCache::default(), &DatabaseType::SQLite, "t").await.unwrap();
    assert!(ddl.contains("CREATE TABLE t (id INTEGER PRIMARY KEY, x TEXT)"));
    assert!(ddl.contains("CREATE INDEX t_x ON t(x)"));
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** (statements separated by `;\n\n`, unique indexes as `CREATE UNIQUE INDEX`, primary indexes skipped since they're in the table body). **Step 4: Run** → PASS. **Commit** `feat(engine): table DDL`.

---

### Task 8: Query history

**Files:** Create `src/engine/services/history.rs`; modify `src/engine/services/mod.rs`.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sql: String,
    pub connection: String,
    /// Unix seconds.
    pub at: u64,
    pub duration_ms: u64,
    pub ok: bool,
}

pub struct History { entries: Vec<HistoryEntry>, path: PathBuf }

impl History {
    pub const MAX: usize = 500;
    /// `~/.config/storing-unicorns/history.json` (same directory as config.toml).
    pub fn default_path() -> anyhow::Result<PathBuf>;
    /// Missing or unreadable file → empty history (never fails the app).
    pub fn load_from(path: PathBuf) -> Self;
    /// Newest last. Skips an entry identical (same sql + connection) to the
    /// newest one (updates its `at`/`duration_ms`/`ok` instead). Keeps the last MAX.
    pub fn push(&mut self, entry: HistoryEntry);
    pub fn save(&self) -> anyhow::Result<()>;
    /// Newest first, case-insensitive substring match on `sql`; empty query → all.
    pub fn search(&self, query: &str) -> Vec<&HistoryEntry>;
}
```

- [ ] **Step 1: Failing tests** (use `tempfile::tempdir()`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn e(sql: &str, at: u64) -> HistoryEntry {
        HistoryEntry { sql: sql.into(), connection: "c".into(), at, duration_ms: 1, ok: true }
    }

    #[test]
    fn push_dedupes_consecutive_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load_from(dir.path().join("h.json"));
        h.push(e("SELECT 1", 1));
        h.push(e("SELECT 1", 2));
        assert_eq!(h.search("").len(), 1);
        assert_eq!(h.search("")[0].at, 2);
        for i in 0..(History::MAX + 10) {
            h.push(e(&format!("SELECT {i}"), i as u64));
        }
        assert_eq!(h.search("").len(), History::MAX);
    }

    #[test]
    fn search_is_newest_first_and_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load_from(dir.path().join("h.json"));
        h.push(e("select * from users", 1));
        h.push(e("SELECT * FROM orders", 2));
        h.push(e("SELECT * FROM Users WHERE id = 1", 3));
        let r = h.search("users");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].at, 3);
    }

    #[test]
    fn save_and_reload_roundtrip_and_corrupt_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.json");
        let mut h = History::load_from(path.clone());
        h.push(e("SELECT 1", 1));
        h.save().unwrap();
        assert_eq!(History::load_from(path.clone()).search("").len(), 1);
        std::fs::write(&path, "{nope").unwrap();
        assert!(History::load_from(path).search("").is_empty());
    }
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** (serde_json). **Step 4: Run** → PASS. **Commit** `feat(engine): persistent query history`.

---

### Task 9: SQL formatting

**Files:** `Cargo.toml` (`sqlformat`, latest 0.3.x), create `src/engine/sql/format.rs`, modify `src/engine/sql/mod.rs`.

```rust
/// Reformat SQL: uppercase keywords, 2-space indent, one blank line between statements.
pub fn format_sql(text: &str) -> String;
```

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_uppercases() {
        let out = format_sql("select a, b from t where a = 1");
        assert!(out.starts_with("SELECT"));
        assert!(out.contains("\nFROM"));
        assert!(out.contains("\nWHERE"));
    }

    #[test]
    fn keeps_string_literals_untouched() {
        assert!(format_sql("select 'from x' as s").contains("'from x'"));
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(format_sql("   "), "");
    }
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** with `sqlformat::format(text, &QueryParams::None, &FormatOptions { indent: Indent::Spaces(2), uppercase: Some(true), lines_between_queries: 2, ..Default::default() })` (adapt to the resolved crate version's API; check its docs in `~/.cargo/registry/src`). Trim the input; return `String::new()` for blank input. **Step 4: Run** → PASS. **Commit** `feat(engine): SQL formatting`.
