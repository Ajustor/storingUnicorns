# DataGrip-like GUI — Implementation Plan (3b/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An egui desktop client with DataGrip ergonomics and better performance: database explorer, several simultaneous connections with colours, consoles with one result tab per statement + output log + history + formatting, table data editors with paging/sort/filter and inline editing submitted in one transaction, value panel, DDL, structure and import/export dialogs, in-app updates.

**Architecture:** `src/gui/` is an `eframe::App`. Every database operation goes through `gui::worker::Worker` (tokio runtime, `Event`s drained each frame, each event tagged with the connection name and/or tab id it belongs to). Open connections live in `gui::sessions::Sessions` (`connection name → Session { Arc<DatabaseConnection>, schemas, per-table details }`). The centre area is a list of `Tab`s (console, data editor, DDL), each bound to a connection. The data grid is one reusable widget (`gui::grid`) used by console results and data editors; its pending edits are a pure, unit-tested model (`gui::grid::changes`) converted to `engine::ops::rows::RowChanges` on Submit. Pure logic gets unit tests; rendering is checked manually with the checklist at the end of each task.

**Tech Stack:** eframe/egui 0.31, egui_extras 0.31 (`TableBuilder`), egui-phosphor 0.9, rfd 0.15, tokio; engine APIs from plans 1 and 3a.

**Prerequisites:** plans 1, 2 and 3a done. Read the spec section "GUI" in `docs/superpowers/specs/2026-10-08-gui-distribution-design.md` before starting any task — it is the acceptance reference for layout, behaviours and shortcuts.

**Reference code:** `docs/superpowers/plans/2026-10-08-3-gui.md` (superseded single-connection plan) contains complete, reusable code for: `theme.rs` (its Task 1 Step 2), the connection form `dialogs/connection.rs` (Task 4 Step 3), SQL highlighting `highlight_job`/`char_to_byte`/`set_cursor` (Task 6 Step 3), the completion popup (Task 7), the status bar and update banner (Task 3 Step 3), the structure dialog (Task 10) and transfer dialogs (Task 11). Tasks below say when to reuse them and what to change.

Conventions: cargo as `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo <cmd>`; TDD for every pure function; one commit per task, trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; no new clippy warnings; UI strings in French, short. Performance rules (the point of this tool vs DataGrip): never block the UI thread on I/O; never clone a whole `QueryResult` per frame; lay out only visible rows; compute filtered/sorted index vectors only when inputs change; `request_repaint` only when something changed or while busy.

---

## File structure

```
src/gui/
├── mod.rs              # run(): NativeOptions, icon later (plan 4), exit action
├── app.rs              # App: owns everything, frame loop, event routing, global shortcuts
├── theme.rs            # from superseded plan + connection colour helpers
├── worker.rs           # async operations, Event enum with routing ids
├── sessions.rs         # open connections and their cached metadata
├── explorer.rs         # left tree + filter + context menus
├── table_search.rs     # Ctrl+N popup
├── status.rs           # status bar + update banner
├── value_panel.rs      # right panel
├── history_popup.rs    # Ctrl+Alt+E
├── editor/
│   ├── mod.rs          # SQL TextEdit with highlighting, cursor/selection helpers
│   └── completion.rs   # popup (from superseded plan)
├── grid/
│   ├── mod.rs          # DataGrid widget (virtualised, sort, select, inline edit)
│   └── changes.rs      # PendingEdits model (pure)
├── tabs/
│   ├── mod.rs          # Tab, TabId, tab bar
│   ├── console.rs      # ConsoleTab: editor + result tabs + output log
│   ├── data.rs         # DataTab: table data editor
│   └── ddl.rs          # DdlTab
└── dialogs/
    ├── mod.rs          # Dialog enum + dispatch
    ├── connection.rs   # from superseded plan + colour
    ├── structure.rs    # from superseded plan, per connection
    └── transfer.rs     # from superseded plan, per connection
```

---

### Task 1: Dependencies, skeleton, theme, entry point

Do **exactly** Task 1 of the superseded plan (`2026-10-08-3-gui.md`), Steps 1–7 (dependencies + release profile, `theme.rs`, skeleton `app.rs`, `mod.rs`, wiring `Mode::Gui` with `console::prepare_gui()` and `tracing_to_file()`), with one addition to `theme.rs`:

