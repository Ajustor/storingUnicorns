//! SQL console tab: editor, one result tab per row-returning statement, and
//! the "Sortie" log of every execution. The results and log and how outcomes
//! update them are in `outcomes`; the result strip is drawn by `results`.

use std::collections::VecDeque;
use std::time::Instant;

use chrono::Local;
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};
use egui_phosphor::regular as icon;

use crate::engine::models::{Column, DatabaseType, QueryResult, SchemaInfo};
use crate::engine::nosql::{self, NoSqlScope};
use crate::engine::ops::rows::RowChanges;
use crate::engine::sql::format::format_sql;
use crate::gui::editor::completion::{self, Completion, CompletionSource};
use crate::gui::editor::{self, EditorContext};
use crate::gui::shortcuts;
use crate::gui::theme::{self, ACCENT, ERROR};
use crate::gui::tx::{self, TxAction, TxView};

use super::TabId;

mod outcomes;
mod results;

pub use outcomes::{
    apply_nosql_outcomes, apply_nosql_refresh, apply_outcomes, apply_refresh, apply_submitted,
    read_only_hint, result_connection, LogLine, ResultTab, MAX_LOG_LINES,
};

/// Row cap of console results.
pub const MAX_ROWS: usize = 1000;

pub const RUN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
pub const RUN_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);
/// « Tout exécuter » without F5, which needs fn on Mac keyboards.
pub const RUN_ALL_ENTER: KeyboardShortcut =
    KeyboardShortcut::new(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Enter);

/// The « Tout exécuter » keys, the one shown on `mac` first.
pub fn run_all_keys(mac: bool) -> [KeyboardShortcut; 2] {
    if mac {
        [RUN_ALL_ENTER, RUN_ALL]
    } else {
        [RUN_ALL, RUN_ALL_ENTER]
    }
}

/// Consume this frame's run keys: (run the statement, run everything).
/// ⌘⇧↩ also matches ⌘↩ (egui ignores an extra Shift), so it is consumed
/// first. The Enter ones belong to the editor (`editor_keys`), F5 to the
/// whole console.
fn run_keys(input: &mut egui::InputState, editor_keys: bool) -> (bool, bool) {
    let all_enter = editor_keys && input.consume_shortcut(&RUN_ALL_ENTER);
    let run = editor_keys && input.consume_shortcut(&RUN);
    let f5 = input.consume_shortcut(&RUN_ALL);
    (run, all_enter || f5)
}

pub const FORMAT: KeyboardShortcut = KeyboardShortcut::new(
    Modifiers {
        alt: true,
        ctrl: false,
        shift: false,
        mac_cmd: false,
        command: true,
    },
    Key::L,
);
pub const HISTORY: KeyboardShortcut = KeyboardShortcut::new(
    Modifiers {
        alt: true,
        ctrl: false,
        shift: false,
        mac_cmd: false,
        command: true,
    },
    Key::E,
);

pub struct ConsoleTab {
    pub query: String,
    /// Cursor position (bytes), as persisted in `QueryTab::cursor_position`.
    pub cursor: usize,
    /// Selected byte range of the editor, when not empty.
    pub selection: Option<(usize, usize)>,
    pub results: Vec<ResultTab>,
    /// `== results.len()` means the "Sortie" log tab.
    pub active_result: usize,
    /// The "Sortie" log, oldest first, at most `MAX_LOG_LINES`.
    pub log: VecDeque<LogLine>,
    pub running_since: Option<Instant>,
    /// Splitter between the editor and the results.
    pub editor_height: f32,
    /// Columns of the result tabs (completion and highlighting).
    pub known_columns: Vec<String>,
    /// Boxed: it keeps the console variant of `TabKind` small.
    pub completion: Box<Completion>,
    /// Move the editor cursor to `cursor` on the next frame (restored console).
    restore_cursor: bool,
    next_result_id: u64,
    /// Result tab whose Submit is running.
    pub submitting: Option<u64>,
    /// Result tab whose SQL is re-run after a Submit (the running script).
    pub refreshing: Option<u64>,
    /// The text changed since the app last looked (it saves consoles a
    /// moment after their last edit).
    pub edited: bool,
    /// MongoDB / Redis console: its current database (`use`, `SELECT`, the
    /// « Base » selector); `None` for an SQL console.
    pub scope: Option<NoSqlScope>,
}

