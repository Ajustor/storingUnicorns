//! SQL console tab: editor, one result tab per row-returning statement, and
//! the "Sortie" log of every execution.

use std::time::Instant;

use chrono::{DateTime, Local};
use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};
use egui_extras::{Column as TableColumn, TableBuilder};
use egui_phosphor::regular as icon;

use crate::engine::models::QueryResult;
use crate::engine::ops::query::StatementOutcome;
use crate::engine::sql::format::format_sql;
use crate::engine::sql::statements::extract_table_from_query;
use crate::gui::editor::{self, completion::Completion, EditorContext};
use crate::gui::theme::{self, ACCENT, ERROR};

use super::TabId;

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

pub struct ResultTab {
    /// "Résultat 1", or the table name when the query reads one table.
    pub title: String,
    pub sql: String,
    pub result: QueryResult,
    pub pinned: bool,
}

pub struct LogLine {
    pub at: DateTime<Local>,
    pub sql: String,
    pub message: String,
    pub ok: bool,
}

pub struct ConsoleTab {
    pub query: String,
    /// Cursor position (bytes), as persisted in `QueryTab::cursor_position`.
    pub cursor: usize,
    /// Selected byte range of the editor, when not empty.
    pub selection: Option<(usize, usize)>,
    pub results: Vec<ResultTab>,
    /// `== results.len()` means the "Sortie" log tab.
    pub active_result: usize,
    pub log: Vec<LogLine>,
    pub running_since: Option<Instant>,
    /// Splitter between the editor and the results.
    pub editor_height: f32,
    /// Columns of the result tabs (completion and highlighting).
    pub known_columns: Vec<String>,
    pub completion: Completion,
    /// Move the editor cursor to `cursor` on the next frame (restored console).
    restore_cursor: bool,
}

/// How to run the console's SQL.
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
}

/// What the console needs from the app to draw itself.
pub struct ConsoleContext<'a> {
    pub tab: TabId,
    pub connection: &'a str,
    /// Open connections and their colour (selector).
    pub connections: &'a [(String, Color32)],
    pub connected: bool,
    pub running: bool,
    /// False while a modal is open: no shortcut fires.
    pub shortcuts: bool,
    pub tables: &'a dyn Fn() -> Vec<String>,
}