```rust
/// Default palette offered in the connection form (DataGrip-like).
pub const CONNECTION_COLORS: [[u8; 3]; 7] = [
    [0xE5, 0x5C, 0x6C], // prod red
    [0xF2, 0xA6, 0x4C], // orange
    [0xF2, 0xC9, 0x4C], // yellow
    [0x4C, 0xC3, 0x8A], // green
    [0x4C, 0x9E, 0xE5], // blue
    [0xB3, 0x6B, 0xFF], // purple
    [0x9A, 0x9A, 0x9A], // grey
];

pub fn rgb(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}
```

Commit `feat(gui): eframe skeleton, theme and GUI entry point`.

---

### Task 2: Worker with connection/tab routing

**Files:** create `src/gui/worker.rs`.

Same mechanics as the superseded plan's Task 2 (tokio `Runtime`, `mpsc` channel, `repaint` callback, `spawn` helper, `progress` helper, `poll()`), with these differences:

```rust
pub type Conn = Arc<DatabaseConnection>;
pub type TabId = u64;

pub enum Event {
    Connected { name: String, conn: Conn, schemas: Vec<SchemaInfo> },
    ConnectFailed { name: String, error: String },
    TestFinished(Result<(), String>),
    Schemas { name: String, outcome: Result<Vec<SchemaInfo>, String> },
    Details { name: String, table: String, outcome: Result<TableDetails, String> },
    /// Console execution (plan 3a `StatementOutcome`s).
    Script { tab: TabId, outcomes: Vec<StatementOutcome> },
    /// One page of a data editor.
    Page { tab: TabId, outcome: Result<QueryResult, String> },
    Count { tab: TabId, outcome: Result<u64, String> },
    Submitted { tab: TabId, outcome: Result<usize, String> },
    Ddl { tab: TabId, outcome: Result<String, String> },
    SchemaApplied { name: String, table: String, outcome: Result<String, String> },
    Exported(Result<(usize, PathBuf), String>),
    Imported { table: String, outcome: Result<ImportStats, String> },
    Batch { kind: &'static str, report: BatchReport },
    Progress { done: usize, total: usize, label: String },
}
```

Methods (each spawns, sends the matching event):
- `connect(config)`, `test_connection(config)`, `refresh_schemas(name, conn)`, `table_details(name, conn, table)` (uses `engine::ops::schema::table_details` with a **per-connection** `TableCache`: keep `HashMap<String, Arc<TableCache>>` in the worker, `cache_for(name)`; `forget(name)` drops it on disconnect).
- `run_script(tab, conn, text, max_rows)`, `run_at_cursor(tab, conn, text, cursor, max_rows)` — tracked in `running: HashMap<TabId, JoinHandle<()>>`; `cancel(tab) -> bool`, `is_running(tab) -> bool`, `any_running() -> bool`.
- `load_page(tab, conn, sql)` (uses `execute_query_limited(sql, None)` + the plan-1 `run_query` enrichment — call `engine::ops::query::run_query`), `count(tab, conn, sql)` (parse first cell as `u64`), `submit(tab, conn, db_type, table, columns, system_columns, changes)`, `ddl(tab, conn, db_type, table)`, `apply_schema(name, conn, m, db_type)`, and the transfer methods of the superseded plan (`export_result`, `import_csv`, `export_tables`, `import_tables`, `truncate_tables`).

- [ ] **Step 1: Failing tests** — adapt the superseded plan's three worker tests: `connect_then_script` (connect, `run_script(1, conn, "SELECT * FROM t", Some(1000))`, expect `Event::Script { tab: 1, outcomes }` with 2 rows), `connect_failure_is_reported`, `cancel_aborts_running_query` (via `cancel(1)` / `is_running(1)`), plus:

```rust
#[test]
fn page_count_and_submit_events_carry_tab_id() {
    // connect to the temp SQLite db, then:
    // load_page(7, conn, "SELECT * FROM t LIMIT 500 OFFSET 0") -> Event::Page { tab: 7, Ok(r) } with 2 rows
    // count(7, conn, "SELECT COUNT(*) FROM t") -> Event::Count { tab: 7, Ok(2) }
    // submit(7, conn, SQLite, "t", r.columns, vec![0], RowChanges{ deletes: vec![r.rows[0].clone()], ..}) -> Event::Submitted { tab: 7, Ok(1) }
}
```

Write the body out fully using the `next_event` helper; match events with `let Event::… = next_event(&mut w) else { panic!() }`.

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS. **Commit** `feat(gui): worker with per-connection and per-tab routing`.

---

### Task 3: Pending edits model

**Files:** create `src/gui/grid/changes.rs` (and `src/gui/grid/mod.rs` with `pub mod changes;` for now).

The grid shows `base` rows (from the database) plus inserted rows; edits never mutate `base` until Submit succeeds.

```rust
use std::collections::{BTreeMap, BTreeSet};

use crate::engine::ops::rows::RowChanges;

/// Identifies a displayed row: an existing row by its index in the base
/// result, or a new row by its index in `inserted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowRef {
    Base(usize),
    New(usize),
}

#[derive(Debug, Default, Clone)]
pub struct PendingEdits {
    /// (base row, column) → new value
    edited: BTreeMap<(usize, usize), String>,
    deleted: BTreeSet<usize>,
    inserted: Vec<Vec<String>>,
}

impl PendingEdits {
    pub fn is_empty(&self) -> bool;
    /// Number of changed rows (edited rows + deleted + inserted).
    pub fn count(&self) -> usize;
    /// Value to display for a cell, taking edits into account.
    pub fn value<'a>(&'a self, base: &'a [Vec<String>], row: RowRef, col: usize) -> &'a str;
    pub fn is_edited(&self, row: RowRef, col: usize) -> bool;   // New rows: false (whole row is new)
    pub fn is_deleted(&self, row: RowRef) -> bool;
    /// Set a cell. Setting a base cell back to its original value removes the edit.
    pub fn set(&mut self, base: &[Vec<String>], row: RowRef, col: usize, value: String);
    /// Append an empty new row (`ncols` empty strings); returns its ref.
    pub fn add_row(&mut self, ncols: usize) -> RowRef;
    /// Base row: toggle deletion mark (and drop its edits when marking). New row: remove it.
    pub fn toggle_delete(&mut self, row: RowRef);
    pub fn new_rows(&self) -> usize;
    /// Convert to the engine format. Base rows both edited and deleted only appear in `deletes`.
    pub fn to_row_changes(&self, base: &[Vec<String>]) -> RowChanges;
    pub fn clear(&mut self);
}
```

- [ ] **Step 1: Failing tests** (`changes.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<Vec<String>> {
        vec![vec!["1".into(), "a".into()], vec!["2".into(), "b".into()]]
    }

    #[test]
    fn edit_display_and_revert_to_original() {
        let b = base();
        let mut p = PendingEdits::default();
        p.set(&b, RowRef::Base(0), 1, "z".into());
        assert_eq!(p.value(&b, RowRef::Base(0), 1), "z");
        assert!(p.is_edited(RowRef::Base(0), 1));
        assert_eq!(p.count(), 1);
        p.set(&b, RowRef::Base(0), 1, "a".into());
        assert!(p.is_empty());
    }

    #[test]
    fn new_rows_and_deletes() {
        let b = base();
        let mut p = PendingEdits::default();
        let r = p.add_row(2);
        p.set(&b, r, 1, "c".into());
        assert_eq!(p.value(&b, r, 1), "c");
        p.toggle_delete(RowRef::Base(1));
        assert!(p.is_deleted(RowRef::Base(1)));
        assert_eq!(p.count(), 2);
        p.toggle_delete(r); // removing a new row drops it
        assert_eq!(p.new_rows(), 0);
        p.toggle_delete(RowRef::Base(1));
        assert!(p.is_empty());
    }

    #[test]
    fn to_row_changes() {
        let b = base();
        let mut p = PendingEdits::default();
        p.set(&b, RowRef::Base(0), 1, "z".into());
        p.set(&b, RowRef::Base(1), 1, "y".into());
        p.toggle_delete(RowRef::Base(1)); // deleted wins over edited
        let r = p.add_row(2);
        p.set(&b, r, 1, "c".into());
        let c = p.to_row_changes(&b);
        assert_eq!(c.updates, vec![(b[0].clone(), vec!["1".to_string(), "z".to_string()])]);
        assert_eq!(c.deletes, vec![b[1].clone()]);
        assert_eq!(c.inserts, vec![vec!["".to_string(), "c".to_string()]]);
    }
}
```

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4: Run** → PASS. **Commit** `feat(gui): pending edits model for the data grid`.

---

### Task 4: App shell — sessions, tabs, layout, status, persistence

**Files:** `src/gui/app.rs`, create `sessions.rs`, `status.rs`, `tabs/mod.rs`, stub `tabs/{console,data,ddl}.rs`, `explorer.rs`, `value_panel.rs`, `dialogs/mod.rs`.

`sessions.rs`:

```rust
pub struct Session {
    pub config: ConnectionConfig,
    pub conn: Conn,
    pub quotes: (char, char),
    pub schemas: Vec<SchemaInfo>,
    /// table (qualified) → details, filled lazily by the explorer.
    pub details: HashMap<String, Result<TableDetails, String>>,
}