/// How to run the console's SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunKind {
    /// Every statement of the text (F5, or the selection).
    Script(String),
    /// The statement or transaction block under the cursor (bytes).
    AtCursor { text: String, cursor: usize },
}

/// Requests the console cannot fulfil itself (they need the worker or the app).
pub enum ConsoleAction {
    Run(RunKind),
    Cancel,
    /// Bind the console to another open connection.
    Rebind(String),
    /// Open the history popup with this search.
    History(String),
    /// Export a result's rows; `table` is the INSERT target, `connection`
    /// the one the rows come from (identifier quotes).
    Export {
        result: QueryResult,
        table: String,
        connection: String,
    },
    /// Transaction mode, Commit or Rollback of the console's connection.
    Tx(TxAction),
    /// Apply the pending edits of result tab `result_index`.
    Submit {
        result_index: usize,
        table: String,
        columns: Vec<Column>,
        changes: RowChanges,
    },
    /// Apply the pending edits of NoSQL result tab `result_index` (its
    /// `Source` says to which documents).
    NoSqlSubmit {
        result_index: usize,
        changes: RowChanges,
    },
    /// Open a Redis key read by a console in its « Clés » tab (TTL,
    /// rename, value pages).
    OpenKey {
        connection: String,
        db: u8,
        key: Vec<u8>,
    },
}

/// What a MongoDB / Redis console needs besides the SQL console's context.
#[derive(Clone, Copy)]
pub struct ConsoleNoSql<'a> {
    pub db: &'a DatabaseType,
    /// Databases → collections (MongoDB), `db0`… (Redis).
    pub schemas: &'a [SchemaInfo],
    /// Redis: the keys listed by the explorer (completion).
    pub keys: &'a [String],
}

/// The databases the « Base » selector offers: MongoDB's by name, Redis'
/// by index (`db3` → "3").
pub fn scope_choices(db: &DatabaseType, schemas: &[SchemaInfo]) -> Vec<String> {
    schemas
        .iter()
        .map(|s| match db {
            DatabaseType::Redis => s.name.strip_prefix("db").unwrap_or(&s.name).to_string(),
            _ => s.name.clone(),
        })
        .collect()
}

/// What the console needs from the app to draw itself.
pub struct ConsoleContext<'a> {
    pub tab: TabId,
    pub connection: &'a str,
    /// Open connections and their colour (selector).
    pub connections: &'a [(String, Color32)],
    /// Colour of any configured connection (result badges).
    pub color_of: &'a dyn Fn(&str) -> Color32,
    pub connected: bool,
    pub running: bool,
    /// False while a modal is open: no shortcut fires.
    pub shortcuts: bool,
    /// Tables and columns of the connection (completion).
    pub source: &'a dyn CompletionSource,
    /// Transaction mode of the connection.
    pub tx: TxView,
    /// The connection is MongoDB / Redis: NoSQL console (no transaction,
    /// « Base » selector, engine completion and formatting).
    pub nosql: Option<ConsoleNoSql<'a>>,
}

/// Whether Escape cancels the running query: only from the editor or with
/// nothing focused (a filter or WHERE field uses Escape to leave itself),
/// and not while the completion popup or a cell editor would take it.
pub fn escape_cancels(
    focused: Option<egui::Id>,
    editor: egui::Id,
    completion_open: bool,
    grid_editing: bool,
) -> bool {
    focused.is_none_or(|f| f == editor) && !completion_open && !grid_editing
}

impl ConsoleTab {
    /// Append to the "Sortie" log, dropping the oldest lines past
    /// `MAX_LOG_LINES`.
    pub fn push_log(&mut self, line: LogLine) {
        self.log.push_back(line);
        while self.log.len() > MAX_LOG_LINES {
            self.log.pop_front();
        }
    }

