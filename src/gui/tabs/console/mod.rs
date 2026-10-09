//! SQL console tab: editor, one result tab per row-returning statement, and
//! the "Sortie" log of every execution. The results and log and how outcomes
//! update them are in `outcomes`; the result strip is drawn by `results`.

use std::collections::VecDeque;
use std::time::Instant;

use chrono::Local;
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};
use egui_phosphor::regular as icon;

use crate::engine::models::{Column, QueryResult};
use crate::engine::ops::rows::RowChanges;
use crate::engine::sql::format::format_sql;
use crate::gui::editor::{self, completion::Completion, EditorContext};
use crate::gui::theme::{self, ACCENT, ERROR};

use super::TabId;

mod outcomes;
mod results;

pub use outcomes::{
    apply_outcomes, apply_refresh, apply_submitted, read_only_hint, result_connection, LogLine,
    ResultTab, MAX_LOG_LINES,
};

/// Row cap of console results.
pub const MAX_ROWS: usize = 1000;

pub const RUN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
pub const RUN_ALL: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F5);
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
    pub completion: Completion,
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
    /// Apply the pending edits of result tab `result_index`.
    Submit {
        result_index: usize,
        table: String,
        columns: Vec<Column>,
        changes: RowChanges,
    },
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
    pub tables: &'a dyn Fn() -> Vec<String>,
}

/// Whether this frame's input carries typed text. On Windows AltGr is
/// reported as Ctrl+Alt, so typing `€` (AltGr+E on AZERTY) also looks like
/// Ctrl+Alt+E: such a key is a character, not a shortcut.
pub fn typed_text(events: &[egui::Event]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, egui::Event::Text(t) if !t.is_empty()))
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
            completion: Completion::default(),
            restore_cursor: true,
            next_result_id: 1,
            submitting: None,
            refreshing: None,
            edited: false,
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

    /// Ctrl+Alt+L: format the selection, else the whole text.
    fn format(&mut self, ctx: &egui::Context, tab: TabId) {
        let (start, end) = self.selection.unwrap_or((0, self.query.len()));
        let (start, end) = (start.min(self.query.len()), end.min(self.query.len()));
        let formatted = format_sql(&self.query[start..end]);
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

        // Editor, resizable through the splitter below it.
        let max_editor = (ui.available_height() - 120.0).max(60.0);
        self.editor_height = self.editor_height.clamp(60.0, max_editor);
        let tables = cx.tables;
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
                        tables,
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
        // AltGr+key: a character for the focused field, not a shortcut.
        let typed = ctx.input(|i| typed_text(&i.events));
        let mut action = None;
        if !typed && pressed(&HISTORY) {
            action = Some(ConsoleAction::History(String::new()));
        }
        if editor_keys && !typed && pressed(&FORMAT) {
            self.format(&ctx, cx.tab);
        }
        if cx.running {
            let focused = ctx.memory(|m| m.focused());
            if escape_cancels(focused, id, self.completion.open, self.grid_editing())
                && pressed(&KeyboardShortcut::new(Modifiers::NONE, Key::Escape))
            {
                action = Some(ConsoleAction::Cancel);
            }
            // A run at a time per console; swallow run keys meanwhile.
            if editor_keys {
                pressed(&RUN);
            }
            pressed(&RUN_ALL);
            return action;
        }
        if editor_keys && pressed(&RUN) && cx.connected {
            action = self.run_current().or(action);
        }
        if pressed(&RUN_ALL) && cx.connected {
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
                .on_hover_text("Sélection ou instruction au curseur (Ctrl+Entrée)")
                .clicked()
            {
                action = self.run_current();
            }
            if ui
                .add_enabled(
                    can_run,
                    egui::Button::new(format!("{} Tout", icon::FAST_FORWARD)),
                )
                .on_hover_text("Tout le script (F5)")
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
            if ui
                .button(format!("{} Formater", icon::MAGIC_WAND))
                .on_hover_text("Formater la sélection ou tout (Ctrl+Alt+L)")
                .clicked()
            {
                self.format(ui.ctx(), cx.tab);
            }
            if ui
                .button(format!("{} Historique", icon::CLOCK_COUNTER_CLOCKWISE))
                .on_hover_text("Requêtes exécutées (Ctrl+Alt+E)")
                .clicked()
            {
                action = Some(ConsoleAction::History(String::new()));
            }
            ui.separator();
            if let Some(name) = connection_selector(ui, cx) {
                action = Some(ConsoleAction::Rebind(name));
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
    egui::ComboBox::from_id_salt(("console_connection", cx.tab))
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
    fn altgr_characters_are_not_shortcuts() {
        let key = egui::Event::Key {
            key: Key::E,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: HISTORY.modifiers,
        };
        assert!(!typed_text(std::slice::from_ref(&key)), "a real Ctrl+Alt+E");
        assert!(typed_text(&[key, egui::Event::Text("€".into())]), "AltGr+E");
        assert!(!typed_text(&[egui::Event::Text(String::new())]));
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