#[derive(Default)]
pub struct Sessions {
    pub open: BTreeMap<String, Session>,   // by connection name
    pub connecting: BTreeSet<String>,
}

impl Sessions {
    pub fn get(&self, name: &str) -> Option<&Session>;
    pub fn conn(&self, name: &str) -> Option<Conn>;
    /// All `(connection, schema, table)` triples, for Ctrl+N and completion.
    pub fn all_tables(&self) -> Vec<(String, String, String)>;
    /// Table names of one connection (completion).
    pub fn tables_of(&self, name: &str) -> Vec<String>;
}
```

(test `all_tables`/`tables_of` with hand-built sessions? `Session` needs a `Conn`; build one with `sqlite_mem`-style in-memory pool inside a tokio runtime in the test — or make `all_tables` a free function over `(&str, &[SchemaInfo])` pairs and test that. Prefer the free function.)

`tabs/mod.rs`:

```rust
pub type TabId = super::worker::TabId;

pub enum TabKind {
    Console(console::ConsoleTab),
    Data(data::DataTab),
    Ddl(ddl::DdlTab),
}

pub struct Tab {
    pub id: TabId,
    pub title: String,
    /// Connection name this tab is bound to.
    pub connection: String,
    pub kind: TabKind,
}

pub struct Tabs {
    pub list: Vec<Tab>,
    pub active: usize,
    next_id: TabId,
}