/// Title of the `index`-th (1-based) result tab of `sql`.
pub fn result_title(sql: &str, index: usize, truncated: bool) -> String {
    let table = extract_table_from_query(sql)
        .and_then(|t| {
            t.rsplit('.').next().map(|t| {
                t.trim_matches(|c| matches!(c, '"' | '`' | '[' | ']'))
                    .to_string()
            })
        })
        .filter(|t| !t.is_empty());
    let title = table.unwrap_or_else(|| format!("Résultat {index}"));
    if truncated {
        format!("{title} ({MAX_ROWS}+)")
    } else {
        title
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Log message of a successful outcome.
fn ok_message(result: &QueryResult, elapsed_ms: u128) -> String {
    if result.columns.is_empty() {
        format!(
            "{} en {elapsed_ms} ms",
            plural(
                result.rows_affected as usize,
                "ligne affectée",
                "lignes affectées"
            )
        )
    } else if result.truncated {
        format!("{MAX_ROWS}+ lignes (limité) en {elapsed_ms} ms")
    } else {
        format!(
            "{} en {elapsed_ms} ms",
            plural(result.rows.len(), "ligne", "lignes")
        )
    }
}

/// Apply a run's outcomes: unpinned result tabs are replaced by one tab per
/// row-returning outcome, each outcome is logged, and the first new result
/// (or "Sortie" on error / no rows) is activated. Returns the status-bar
/// summary.
pub fn apply_outcomes(
    console: &mut ConsoleTab,
    outcomes: Vec<StatementOutcome>,
    now: DateTime<Local>,
) -> String {
    console.running_since = None;
    console.results.retain(|r| r.pinned);
    let first_new = console.results.len();
    let mut failed = false;
    let mut total_ms = 0;
    let mut last_rows = None;
    for o in outcomes {
        total_ms += o.elapsed_ms;
        match o.result {
            Ok(result) => {
                console.log.push(LogLine {
                    at: now,
                    sql: o.sql.clone(),
                    message: ok_message(&result, o.elapsed_ms),
                    ok: true,
                });
                last_rows = Some(if result.columns.is_empty() {
                    plural(
                        result.rows_affected as usize,
                        "ligne affectée",
                        "lignes affectées",
                    )
                } else if result.truncated {
                    format!("{MAX_ROWS}+ lignes")
                } else {
                    plural(result.rows.len(), "ligne", "lignes")
                });
                if !result.columns.is_empty() {
                    let title = result_title(&o.sql, console.results.len() + 1, result.truncated);
                    console.results.push(ResultTab {
                        title,
                        sql: o.sql,
                        result,
                        pinned: false,
                    });
                }
            }
            Err(e) => {
                failed = true;
                console.log.push(LogLine {
                    at: now,
                    sql: o.sql,
                    message: e,
                    ok: false,
                });
            }
        }
    }
    console.active_result = if failed || console.results.len() == first_new {
        console.results.len()
    } else {
        first_new
    };
    console.refresh_known_columns();
    match (failed, last_rows) {
        (true, _) => format!("Erreur · {total_ms} ms"),
        (false, Some(rows)) => format!("{rows} · {total_ms} ms"),
        (false, None) => format!("{total_ms} ms"),
    }
}

impl ConsoleTab {
    pub fn new(query: String, cursor: usize) -> Self {
        let cursor = cursor.min(query.len());
        Self {
            query,
            cursor,
            selection: None,
            results: Vec::new(),
            active_result: 0,
            log: Vec::new(),
            running_since: None,
            editor_height: 220.0,
            known_columns: Vec::new(),
            completion: Completion::default(),
            restore_cursor: true,
        }
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
        self.log.push(LogLine {
            at: Local::now(),
            sql: String::new(),
            message: "Annulé".into(),
            ok: false,
        });
        self.active_result = self.results.len();
    }

    /// Insert `sql` at the cursor (replacing the selection) and focus the editor.
    pub fn insert(&mut self, ctx: &egui::Context, tab: TabId, sql: &str) {
        let (start, end) = self.selection.unwrap_or((self.cursor, self.cursor));
        let (start, end) = (start.min(self.query.len()), end.min(self.query.len()));
        self.query.replace_range(start..end, sql);
        self.cursor = start + sql.len();
        self.selection = None;
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
        self.splitter(ui);

        if let Some(a) = self.results_area(ui, id) {
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
        let mut action = None;
        if pressed(&HISTORY) {
            action = Some(ConsoleAction::History(String::new()));
        }
        if editor_keys && pressed(&FORMAT) {
            self.format(&ctx, cx.tab);
        }
        if cx.running {
            if !self.completion.open
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

    fn results_area(&mut self, ui: &mut egui::Ui, id: egui::Id) -> Option<ConsoleAction> {
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (i, r) in self.results.iter_mut().enumerate() {
                let spacing = ui.spacing().item_spacing.x;
                ui.spacing_mut().item_spacing.x = 2.0;
                let label = format!("{} {}", icon::TABLE, r.title);
                if ui
                    .selectable_label(i == self.active_result, label)
                    .on_hover_text(&r.sql)
                    .clicked()
                {
                    self.active_result = i;
                }
                let pin = if r.pinned {
                    RichText::new(icon::PUSH_PIN).color(ACCENT)
                } else {
                    RichText::new(icon::PUSH_PIN).weak()
                };
                if ui
                    .small_button(pin)
                    .on_hover_text(if r.pinned {
                        "Désépingler"
                    } else {
                        "Épingler (gardé à la prochaine exécution)"
                    })
                    .clicked()
                {
                    r.pinned = !r.pinned;
                }
                if ui.small_button(icon::X).on_hover_text("Fermer").clicked() {
                    close = Some(i);
                }
                ui.spacing_mut().item_spacing.x = spacing;
                ui.separator();
            }
            let errors = self.log.iter().rev().take_while(|l| !l.ok).count();
            let label = if errors > 0 {
                RichText::new(format!("{} Sortie", icon::LIST_BULLETS)).color(ERROR)
            } else {
                RichText::new(format!("{} Sortie", icon::LIST_BULLETS))
            };
            if ui
                .selectable_label(self.active_result >= self.results.len(), label)
                .clicked()
            {
                self.active_result = self.results.len();
            }
        });
        if let Some(i) = close {
            self.results.remove(i);
            if self.active_result > i || self.active_result > self.results.len() {
                self.active_result = self.active_result.saturating_sub(1);
            }
            self.refresh_known_columns();
        }
        ui.separator();
        match self.results.get(self.active_result) {
            Some(r) => {
                simple_result_table(ui, id.with(("result", self.active_result)), &r.result);
                None
            }
            None => self.log_view(ui, id),
        }
    }

    /// "Sortie": every execution, newest last. Clicking a line searches its
    /// SQL in the history.
    fn log_view(&self, ui: &mut egui::Ui, id: egui::Id) -> Option<ConsoleAction> {
        if self.log.is_empty() {
            ui.label(RichText::new("Exécutez une requête (Ctrl+Entrée) ou le script (F5).").weak());
            return None;
        }
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt(id.with("log"))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(line.at.format("%H:%M:%S").to_string()).weak());
                        let msg = RichText::new(&line.message);
                        ui.label(if line.ok { msg } else { msg.color(ERROR) });
                        if !line.sql.is_empty() {
                            let sql = crate::gui::history_popup::preview(&line.sql, 120);
                            let r = ui
                                .add(
                                    egui::Label::new(RichText::new(sql).monospace().weak())
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Chercher dans l'historique");
                            if r.clicked() {
                                action = Some(ConsoleAction::History(line.sql.clone()));
                            }
                        }
                    });
                }
            });
        action
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