    pub fn new(query: String, cursor: usize) -> Self {
        let cursor = cursor.min(query.len());
        Self {
            query,
            cursor,
            selection: None,
            results: Vec::new(),
            active_result: 0,
            log: VecDeque::new(),
            running_since: None,
            editor_height: 220.0,
            known_columns: Vec::new(),
            completion: Box::default(),
            restore_cursor: true,
            next_result_id: 1,
            submitting: None,
            refreshing: None,
            edited: false,
            scope: None,
        }
    }

    fn next_result_id(&mut self) -> u64 {
        let id = self.next_result_id;
        self.next_result_id += 1;
        id
    }

    /// Why result tab `index` is read-only for now: its edits are being
    /// submitted, or its rows are about to be replaced (re-run after a
    /// submit, or a run that replaces the unpinned results). Edits made
    /// meanwhile would be lost. `running`: the console's script run is pending.
    pub fn result_lock(&self, index: usize, running: bool) -> Option<&'static str> {
        let r = self.results.get(index)?;
        if self.submitting == Some(r.id) {
            Some("Envoi en cours…")
        } else if self.refreshing == Some(r.id)
            || (running && self.refreshing.is_none() && !r.pinned)
        {
            Some("Chargement…")
        } else {
            None
        }
    }

    /// Write the text of the result tabs' open cell editors into their edits.
    pub fn commit_editors(&mut self) {
        for r in &mut self.results {
            r.grid.commit_editing(&r.result.rows);
        }
    }

    /// Whether a new run would drop pending edits: it replaces the unpinned
    /// result tabs (open cell editors committed first).
    pub fn rerun_loses_edits(&mut self) -> bool {
        self.commit_editors();
        self.results
            .iter()
            .any(|r| !r.pinned && !r.grid.edits.is_empty())
    }

    /// A cell of the active result is being edited (Escape cancels it first).
    fn grid_editing(&self) -> bool {
        self.results
            .get(self.active_result)
            .is_some_and(|r| r.grid.editing.is_some())
    }

    pub fn editor_id(tab: TabId) -> egui::Id {
        egui::Id::new(("console", tab))
    }

    fn refresh_known_columns(&mut self) {
        let mut cols: Vec<String> = self
            .results
            .iter()
            .flat_map(|r| r.result.columns.iter().map(|c| c.name.clone()))
            .collect();
        cols.sort();
        cols.dedup();
        self.known_columns = cols;
    }

    /// A NoSQL run left the console in `scope` (`use`, `SELECT`): the
    /// « Base » selector follows. Whether it changed (the console is saved).
    pub fn apply_scope(&mut self, scope: NoSqlScope) -> bool {
        if self.scope.as_ref() == Some(&scope) {
            return false;
        }
        self.scope = Some(scope);
        self.edited = true;
        true
    }

    /// Log a cancelled run.
    pub fn cancelled(&mut self) {
        self.running_since = None;
        self.push_log(LogLine::new(
            Local::now(),
            String::new(),
            "Annulé".into(),
            false,
        ));
        self.active_result = self.results.len();
    }

    /// Insert `sql` at the cursor (replacing the selection) and focus the editor.
    pub fn insert(&mut self, ctx: &egui::Context, tab: TabId, sql: &str) {
        let (start, end) = self.selection.unwrap_or((self.cursor, self.cursor));
        let (start, end) = (start.min(self.query.len()), end.min(self.query.len()));
        self.query.replace_range(start..end, sql);
        self.cursor = start + sql.len();
        self.selection = None;
        self.edited = true;
        let id = Self::editor_id(tab);
        editor::set_cursor(ctx, id, &self.query, self.cursor);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    /// Ctrl+Alt+L: format the selection, else the whole text (MongoDB:
    /// the engine's shell formatter; nothing for Redis or a text that does
    /// not parse).
    fn format(&mut self, ctx: &egui::Context, tab: TabId, db: Option<&DatabaseType>) {
        let (start, end) = self.selection.unwrap_or((0, self.query.len()));
        let (start, end) = (start.min(self.query.len()), end.min(self.query.len()));
        let formatted = match db {
            Some(db) => nosql::format(db, &self.query[start..end]).unwrap_or_default(),
            None => format_sql(&self.query[start..end]),
        };
        if formatted.is_empty() {
            return;
        }
        self.query.replace_range(start..end, &formatted);
        self.cursor = start + formatted.len();
        self.selection = None;
        self.edited = true;
        editor::set_cursor(ctx, Self::editor_id(tab), &self.query, self.cursor);
    }

    /// Ctrl+Enter: the selection, else the statement at the cursor.
    fn run_current(&self) -> Option<ConsoleAction> {
        if let Some((start, end)) = self.selection {
            let sql = self.query.get(start..end)?;
            if !sql.trim().is_empty() {
                return Some(ConsoleAction::Run(RunKind::Script(sql.to_string())));
            }
        }
        if self.query.trim().is_empty() {
            return None;
        }
        Some(ConsoleAction::Run(RunKind::AtCursor {
            text: self.query.clone(),
            cursor: self.cursor,
        }))
    }

    fn run_all(&self) -> Option<ConsoleAction> {
        (!self.query.trim().is_empty())
            .then(|| ConsoleAction::Run(RunKind::Script(self.query.clone())))
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: ConsoleContext) -> Option<ConsoleAction> {
        let id = Self::editor_id(cx.tab);
        if std::mem::take(&mut self.restore_cursor) {
            editor::set_cursor(ui.ctx(), id, &self.query, self.cursor);
        }
        let mut action = self.shortcuts(ui, &cx, id);
        if let Some(a) = self.toolbar(ui, &cx) {
            action = Some(a);
        }
        ui.separator();
        if self.query.len() > editor::HIGHLIGHT_MAX_BYTES {
            // Outside the editor's scroll area, so it stays in view.
            ui.label(
                RichText::new("Coloration désactivée (gros script)")
                    .small()
                    .weak(),
            );
        }

        // Editor, resizable through the splitter below it.
        let max_editor = (ui.available_height() - 120.0).max(60.0);
        self.editor_height = self.editor_height.clamp(60.0, max_editor);
        let source = cx.source;
        let scope = self.scope.clone().unwrap_or(NoSqlScope {
            database: String::new(),
        });
        let nosql = cx.nosql.map(|n| completion::NoSqlScope {
            db: n.db,
            scope: &scope,
            schemas: n.schemas,
            keys: n.keys,
            prefix: "",
        });
        let out = egui::ScrollArea::vertical()
            .id_salt(id.with("scroll"))
            .max_height(self.editor_height)
            .min_scrolled_height(self.editor_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                editor::show(
                    ui,
                    &mut self.query,
                    id,
                    self.editor_height,
                    &mut self.completion,
                    EditorContext {
                        known_columns: &self.known_columns,
                        source,
                        nosql,
                    },
                )
            })
            .inner;
        self.cursor = out.cursor;
        self.selection = out.selection;
        self.edited |= out.changed;
        self.splitter(ui);

        if let Some(a) = self.results_area(ui, id, &cx) {
            action = Some(a);
        }
        action
    }

    /// Console shortcuts, consumed before the editor sees them.
    fn shortcuts(
        &mut self,
        ui: &mut egui::Ui,
        cx: &ConsoleContext,
        id: egui::Id,
    ) -> Option<ConsoleAction> {
        if !cx.shortcuts {
            return None;
        }
        let ctx = ui.ctx().clone();
        // Ctrl+Enter / Ctrl+Alt+L belong to the editor: only when it has the
        // focus, or nothing has (they must not steal keys from other fields).
        let editor_keys = ctx.memory(|m| m.focused().is_none_or(|f| f == id));
        let pressed = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
        // AltGr+key off macOS: a character, not a shortcut; on macOS the
        // character Option made alongside ⌥⌘ is dropped.
        let mac = shortcuts::is_mac();
        let cmd_alt =
            |s: &KeyboardShortcut| ctx.input_mut(|i| shortcuts::consume_cmd_alt(i, s, mac));
        let mut action = None;
        if cmd_alt(&HISTORY) {
            action = Some(ConsoleAction::History(String::new()));
        }
        if editor_keys && cmd_alt(&FORMAT) {
            self.format(&ctx, cx.tab, cx.nosql.map(|n| n.db));
        }
        // Consumed while running too: a run at a time per console.
        let (run, run_all) = ctx.input_mut(|i| run_keys(i, editor_keys));
        if cx.running {
            let focused = ctx.memory(|m| m.focused());
            if escape_cancels(focused, id, self.completion.open, self.grid_editing())
                && pressed(&KeyboardShortcut::new(Modifiers::NONE, Key::Escape))
            {
                action = Some(ConsoleAction::Cancel);
            }
            return action;
        }
        if run && cx.connected {
            action = self.run_current().or(action);
        }
        if run_all && cx.connected {
            action = self.run_all().or(action);
        }
        action
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &ConsoleContext) -> Option<ConsoleAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            let can_run = cx.connected && !cx.running;
            if ui
                .add_enabled(
                    can_run,
                    egui::Button::new(format!("{} Exécuter", icon::PLAY)),
                )
                .on_hover_text(format!(
                    "Sélection ou instruction au curseur ({})",
                    shortcuts::label_here(&RUN)
                ))
                .clicked()
            {
                action = self.run_current();
            }
            if ui
                .add_enabled(
                    can_run,
                    egui::Button::new(format!("{} Tout", icon::FAST_FORWARD)),
                )
                .on_hover_text(format!(
                    "Tout le script ({})",
                    shortcuts::hint_here(&run_all_keys(shortcuts::is_mac()))
                ))
                .clicked()
            {
                action = self.run_all();
            }
            if cx.running {
                if ui
                    .button(RichText::new(format!("{} Annuler", icon::STOP)).color(ERROR))
                    .on_hover_text("Annuler l'exécution (Échap)")
                    .clicked()
                {
                    action = Some(ConsoleAction::Cancel);
                }
                ui.spinner();
                if let Some(since) = self.running_since {
                    ui.label(
                        RichText::new(format!("{:.1} s", since.elapsed().as_secs_f32())).weak(),
                    );
                }
            }
            ui.separator();
            // Redis commands have no formatter.
            let can_format = cx.nosql.is_none_or(|n| *n.db == DatabaseType::MongoDB);
            if can_format
                && ui
                    .button(format!("{} Formater", icon::MAGIC_WAND))
                    .on_hover_text(format!(
                        "Formater la sélection ou tout ({})",
                        shortcuts::label_here(&FORMAT)
                    ))
                    .clicked()
            {
                self.format(ui.ctx(), cx.tab, cx.nosql.map(|n| n.db));
            }
            if ui
                .button(format!("{} Historique", icon::CLOCK_COUNTER_CLOCKWISE))
                .on_hover_text(format!(
                    "Requêtes exécutées ({})",
                    shortcuts::label_here(&HISTORY)
                ))
                .clicked()
            {
                action = Some(ConsoleAction::History(String::new()));
            }
            ui.separator();
            if let Some(name) = connection_selector(ui, cx) {
                action = Some(ConsoleAction::Rebind(name));
            }
            match cx.nosql {
                // No manual transaction mode for MongoDB / Redis.
                Some(n) => {
                    if scope_selector(ui, cx.tab, n, &mut self.scope) {
                        self.edited = true;
                    }
                }
                None => {
                    if let Some(a) = tx::controls(ui, cx.tab, cx.tx) {
                        action = Some(ConsoleAction::Tx(a));
                    }
                }
            }
        });
        action
    }

    /// Drag handle resizing the editor.
    fn splitter(&mut self, ui: &mut egui::Ui) {
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::drag());
        let response = response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
        if response.dragged() {
            self.editor_height += response.drag_delta().y;
        }
        let stroke = if response.hovered() || response.dragged() {
            egui::Stroke::new(2.0, ACCENT)
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        ui.painter().hline(rect.x_range(), rect.center().y, stroke);
    }
}