impl Tabs {
    pub fn add(&mut self, connection: String, title: String, kind: TabKind) -> TabId; // activates it
    pub fn close(&mut self, index: usize);
    pub fn find(&mut self, id: TabId) -> Option<&mut Tab>;
    pub fn active_mut(&mut self) -> Option<&mut Tab>;
    /// Data tab already open for (connection, table)? → its index.
    pub fn find_data(&self, connection: &str, table: &str) -> Option<usize>;
}
```

Unit-test `Tabs` (add/close/active index stays valid, `find_data`).

`app.rs` `App` fields: `config`, `theme`, `exit_action`, `worker`, `updater`, `sessions`, `tabs`, `history: History` (plan 3a), `explorer_filter: String`, `show_value_panel: bool`, `status: Status`, `progress`, `dialog: Option<Dialog>`, `table_search: Option<TableSearch>`, `history_popup: Option<HistoryPopup>`.

Frame layout (order matters in egui): update banner (top), status bar (bottom), explorer `SidePanel::left` (resizable, 280 px), value panel `SidePanel::right` (resizable, 320 px, only when `show_value_panel`), `CentralPanel` with the tab bar and the active tab body, then dialogs/popups. Tab bar: each tab shows a coloured dot (connection colour, `theme::rgb`, grey if none), an icon per kind (console `TERMINAL_WINDOW`, data `TABLE`, DDL `CODE`), the title, a close `×`; middle-click closes; `+` opens a console on the active tab's connection (or the first open session).

Event routing in `App::handle_event`: `Connected` → insert `Session` (quotes from `quote_chars`), save `last_connection`; if no tab is bound to it, open a console bound to it (restoring persisted consoles first, see below). `Script/Page/Count/Submitted/Ddl` → `self.tabs.find(tab)` and forward to that tab's `on_event` (tabs closed meanwhile → drop the event). Others → dialogs/explorer/status.

Persistence: consoles are saved in `QueryTabsState` (shared with the TUI): on close and on `Ctrl+S`, write one `QueryTab { name: title, query, cursor_position, connection: Some(conn) }` per console tab. On startup, keep them in `pending_consoles`; when a connection opens, restore the consoles bound to it. Consoles with `connection: None` (written by the TUI) are restored on the first connection opened.

Status bar and update banner: reuse the superseded plan's `status.rs` (Task 3 Step 3), changing the left part to "● N connexion(s)" (dots in each session colour) and showing the active tab's last-execution summary.

Global shortcuts (consumed only when no dialog/popup is open): `Ctrl+T`, `Ctrl+W`, `Ctrl+S`, `Ctrl+N`, `Ctrl+Alt+E`, `Ctrl+R` (refresh active tab's data, else explorer of active connection). Tab-local shortcuts (`Ctrl+Enter`, `F5`, `Escape`, `Ctrl+Alt+L`, grid keys) are handled by the tab bodies.

On close (`viewport().close_requested()`): save consoles, history, config (theme), `updater.schedule_on_quit()`, hand the exit action to `gui::run`.

- [ ] Tests for the pure parts (tabs, all_tables), implement, manual check: app starts with explorer, empty centre "Ouvrez une connexion dans l'explorateur", status bar, theme toggle persists. **Commit** `feat(gui): app shell with sessions, tabs and status bar`.

---

### Task 5: Connection dialog and database explorer

**Files:** `dialogs/connection.rs` (reuse superseded Task 4 Step 3 + tests), `dialogs/mod.rs`, `explorer.rs`.

Connection form additions: a colour row with the 7 `CONNECTION_COLORS` swatches plus "aucune" (selected swatch outlined); `validate()` copies it into `ConnectionConfig.color`. Add a test that the colour round-trips through `validate()`.

Explorer (`explorer.rs`), DataGrip-like tree with `egui::CollapsingHeader` (or `egui::collapsing_header::CollapsingState` for lazy loading):
- Header row: "Explorateur", `+` (new connection), refresh.
- Filter field: filters table names (case-insensitive) across open sessions; when non-empty, matching nodes are force-opened.
- For each configured connection: colour dot + name + db type (weak). Not connected → collapsed; **opening it connects** (`worker.connect`), spinner while `sessions.connecting` contains it, error text (red, wrapped) if the last attempt failed.
- Connected → schemas → tables. Opening a table node requests `worker.table_details` once (cache in `Session.details`), then shows sub-nodes "Colonnes (n)" (name, type weak, key icon for PK, `NOT NULL` weak), "Clés" (PK and FKs `→ ref_table(ref_cols)`), "Index (n)" (name, columns, `UNIQUE`).
- Double-click a table → open/activate its data tab (`tabs.find_data`, else `DataTab::new` from Task 8 — until then, a stub that shows the title).
- Context menus:
  - connection: Se connecter / Déconnecter (drop session, `worker.forget`, close nothing — tabs of a disconnected connection show "Déconnecté — reconnecter" with a button), Nouvelle console, Rafraîchir, Modifier, Supprimer (confirm dialog; disallowed while connected → disconnect first), Export / Import / Vidage par lot.
  - table: Ouvrir les données, Nouvelle console (`SELECT * FROM <table>` prefilled), DDL, Structure…, Importer un CSV…, Vider la table…, Copier le nom.

- [ ] Tests: form colour; a pure `filter_tables(schemas, filter)` (reuse superseded Task 5 test). Manual: two SQLite files configured with different colours, both open at once, tree shows columns/keys/indexes, filter works. **Commit** `feat(gui): connection dialog with colours and database explorer`.

---

### Task 6: Console tab

**Files:** `tabs/console.rs`, `editor/mod.rs`, `editor/completion.rs`, `history_popup.rs`.

`editor/mod.rs`: reuse superseded Task 6 (`char_to_byte`, `byte_to_char`, `highlight_job`, `set_cursor` + tests), but make the editor a function taking `(&mut String, id: egui::Id, known_columns, completion state, tables)` and returning `EditorOutput { cursor: usize, selection: Option<(usize, usize)> /* bytes */, changed: bool }` so each console has its own editor id (`egui::Id::new(("console", tab_id))`). Completion (`editor/completion.rs`, reuse superseded Task 7 + tests) gets tables from `sessions.tables_of(connection)` and columns from the console's last results.

`ConsoleTab`:

```rust
pub struct ResultTab {
    pub title: String,          // "Résultat 1", or the table name when single-table
    pub sql: String,
    pub result: QueryResult,
    pub pinned: bool,
    pub grid: grid::GridState,  // Task 7; until then a placeholder struct
}