/// Read-only, virtualised table of a console result (replaced by the data
/// grid in plan 3b, Task 7).
pub fn simple_result_table(ui: &mut egui::Ui, id: egui::Id, result: &QueryResult) {
    let n = result.columns.len();
    if n == 0 {
        return;
    }
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    egui::ScrollArea::horizontal()
        .id_salt(id.with("h"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            TableBuilder::new(ui)
                .id_salt(id)
                .striped(true)
                .resizable(true)
                .auto_shrink([false, false])
                .column(TableColumn::exact(44.0))
                .columns(TableColumn::initial(140.0).at_least(40.0).clip(true), n)
                .header(row_height + 2.0, |mut header| {
                    header.col(|ui| {
                        ui.label(RichText::new("#").weak());
                    });
                    for c in &result.columns {
                        header.col(|ui| {
                            let name = if c.is_primary_key {
                                format!("{} {}", icon::KEY, c.name)
                            } else {
                                c.name.clone()
                            };
                            ui.strong(name).on_hover_text(&c.type_name);
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_height, result.rows.len(), |mut row| {
                        let i = row.index();
                        row.col(|ui| {
                            ui.label(RichText::new((i + 1).to_string()).weak());
                        });
                        for cell in &result.rows[i] {
                            row.col(|ui| {
                                if cell == "NULL" {
                                    ui.label(RichText::new("NULL").weak().italics());
                                } else {
                                    ui.label(cell.as_str());
                                }
                            });
                        }
                    });
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::Column;

    fn result(columns: &[&str], rows: usize) -> QueryResult {
        QueryResult {
            columns: columns
                .iter()
                .map(|c| Column {
                    name: c.to_string(),
                    type_name: "TEXT".into(),
                    nullable: true,
                    is_primary_key: false,
                })
                .collect(),
            rows: vec![vec!["x".to_string(); columns.len()]; rows],
            ..Default::default()
        }
    }

    fn tab(title: &str, pinned: bool) -> ResultTab {
        ResultTab {
            title: title.into(),
            sql: format!("SELECT * FROM {title}"),
            result: result(&["a"], 1),
            pinned,
        }
    }

    fn ok(sql: &str, r: QueryResult) -> StatementOutcome {
        StatementOutcome {
            sql: sql.into(),
            result: Ok(r),
            elapsed_ms: 5,
        }
    }

    #[test]
    fn result_title_from_table_or_index() {
        assert_eq!(result_title("SELECT * FROM users", 1, false), "users");
        assert_eq!(
            result_title("select id from \"main\".\"users\" where 1", 2, false),
            "users"
        );
        assert_eq!(result_title("SELECT 1", 3, false), "Résultat 3");
        assert_eq!(result_title("SELECT 1", 1, true), "Résultat 1 (1000+)");
        assert_eq!(
            result_title("SELECT * FROM [dbo].[t]", 1, true),
            "t (1000+)"
        );
    }

    #[test]
    fn apply_outcomes_keeps_pinned_and_logs() {
        let mut c = ConsoleTab::new(String::new(), 0);
        c.results = vec![tab("kept", true), tab("dropped", false)];
        c.running_since = Some(Instant::now());
        let mut affected = QueryResult {
            rows_affected: 2,
            ..Default::default()
        };
        affected.columns.clear();
        let mut big = result(&["id", "name"], 3);
        big.truncated = true;
        let summary = apply_outcomes(
            &mut c,
            vec![
                ok("UPDATE t SET a = 1", affected),
                ok("SELECT id, name FROM users", big),
                ok("SELECT 42", result(&["?column?"], 1)),
            ],
            Local::now(),
        );
        let titles: Vec<_> = c.results.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["kept", "users (1000+)", "Résultat 3"]);
        assert_eq!(c.active_result, 1, "first new result");
        let messages: Vec<_> = c.log.iter().map(|l| l.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "2 lignes affectées en 5 ms",
                "1000+ lignes (limité) en 5 ms",
                "1 ligne en 5 ms"
            ]
        );
        assert!(c.log.iter().all(|l| l.ok));
        assert!(c.running_since.is_none());
        assert_eq!(summary, "1 ligne · 15 ms");
        assert_eq!(c.known_columns, ["?column?", "a", "id", "name"]);
    }

    #[test]
    fn apply_outcomes_activates_output_on_error_or_no_rows() {
        let mut c = ConsoleTab::new(String::new(), 0);
        c.results = vec![tab("old", false)];
        let summary = apply_outcomes(
            &mut c,
            vec![
                ok("SELECT * FROM t", result(&["a"], 2)),
                StatementOutcome {
                    sql: "SELEC".into(),
                    result: Err("syntax error".into()),
                    elapsed_ms: 1,
                },
            ],
            Local::now(),
        );
        assert_eq!(c.results.len(), 1);
        assert_eq!(c.results[0].title, "t");
        assert_eq!(c.active_result, 1, "Sortie");
        assert_eq!(c.log[1].message, "syntax error");
        assert!(!c.log[1].ok);
        assert_eq!(summary, "Erreur · 6 ms");

        let mut affected = QueryResult::default();
        affected.rows_affected = 1;
        apply_outcomes(&mut c, vec![ok("DELETE FROM t", affected)], Local::now());
        assert!(c.results.is_empty(), "unpinned results replaced");
        assert_eq!(c.active_result, 0, "Sortie when nothing returned rows");
        assert_eq!(c.log[2].message, "1 ligne affectée en 5 ms");
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