/// « Base : » of a NoSQL console (its scope, also changed by `use` /
/// `SELECT`); whether the user chose another one.
fn scope_selector(
    ui: &mut egui::Ui,
    tab: TabId,
    n: ConsoleNoSql,
    scope: &mut Option<NoSqlScope>,
) -> bool {
    let current = scope
        .as_ref()
        .map(|s| s.database.clone())
        .unwrap_or_default();
    let mut chosen = None;
    ui.label("Base :");
    let shown = if current.is_empty() {
        "—".to_string()
    } else {
        current.clone()
    };
    let choices = scope_choices(n.db, n.schemas);
    egui::ComboBox::from_id_salt(("console_scope", tab))
        .width(crate::gui::popup::combo_width(ui, choices.iter()).min(crate::gui::popup::COMBO_MAX))
        .selected_text(shown)
        .show_ui(ui, |ui| {
            for name in &choices {
                if ui.selectable_label(*name == current, name).clicked() && *name != current {
                    chosen = Some(name.clone());
                }
            }
        })
        .response
        .on_hover_text("Base courante (aussi changée par use / SELECT)");
    match chosen {
        Some(database) => {
            *scope = Some(NoSqlScope { database });
            true
        }
        None => false,
    }
}

/// Combo of the open connections; returns the newly chosen one.
fn connection_selector(ui: &mut egui::Ui, cx: &ConsoleContext) -> Option<String> {
    let mut chosen = None;
    ui.spacing_mut().item_spacing.x = 4.0;
    let color = cx
        .connections
        .iter()
        .find(|(n, _)| n == cx.connection)
        .map(|(_, c)| *c)
        .unwrap_or(Color32::GRAY);
    theme::dot(ui, color);
    // Items are a dot then the name: room for the dot on top of the name.
    let width = crate::gui::popup::combo_width(ui, cx.connections.iter().map(|(n, _)| n))
        + 10.0
        + ui.spacing().item_spacing.x;
    egui::ComboBox::from_id_salt(("console_connection", cx.tab))
        .width(width.min(crate::gui::popup::COMBO_MAX))
        .selected_text(cx.connection)
        .show_ui(ui, |ui| {
            for (name, color) in cx.connections {
                ui.horizontal(|ui| {
                    theme::dot(ui, *color);
                    if ui.selectable_label(name == cx.connection, name).clicked()
                        && name != cx.connection
                    {
                        chosen = Some(name.clone());
                    }
                });
            }
        })
        .response
        .on_hover_text("Connexion de la console");
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_all_enter_runs_everything_not_the_statement() {
        use crate::gui::shortcuts::test_support::{frame, press};
        for mac in [true, false] {
            let keys =
                |s: &KeyboardShortcut, editor| frame(vec![press(s, mac)], |i| run_keys(i, editor));
            assert_eq!(keys(&RUN_ALL_ENTER, true), (false, true), "mac: {mac}");
            assert_eq!(keys(&RUN, true), (true, false));
            assert_eq!(keys(&RUN_ALL, true), (false, true));
            // Outside the editor only F5 is the console's.
            assert_eq!(keys(&RUN_ALL_ENTER, false), (false, false));
            assert_eq!(keys(&RUN, false), (false, false));
            assert_eq!(keys(&RUN_ALL, false), (false, true));
        }
    }

    #[test]
    fn run_all_shows_the_key_every_keyboard_has() {
        use crate::gui::shortcuts::hint;
        assert_eq!(hint(&run_all_keys(true), true), "⇧⌘↩");
        assert_eq!(hint(&run_all_keys(false), false), "F5 / Ctrl+Maj+Entrée");
        assert_eq!(shortcuts::label(&FORMAT, true), "⌥⌘L");
        assert_eq!(shortcuts::label(&HISTORY, false), "Ctrl+Alt+E");
        assert_eq!(shortcuts::label(&RUN, true), "⌘↩");
    }

    #[test]
    fn escape_cancels_only_from_the_editor_or_nothing() {
        let editor = egui::Id::new("editor");
        let filter = egui::Id::new("filter");
        assert!(escape_cancels(None, editor, false, false));
        assert!(escape_cancels(Some(editor), editor, false, false));
        assert!(
            !escape_cancels(Some(filter), editor, false, false),
            "filter field"
        );
        assert!(
            !escape_cancels(Some(editor), editor, true, false),
            "completion"
        );
        assert!(!escape_cancels(None, editor, false, true), "cell editor");
    }

    #[test]
    fn log_keeps_the_newest_lines() {
        let mut c = ConsoleTab::new(String::new(), 0);
        for i in 0..MAX_LOG_LINES + 5 {
            c.push_log(LogLine::new(
                Local::now(),
                format!("SELECT {i}\nFROM t"),
                String::new(),
                true,
            ));
        }
        assert_eq!(c.log.len(), MAX_LOG_LINES);
        assert_eq!(c.log[0].sql, "SELECT 5\nFROM t");
        assert_eq!(c.log[0].preview, "SELECT 5…");
        assert_eq!(c.log.back().map(|l| l.time.len()), Some(8));
    }

    #[test]
    fn the_base_selector_follows_use_and_select() {
        let mut c = ConsoleTab::new(String::new(), 0);
        let shop = NoSqlScope {
            database: "shop".into(),
        };
        assert!(c.apply_scope(shop.clone()));
        assert!(c.edited, "saved with the console");
        c.edited = false;
        assert!(!c.apply_scope(shop.clone()), "unchanged");
        assert!(!c.edited);
        assert_eq!(c.scope, Some(shop));
    }

    /// Labels of the widgets the console draws with `nosql`.
    fn toolbar_labels(nosql: Option<ConsoleNoSql>) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut c = ConsoleTab::new("x".into(), 0);
        c.scope = nosql.map(|_| NoSqlScope {
            database: "shop".into(),
        });
        let color_of = |_: &str| Color32::GRAY;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 800.0),
            )),
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                c.show(
                    ui,
                    ConsoleContext {
                        tab: 1,
                        connection: "c",
                        connections: &[],
                        color_of: &color_of,
                        connected: true,
                        running: false,
                        shortcuts: false,
                        source: &crate::gui::editor::completion::NoSource,
                        tx: TxView::Auto,
                        nosql,
                    },
                );
            });
        });
        out.platform_output
            .accesskit_update
            .map(|u| {
                u.nodes
                    .iter()
                    .flat_map(|(_, n)| [n.label(), n.value()])
                    .flatten()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn nosql_consoles_have_a_base_selector_and_their_own_formatter() {
        let has = |labels: &[String], text: &str| labels.iter().any(|l| l.contains(text));
        let sql = toolbar_labels(None);
        assert!(has(&sql, "Formater"), "{sql:?}");
        assert!(!has(&sql, "Base :"));
        let schemas = [SchemaInfo {
            name: "shop".into(),
            tables: vec![],
            expanded: false,
        }];
        let nosql = |db| ConsoleNoSql {
            db,
            schemas: &schemas,
            keys: &[],
        };
        let mongo = toolbar_labels(Some(nosql(&DatabaseType::MongoDB)));
        assert!(has(&mongo, "Base :"), "{mongo:?}");
        assert!(has(&mongo, "Formater"), "MongoDB has a formatter");
        let redis = toolbar_labels(Some(nosql(&DatabaseType::Redis)));
        assert!(has(&redis, "Base :"));
        assert!(!has(&redis, "Formater"), "Redis has none");
    }

    #[test]
    fn redis_scopes_are_database_indexes() {
        let schemas = vec![
            SchemaInfo {
                name: "db0".into(),
                tables: vec![],
                expanded: false,
            },
            SchemaInfo {
                name: "db12".into(),
                tables: vec![],
                expanded: false,
            },
        ];
        assert_eq!(scope_choices(&DatabaseType::Redis, &schemas), ["0", "12"]);
        assert_eq!(
            scope_choices(&DatabaseType::MongoDB, &schemas),
            ["db0", "db12"]
        );
    }

    #[test]
    fn run_requests_use_selection_else_cursor() {
        let mut c = ConsoleTab::new("SELECT 1; SELECT 2".into(), 3);
        assert!(matches!(
            c.run_current(),
            Some(ConsoleAction::Run(RunKind::AtCursor { cursor: 3, .. }))
        ));
        c.selection = Some((10, 18));
        match c.run_current() {
            Some(ConsoleAction::Run(RunKind::Script(sql))) => assert_eq!(sql, "SELECT 2"),
            _ => panic!("selection runs as a script"),
        }
        let empty = ConsoleTab::new("  \n".into(), 0);
        assert!(empty.run_current().is_none());
        assert!(empty.run_all().is_none());
    }
}