pub struct LogLine { pub at: chrono::DateTime<chrono::Local>, pub sql: String, pub message: String, pub ok: bool }

pub struct ConsoleTab {
    pub query: String,
    pub cursor: usize,
    pub results: Vec<ResultTab>,
    pub active_result: usize,   // == results.len() means the "Sortie" log tab
    pub log: Vec<LogLine>,
    pub running_since: Option<Instant>,
    pub editor_height: f32,     // splitter between editor and results
}
```

Behaviour:
- Toolbar: ▶ Exécuter (Ctrl+Entrée), ⏩ Tout (F5), ■ Annuler (only while running), Formater (Ctrl+Alt+L), Historique (Ctrl+Alt+E), connection selector (combo of open sessions; changing it rebinds the console).
- `Ctrl+Entrée`: selection non-empty → `worker.run_script(selection)`; else `worker.run_at_cursor(text, cursor)`. `F5` → `run_script(all)`. `max_rows = Some(1000)`. `Échap` while running → `worker.cancel(tab)` + log "Annulé".
- On `Event::Script`: drop unpinned result tabs; for each outcome append a `LogLine` ("N ligne(s) en X ms", "N ligne(s) affectée(s) en X ms", or the error); every `Ok` result with columns becomes a new `ResultTab` (title from `extract_table_from_query` else "Résultat k"; `truncated` → title suffix "1000+"). Activate the first new result tab, or "Sortie" if there is none or an error occurred. Push each outcome to `History` (`ok`, duration).
- `Ctrl+Alt+L` → `format_sql` on the selection if any, else the whole text.
- Result tab strip under the splitter: tabs with pin toggle (📌) and close; "Sortie" always last, showing the log (newest last, errors red, click a line → puts its SQL in the history popup's search). The result body is the grid from Task 7 (editable when the result comes from exactly one table that has a primary key: `table.is_some() && columns.iter().any(|c| c.is_primary_key)`).
- History popup (`history_popup.rs`): modal list with a search field (`History::search`), newest first, showing connection and relative time; `Entrée`/double-click inserts the SQL at the cursor of the active console; `Échap` closes.

- [ ] Tests: a pure `fn result_title(sql, index, truncated) -> String`, a pure `fn apply_outcomes(&mut ConsoleTab, outcomes)` (build `ConsoleTab` with two pinned/unpinned results, apply outcomes, check pinned kept, log lines, active tab). Manual: multi-statement script yields several result tabs + log; pin survives re-run; cancel a recursive CTE; format; history insert. **Commit** `feat(gui): console with result tabs, output log, formatting and history`.

---

### Task 7: Data grid widget

**Files:** `grid/mod.rs`.

```rust
pub struct GridState {
    pub selected: Option<(RowRef, usize)>,  // focused cell
    pub editing: Option<(RowRef, usize, String)>,
    pub edits: changes::PendingEdits,
    /// Client-side view: indices of base rows after filter/sort (consoles only).
    pub view: Vec<usize>,
    pub filter: String,
    pub sort: Option<(usize, bool)>,        // client-side sort (consoles)
    view_dirty: bool,
}

pub enum GridAction {
    None,
    /// Header clicked on a server-sorted grid (data tab): column index.
    SortBy(usize),
    Submit,
    Revert,
    SelectionChanged,
}

pub struct GridOptions<'a> {
    pub result: &'a QueryResult,
    pub editable: bool,
    /// true → header clicks emit SortBy (server side); false → sort locally.
    pub server_sort: bool,
    pub sort_indicator: Option<(usize, bool)>,
}

pub fn show(ui: &mut egui::Ui, id: egui::Id, state: &mut GridState, opts: GridOptions) -> GridAction;
```

Rendering/behaviour:
- `ScrollArea::horizontal` + `TableBuilder` (`striped`, `resizable`, `Column::initial(140).at_least(40).clip(true)`, plus a narrow leading row-number column), `body.rows(20.0, n, …)` so only visible rows are laid out. Rows = `view` (base indices) followed by new rows.
- Header: column name (key icon for PK, type on hover), sort arrow; click → `SortBy` or local sort (`sort` cycles asc/desc/none; recompute `view` with a stable sort on string values, numbers compared numerically when both parse as f64).
- Local filter field above the grid (consoles only): substring match on any cell → recompute `view` (reuse superseded `matching_rows` + test).
- Cell click selects (focused cell outline in accent); arrow keys move the focus when the grid has focus.
- Display: `NULL` weak italic; edited cell background amber (`Color32::from_rgb(0xF2, 0xA6, 0x4C)` at 25% alpha); new row background green 15%; deleted row text red strikethrough.
- Inline edit (editable only): double-click or `F2` → `editing = Some(..)` with a `TextEdit::singleline` inside the cell, focused; `Entrée` commits into `edits.set`, `Échap` cancels, focus loss commits. `Tab` commits and moves to the next cell.
- Keys (editable): `Alt+Insert` → `edits.add_row(ncols)` and start editing its first non-PK cell; `Ctrl+Suppr` → `toggle_delete` on the focused row; `Ctrl+Entrée` → `Submit`; `Ctrl+Z` while not editing → `Revert` (asks nothing; DataGrip-like "revert selected" is out of scope).
- Context menu on a cell: Copier la valeur, Copier la ligne (TSV, reuse superseded `to_tsv` + test), and if editable: Modifier (F2), Mettre à NULL, Ajouter une ligne, Supprimer la ligne / Annuler la suppression.
- Footer row: "N lignes" (+ " · 1000+ (limité)" when truncated), and when `edits` non-empty: "M modification(s)" + buttons ✓ Submit, ↺ Revert.
- `selected` change → `SelectionChanged` (the value panel reads it).

- [ ] Tests: `matching_rows`, `to_tsv`, a pure `sorted_view(rows, col, asc) -> Vec<usize>` (numeric vs string ordering, stability). Manual: 200k-row console result scrolls smoothly (generate with a recursive CTE in SQLite, with `max_rows` temporarily raised), edit/add/delete marks render correctly. **Commit** `feat(gui): virtualised data grid with inline editing`.

---

### Task 8: Data editor tab

**Files:** `tabs/data.rs`.

```rust
pub struct DataTab {
    pub table: String,              // qualified + quoted
    pub query: DataQuery,           // plan 3a paging (page_size 500)
    pub filter_input: String,       // WHERE editor (applied on Entrée)
    pub order_input: String,        // ORDER BY editor
    pub result: Option<QueryResult>,
    pub total: Option<u64>,
    pub grid: GridState,
    pub loading: bool,
    pub error: Option<String>,
    pub system_columns: Vec<usize>,
}
```

- Opening a data tab → `worker.load_page(build_select)` and `worker.count(build_count)`.
- Toolbar: ⟳ Rafraîchir (Ctrl+R), page navigation `|< < page p / P > >|` (P from `total`, "?" while counting), "500 lignes/page", pending count + ✓ Submit / ↺ Revert, DDL button.
- Filter bar: `WHERE [ … ]  ORDER BY [ … ]`, single-line `TextEdit`s with SQL highlighting; `Entrée` applies (reset page 0, reload + recount); errors from the database show in a red band above the grid (the previous page stays visible).
- Header click (`GridAction::SortBy(i)`) → `query.order_by = toggle_order(&query.order_by, quoted column)`, mirror into `order_input`, reload.
- Leaving a page / refreshing / changing filter with pending edits → confirm dialog "Abandonner les modifications ?" (Submit / Abandonner / Annuler).
- Submit → `worker.submit(tab, conn, db_type, table, columns, system_columns, edits.to_row_changes(rows))`; on `Submitted(Ok(n))` → status "n modification(s) appliquée(s)", clear edits, reload page + count; on error → keep edits, red band with the error (transaction rolled back).
- `system_columns = detect_system_columns(columns)` when the first page arrives (used for inserts).
- Console results flagged editable (Task 6) use the same Submit flow with `table = extract_table_from_query(sql)`; after success the console re-runs that statement to refresh its result tab.

- [ ] Tests: a pure `fn page_count(total: u64, page_size: usize) -> usize` and `fn can_leave(edits) -> bool`. Manual: browse a 10k-row table page by page, sort by header, filter `id > 100`, edit/add/delete then Submit → rows persisted; failing Submit (NOT NULL) keeps edits and shows the error; Revert. **Commit** `feat(gui): table data editor with paging, sort, filter and submit`.

---

### Task 9: Value panel, DDL tab, table search

**Files:** `value_panel.rs`, `tabs/ddl.rs`, `table_search.rs`.

- Value panel (toggle button in the tab toolbar and `Ctrl+Shift+V`?—no extra shortcut; a toggle in the status bar is enough): shows column name + type and the focused cell's full value in a scrollable read-only (or editable when the grid is editable) multiline `TextEdit`. If the value parses as JSON (`serde_json::from_str::<serde_json::Value>` and starts with `{` or `[`), show it pretty-printed with a "JSON" badge; editing JSON writes back the compact form on "Appliquer". Edits go through `edits.set` of the owning grid. Pure helper `fn pretty_json(s) -> Option<String>` with tests.
- DDL tab: opened from explorer/data tab; `worker.ddl(…)`; read-only highlighted editor (reuse `highlight_job`), "Copier" button; refresh.
- `Ctrl+N` table search: modal popup with a text field, fuzzy match (case-insensitive subsequence, ranked: prefix > contains > subsequence; pure fn `rank(query, name) -> Option<u32>` + tests) over `sessions.all_tables()`, rows show colour dot, `schema.table`, connection weak; ↑/↓, `Entrée` opens the data tab, `Échap` closes. Max 50 shown.

- [ ] Tests for `pretty_json` and `rank`. Manual checks. **Commit** `feat(gui): value panel, DDL tab and table search`.

---

### Task 10: Structure and transfer dialogs

Reuse the superseded plan's Task 10 (structure, with `ColumnEdit::to_modification` test) and Task 11 (export result / import CSV / batch export-import-truncate, with `BatchDialog` and `format_for` tests), changing every use of "the" connection to an explicit connection name carried by the dialog (`StructureDialog { connection, table, … }`, `BatchDialog { connection, … }`), taking `Conn`/quotes/db_type from `sessions.get(&connection)`. Export of a result is offered from the grid footer ("Exporter…") for console results and data pages. After a successful schema change, invalidate `Session.details[table]` and refresh open data tabs of that table. After batch import/truncate, reload open data tabs of that connection.

- [ ] Tests from the superseded tasks. Manual: structure add/rename/drop on SQLite, export CSV/SQL, import CSV with progress, batch operations on a connection that isn't the active tab's. **Commit** `feat(gui): structure and import/export dialogs`.

---

### Task 11: Polish, performance check, README

- [ ] `cargo clippy --all-targets` clean for `src/gui`.
- [ ] Performance checks (record numbers in the commit message): release build cold start to first frame < 1 s; idle CPU ~0 % (no continuous repaint when idle); RAM with two connections and a 1 000-row result < 120 MB; scrolling a 200k-row local result stays smooth.
- [ ] Full manual checklist against the spec "GUI" section (each bullet), on SQLite, plus Postgres/MySQL if available.
- [ ] `storingUnicorns tui` still works.
- [ ] README: replace the GUI section written for the superseded plan with the DataGrip-like features and the shortcut table from the spec; keep TUI keybindings under "Raccourcis (TUI)".
- [ ] **Commit** `docs: GUI usage and shortcuts`.
