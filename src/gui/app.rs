use std::sync::{Arc, Mutex};

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};
use egui_phosphor::regular as icon;

use crate::engine::config::AppConfig;
use crate::engine::db::utils::display_qualified;
use crate::engine::models::ConnectionConfig;
use crate::engine::services::history::{History, HistoryEntry};
use crate::engine::services::query_tabs::{QueryTab, QueryTabsState};
use crate::engine::sql::paging::{build_count, build_select};
use crate::updater::{ExitAction, Updater};

use super::dialogs::{self, connection::same_target, transfer, Dialog};
use super::history_popup::{self, HistoryPopup};
use super::sessions::{Session, Sessions};
use super::status::{self, Status, StatusKind};
use super::table_search::TableSearch;
use super::tabs::console::{self, ConsoleAction, ConsoleContext, ConsoleTab, RunKind};
use super::tabs::data::{DataAction, DataContext, DataTab, Nav};
use super::tabs::ddl::{DdlAction, DdlTab};
use super::tabs::{self, TabId, TabKind, Tabs};
use super::theme::{self, ThemeChoice, ERROR};
use super::value_panel::ValuePanel;
use super::worker::{Event, Worker};

pub const NEW_CONSOLE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
pub const CLOSE_TAB: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
pub const SAVE_CONSOLES: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
pub const REFRESH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
pub const TABLE_SEARCH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::N);

pub struct App {
    pub config: AppConfig,
    pub theme: ThemeChoice,
    /// Read by `gui::run` after the window closes.
    pub exit_action: Arc<Mutex<Option<ExitAction>>>,
    pub worker: Worker,
    pub updater: Updater,
    pub sessions: Sessions,
    pub tabs: Tabs,
    pub history: History,
    pub explorer_filter: String,
    pub show_value_panel: bool,
    pub value_panel: ValuePanel,
    pub status: Status,
    /// Import/export progress `(done, total, label)`.
    pub progress: Option<(usize, usize, String)>,
    pub dialog: Option<Dialog>,
    /// Persisted consoles whose connection hasn't been opened yet.
    pending_consoles: Vec<QueryTab>,
    /// Consoles without a connection (written by the TUI) go to the first
    /// connection opened; set once that happened.
    unbound_restored: bool,
    closed: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        exit_action: Arc<Mutex<Option<ExitAction>>>,
    ) -> Self {
        let config = AppConfig::load().unwrap_or_default();
        let theme = ThemeChoice::from_config(config.theme.as_deref());
        cc.egui_ctx.set_fonts(theme::fonts());
        theme::apply(&cc.egui_ctx, theme);

        let ctx = cc.egui_ctx.clone();
        let worker = Worker::new(move || ctx.request_repaint());
        let ctx = cc.egui_ctx.clone();
        let mut updater = Updater::new(move || ctx.request_repaint());
        updater.check(false, config.skipped_version.as_deref());

        let history_path = History::default_path()
            .unwrap_or_else(|_| std::env::temp_dir().join("storingUnicorns-history.json"));
        let mut status = Status::default();
        let pending_consoles = match QueryTabsState::load() {
            Ok(state) => state
                .tabs
                .into_iter()
                // `load` inserts an empty default tab when there is none.
                .filter(|t| !(t.query.is_empty() && t.connection.is_none()))
                .collect(),
            Err(e) => {
                status = Status {
                    text: format!("Lecture des consoles impossible : {e}"),
                    kind: StatusKind::Error,
                };
                Vec::new()
            }
        };

        Self {
            config,
            theme,
            exit_action,
            worker,
            updater,
            sessions: Sessions::default(),
            tabs: Tabs::default(),
            history: History::load_from(history_path),
            explorer_filter: String::new(),
            show_value_panel: false,
            value_panel: ValuePanel::default(),
            status,
            progress: None,
            dialog: None,
            pending_consoles,
            unbound_restored: false,
            closed: false,
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Info);
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Success);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.set_status(text, StatusKind::Error);
    }

    fn set_status(&mut self, text: impl Into<String>, kind: StatusKind) {
        self.status = Status {
            text: text.into(),
            kind,
        };
    }

    pub fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.error(format!("Impossible d'enregistrer la configuration : {e}"));
        }
    }

    /// Tag colour of connection `name`.
    pub fn color_of(&self, name: &str) -> Color32 {
        theme::connection_color(
            self.config
                .connections
                .iter()
                .find(|c| c.name == name)
                .and_then(|c| c.color),
        )
    }

    /// Open connection `name` in the background (no-op while it's opening).
    pub fn connect(&mut self, name: &str) {
        let Some(config) = self
            .config
            .connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
        else {
            self.error(format!("Connexion « {name} » introuvable"));
            return;
        };
        if !self.sessions.connecting.insert(name.to_string()) {
            return;
        }
        self.sessions.errors.remove(name);
        self.worker.connect(config);
    }

    /// Store a new (`original == None`) or edited connection. A rename moves
    /// its tabs and saved consoles along; a change of target (host, base…)
    /// drops its open session.
    pub fn save_connection(&mut self, original: Option<String>, config: ConnectionConfig) {
        let index = original
            .as_ref()
            .and_then(|old| self.config.connections.iter().position(|c| &c.name == old));
        let (Some(old), Some(index)) = (original, index) else {
            self.config.connections.push(config);
            self.save_config();
            return;
        };
        let previous = std::mem::replace(&mut self.config.connections[index], config.clone());
        let keep_session = same_target(&previous, &config);
        let new = config.name.clone();
        if old != new {
            for tab in self.tabs.list.iter_mut().filter(|t| t.connection == old) {
                tab.connection = new.clone();
            }
            for q in &mut self.pending_consoles {
                if q.connection.as_deref() == Some(old.as_str()) {
                    q.connection = Some(new.clone());
                }
            }
            if self.config.last_connection.as_deref() == Some(old.as_str()) {
                self.config.last_connection = Some(new.clone());
            }
            self.sessions.errors.remove(&old);
            // Metadata caches are keyed by name.
            self.worker.forget(&old);
            if let Some(session) = self.sessions.open.remove(&old) {
                if keep_session {
                    self.sessions.open.insert(new.clone(), session);
                }
            }
        } else if !keep_session {
            self.disconnect(&old);
        }
        if let Some(session) = self.sessions.get_mut(&new) {
            session.config = config;
        }
        self.save_config();
    }

    /// Remove a (disconnected) connection from the configuration.
    pub fn delete_connection(&mut self, name: &str) {
        self.config.connections.retain(|c| c.name != name);
        self.sessions.errors.remove(name);
        if self.config.last_connection.as_deref() == Some(name) {
            self.config.last_connection = None;
        }
        self.save_config();
        self.info(format!("Connexion « {name} » supprimée"));
    }

    /// Drop the session of `name`; its tabs stay open, marked disconnected.
    pub fn disconnect(&mut self, name: &str) {
        if self.sessions.open.remove(name).is_some() {
            self.worker.forget(name);
            self.info(format!("{name} déconnecté"));
        }
    }

    /// Reload the schemas of `name` and forget its cached table details.
    pub fn refresh(&mut self, name: &str) {
        let Some(session) = self.sessions.get_mut(name) else {
            return;
        };
        session.details.clear();
        session.loading.clear();
        let conn = session.conn.clone();
        self.worker.forget(name);
        self.worker.refresh_schemas(name.to_string(), conn);
    }

    /// Connection of the active tab when it is open, else the first open one.
    pub fn current_connection(&self) -> Option<String> {
        self.tabs
            .active()
            .map(|t| &t.connection)
            .filter(|c| self.sessions.open.contains_key(*c))
            .or_else(|| self.sessions.open.keys().next())
            .cloned()
    }

    pub fn new_console(&mut self, connection: &str, query: String) -> TabId {
        let title = format!("Console {}", self.tabs.console_count() + 1);
        let cursor = query.len();
        self.tabs.add(
            connection.to_string(),
            title,
            TabKind::Console(ConsoleTab::new(query, cursor)),
        )
    }

    /// Activate the data tab of `table` (qualified), opening it if needed.
    pub fn open_data(&mut self, connection: &str, table: &str, title: &str) {
        match self.tabs.find_data(connection, table) {
            Some(i) => self.tabs.active = i,
            None => {
                let quotes = self
                    .sessions
                    .get(connection)
                    .map_or(('"', '"'), |s| s.quotes);
                let id = self.tabs.add(
                    connection.to_string(),
                    title.to_string(),
                    TabKind::Data(Box::new(DataTab::new(table.to_string(), quotes))),
                );
                self.data_action(id, DataAction::Load { count: true });
            }
        }
    }

    /// Apply what data tab `id` asked for.
    pub fn data_action(&mut self, id: TabId, action: DataAction) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Data(d) = &mut tab.kind else {
            return;
        };
        match action {
            DataAction::Load { count } => {
                let Some(session) = self.sessions.get(&tab.connection) else {
                    return;
                };
                let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
                d.needs_load = false;
                d.loading = true;
                let sql = build_select(&d.query, &db_type);
                tab.runs.page = Some(
                    self.worker
                        .load_page(id, conn.clone(), d.table.clone(), sql),
                );
                if count {
                    d.total = None;
                    tab.runs.count = Some(self.worker.count(id, conn, build_count(&d.query)));
                }
            }
            DataAction::Submit => {
                if tab.runs.submit.is_some() || d.grid.edits.is_empty() {
                    d.after_submit = None;
                    return;
                }
                let Some(result) = &d.result else {
                    return;
                };
                let Some(session) = self.sessions.get(&tab.connection) else {
                    d.after_submit = None;
                    let text = format!("{} n'est pas connectée", tab.connection);
                    self.error(text);
                    return;
                };
                let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
                let run = self.worker.submit(
                    id,
                    conn,
                    db_type,
                    d.table.clone(),
                    result.columns.clone(),
                    d.grid.edits.to_row_changes(&result.rows),
                );
                d.error = None;
                tab.runs.submit = Some(run);
                tab.summary = Some("Submit…".into());
            }
            DataAction::Confirm => self.dialog = Some(Dialog::DiscardEdits(id)),
            DataAction::Ddl => {
                let (connection, table, title) =
                    (tab.connection.clone(), d.table.clone(), tab.title.clone());
                self.open_ddl(&connection, &table, &title);
            }
            DataAction::Applied(n) => {
                self.success(format!("{n} modification(s) appliquée(s)"));
                self.data_action(id, DataAction::Load { count: true });
            }
            DataAction::SubmitFailed(e) => {
                self.error(format!("Submit annulé (transaction annulée) : {e}"))
            }
            DataAction::Export => {
                let Some(result) = d.result.clone() else {
                    return;
                };
                let (connection, table) = (tab.connection.clone(), d.table.clone());
                transfer::export_result(self, &connection, result, table);
            }
        }
    }

    /// Reload the open data tabs of `connection` (only those of `table` when
    /// given) after their rows or columns changed elsewhere. Tabs with
    /// pending edits are left alone.
    pub fn reload_data_tabs(&mut self, connection: &str, table: Option<&str>) {
        let ids: Vec<TabId> = self
            .tabs
            .list
            .iter()
            .filter(|t| t.connection == connection)
            .filter(|t| {
                matches!(&t.kind, TabKind::Data(d)
                    if table.is_none_or(|x| d.table == x) && d.grid.edits.is_empty())
            })
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.data_action(id, DataAction::Load { count: true });
        }
    }

    /// Open a DDL tab for `table` (qualified) and request its `CREATE TABLE`.
    pub fn open_ddl(&mut self, connection: &str, table: &str, title: &str) {
        let Some(session) = self.sessions.get(connection) else {
            return;
        };
        let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
        let id = self.tabs.add(
            connection.to_string(),
            format!("DDL {title}"),
            TabKind::Ddl(DdlTab::new(table.to_string())),
        );
        let run = self.worker.ddl(id, conn, db_type, table.to_string());
        if let Some(tab) = self.tabs.find(id) {
            tab.runs.ddl = Some(run);
        }
    }

    /// Request the `CREATE TABLE` of DDL tab `id` again.
    fn reload_ddl(&mut self, id: TabId) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let (TabKind::Ddl(d), Some(session)) = (&mut tab.kind, self.sessions.get(&tab.connection))
        else {
            return;
        };
        let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
        d.reload();
        tab.runs.ddl = Some(self.worker.ddl(id, conn, db_type, d.table.clone()));
    }

    /// Ctrl+N: search a table in every open connection.
    fn table_search(&mut self) {
        if self.sessions.open.is_empty() {
            self.error("Ouvrez d'abord une connexion");
            return;
        }
        self.dialog = Some(Dialog::TableSearch(TableSearch::new(
            self.sessions.all_tables(),
        )));
    }

    pub fn close_tab(&mut self, index: usize) {
        if let Some(tab) = self.tabs.close(index) {
            self.worker.cancel(tab.id);
        }
    }

    /// Write every console (open or not yet restored) to `queries.toml`.
    pub fn save_consoles(&mut self) -> anyhow::Result<()> {
        tabs::persisted_consoles(&self.tabs, &self.pending_consoles).save()
    }

    fn on_connected(&mut self, session: Session) {
        let name = session.config.name.clone();
        self.sessions.connecting.remove(&name);
        self.sessions.errors.remove(&name);
        self.sessions.open.insert(name.clone(), session);
        self.success(format!("Connecté à {name}"));
        if self.config.last_connection.as_deref() != Some(name.as_str()) {
            self.config.last_connection = Some(name.clone());
            self.save_config();
        }

        let restored =
            tabs::take_consoles(&mut self.pending_consoles, &name, !self.unbound_restored);
        self.unbound_restored = true;
        for q in restored {
            self.tabs.add(
                name.clone(),
                q.name,
                TabKind::Console(ConsoleTab::new(q.query, q.cursor_position)),
            );
        }
        if !self.tabs.list.iter().any(|t| t.connection == name) {
            self.new_console(&name, String::new());
        }
    }

    fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Connected {
                name,
                conn,
                schemas,
            } => {
                let Some(config) = self
                    .config
                    .connections
                    .iter()
                    .find(|c| c.name == name)
                    .cloned()
                else {
                    // Deleted or renamed while connecting.
                    self.sessions.connecting.remove(&name);
                    return;
                };
                self.on_connected(Session::new(config, conn, schemas));
            }
            Event::ConnectFailed { name, error } => {
                self.sessions.connecting.remove(&name);
                self.error(format!("Connexion à {name} impossible : {error}"));
                self.sessions.errors.insert(name, error);
            }
            Event::Schemas { name, outcome } => match outcome {
                Ok(schemas) => {
                    if let Some(s) = self.sessions.get_mut(&name) {
                        s.schemas = schemas;
                    }
                }
                Err(e) => self.error(format!("{name} : lecture des tables impossible : {e}")),
            },
            Event::Details {
                name,
                table,
                outcome,
            } => {
                if let Some(s) = self.sessions.get_mut(&name) {
                    s.loading.remove(&table);
                    s.details.insert(table, outcome);
                }
            }
            ev @ Event::Script { .. } => self.on_script(ev),
            ev @ Event::Submitted { .. } if self.is_console(&ev) => self.on_console_submitted(ev),
            ev @ (Event::Page { .. }
            | Event::Count { .. }
            | Event::Submitted { .. }
            | Event::Ddl { .. }) => {
                // Tabs closed meanwhile simply drop their outcome.
                let Some(id) = ev.tab() else {
                    return;
                };
                if let Some(action) = self.tabs.find(id).and_then(|tab| tab.on_event(ev)) {
                    self.data_action(id, action);
                }
            }
            Event::Progress { done, total, label } => self.progress = Some((done, total, label)),
            Event::SchemaApplied {
                name,
                table,
                outcome,
            } => dialogs::structure::on_applied(self, name, table, outcome),
            Event::Exported(outcome) => match outcome {
                Ok((n, path)) => {
                    self.success(format!("{n} ligne(s) exportée(s) vers {}", path.display()))
                }
                Err(e) => self.error(format!("Export impossible : {e}")),
            },
            Event::Imported {
                name,
                table,
                outcome,
            } => {
                self.progress = None;
                if outcome.as_ref().is_ok_and(|s| s.succeeded() > 0) {
                    self.reload_data_tabs(&name, Some(&table));
                }
                let table = display_qualified(&table);
                match outcome {
                    Ok(stats) if stats.errors.is_empty() => self.success(format!(
                        "{table} : {} insérée(s), {} mise(s) à jour",
                        stats.inserted, stats.updated
                    )),
                    Ok(stats) => self.error(format!(
                        "{table} : {} erreur(s), première : {}",
                        stats.errors.len(),
                        stats.errors[0]
                    )),
                    Err(e) => self.error(format!("{table} : {e}")),
                }
            }
            Event::Batch { name, kind, report } => {
                self.progress = None;
                if kind != "Export" && report.succeeded > 0 {
                    self.reload_data_tabs(&name, None);
                }
                if report.errors.is_empty() {
                    self.success(format!(
                        "{kind} ({name}) : {}/{} table(s), {} ligne(s)",
                        report.succeeded, report.total, report.rows_affected
                    ));
                } else {
                    self.error(format!(
                        "{kind} ({name}) : {} erreur(s), première : {}",
                        report.errors.len(),
                        report.errors[0]
                    ));
                }
            }
            ev @ Event::TestFinished(_) => dialogs::handle_event(self, ev),
        }
    }

    /// Whether the outcome `ev` belongs to an open console tab.
    fn is_console(&mut self, ev: &Event) -> bool {
        ev.tab()
            .and_then(|id| self.tabs.find(id))
            .is_some_and(|t| matches!(t.kind, TabKind::Console(_)))
    }

    /// Submit of a console result: on success clear its edits and re-run its
    /// SQL to refresh it; on failure keep the edits and show the error.
    fn on_console_submitted(&mut self, ev: Event) {
        let Some(tab) = ev.tab().and_then(|id| self.tabs.find(id)) else {
            return;
        };
        if !tab.runs.accept(&ev) {
            return;
        }
        let (Event::Submitted { outcome, .. }, TabKind::Console(c)) = (ev, &mut tab.kind) else {
            return;
        };
        let Some(id) = c.submitting.take() else {
            return;
        };
        let Some(r) = c.results.iter_mut().find(|r| r.id == id) else {
            return;
        };
        match outcome {
            Ok(n) => {
                r.grid.editing = None;
                r.grid.edits.clear();
                r.error = None;
                let sql = r.sql.clone();
                let text = format!("{n} modification(s) appliquée(s)");
                tab.summary = Some(text.clone());
                // A run in progress keeps going: the result refreshes on the next run.
                if let (None, Some(conn)) = (tab.runs.script, self.sessions.conn(&tab.connection)) {
                    let run = self
                        .worker
                        .run_script(tab.id, conn, sql, Some(console::MAX_ROWS));
                    tab.runs.script = Some(run);
                    c.refreshing = Some(id);
                    c.running_since = Some(std::time::Instant::now());
                }
                self.success(text);
            }
            Err(e) => {
                r.error = Some(e.clone());
                self.error(format!("Submit annulé (transaction annulée) : {e}"));
            }
        }
    }

    /// Outcomes of a console run: result tabs, log, history, summary.
    fn on_script(&mut self, ev: Event) {
        let Some(tab) = ev.tab().and_then(|id| self.tabs.find(id)) else {
            return;
        };
        if !tab.runs.accept(&ev) {
            return;
        }
        let (Event::Script { outcomes, .. }, TabKind::Console(c)) = (ev, &mut tab.kind) else {
            return;
        };
        if let Some(id) = c.refreshing.take() {
            tab.summary = Some(console::apply_refresh(
                c,
                id,
                outcomes,
                chrono::Local::now(),
            ));
            return;
        }
        let at = history_popup::now_secs();
        for o in outcomes.iter().filter(|o| !o.sql.trim().is_empty()) {
            self.history.push(HistoryEntry {
                sql: o.sql.clone(),
                connection: tab.connection.clone(),
                at,
                duration_ms: o.elapsed_ms as u64,
                ok: o.result.is_ok(),
            });
        }
        tab.summary = Some(console::apply_outcomes(c, outcomes, chrono::Local::now()));
    }

    /// Apply what the console at `index` asked for.
    fn console_action(&mut self, index: usize, action: ConsoleAction) {
        let Some(tab) = self.tabs.list.get_mut(index) else {
            return;
        };
        let TabKind::Console(c) = &mut tab.kind else {
            return;
        };
        match action {
            ConsoleAction::Run(kind) => {
                let Some(conn) = self.sessions.conn(&tab.connection) else {
                    self.status = Status {
                        text: format!("{} n'est pas connectée", tab.connection),
                        kind: StatusKind::Error,
                    };
                    return;
                };
                let max = Some(console::MAX_ROWS);
                let run = match kind {
                    RunKind::Script(text) => self.worker.run_script(tab.id, conn, text, max),
                    RunKind::AtCursor { text, cursor } => {
                        self.worker.run_at_cursor(tab.id, conn, text, cursor, max)
                    }
                };
                tab.runs.script = Some(run);
                c.refreshing = None;
                c.running_since = Some(std::time::Instant::now());
                tab.summary = Some("Exécution…".into());
            }
            ConsoleAction::Cancel => {
                self.worker.cancel(tab.id);
                tab.runs.script = None;
                c.refreshing = None;
                c.cancelled();
                tab.summary = Some("Annulé".into());
            }
            ConsoleAction::Rebind(name) => {
                tab.connection = name;
                c.completion = Default::default();
            }
            ConsoleAction::History(search) => {
                self.dialog = Some(Dialog::History(HistoryPopup::new(search)));
            }
            ConsoleAction::Export { result, table } => {
                let connection = tab.connection.clone();
                transfer::export_result(self, &connection, result, table);
            }
            ConsoleAction::Submit {
                result_index,
                table,
                columns,
                changes,
            } => {
                if tab.runs.submit.is_some() {
                    return;
                }
                let Some(session) = self.sessions.get(&tab.connection) else {
                    self.status = Status {
                        text: format!("{} n'est pas connectée", tab.connection),
                        kind: StatusKind::Error,
                    };
                    return;
                };
                let Some(r) = c.results.get_mut(result_index) else {
                    return;
                };
                r.error = None;
                c.submitting = Some(r.id);
                let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
                let run = self
                    .worker
                    .submit(tab.id, conn, db_type, table, columns, changes);
                tab.runs.submit = Some(run);
                tab.summary = Some("Submit…".into());
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        let pressed = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
        if pressed(&NEW_CONSOLE) {
            match self.current_connection() {
                Some(c) => {
                    self.new_console(&c, String::new());
                }
                None => self.error("Ouvrez d'abord une connexion"),
            }
        }
        if pressed(&CLOSE_TAB) {
            self.close_tab(self.tabs.active);
        }
        if pressed(&SAVE_CONSOLES) {
            match self.save_consoles() {
                Ok(()) => self.info("Consoles enregistrées"),
                Err(e) => self.error(format!("Enregistrement des consoles impossible : {e}")),
            }
        }
        if pressed(&TABLE_SEARCH) {
            self.table_search();
        }
        if pressed(&REFRESH) {
            // A data tab reloads its page, other tabs their connection.
            let active = self.tabs.active_mut().map(|t| match &mut t.kind {
                TabKind::Data(d) => Err((t.id, d.navigate(Nav::Refresh))),
                _ => Ok(t.connection.clone()),
            });
            match active {
                Some(Ok(c)) => self.refresh(&c),
                Some(Err((id, action))) => self.data_action(id, action),
                None => {}
            }
        }
    }

    fn on_close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        if let Err(e) = self.save_consoles() {
            tracing::error!("saving consoles: {e}");
        }
        if let Err(e) = self.history.save() {
            tracing::error!("saving history: {e}");
        }
        self.config.theme = self.theme.to_config();
        if let Err(e) = self.config.save() {
            tracing::error!("saving config: {e}");
        }
        self.updater.schedule_on_quit();
        *self
            .exit_action
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.updater.exit_action.clone();
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        enum Action {
            Activate(usize),
            Close(usize),
            NewConsole,
        }
        let mut action = None;
        egui::ScrollArea::horizontal()
            .id_salt("tab_bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, tab) in self.tabs.list.iter().enumerate() {
                        let spacing = ui.spacing().item_spacing.x;
                        ui.spacing_mut().item_spacing.x = 2.0;
                        theme::dot(ui, self.color_of(&tab.connection))
                            .on_hover_text(&tab.connection);
                        let label = format!("{} {}", tab.kind.icon(), tab.title);
                        let r = ui
                            .selectable_label(i == self.tabs.active, label)
                            .on_hover_text(&tab.connection);
                        if r.clicked() {
                            action = Some(Action::Activate(i));
                        }
                        let close = ui
                            .small_button(icon::X)
                            .on_hover_text("Fermer (Ctrl+W)")
                            .clicked();
                        if close || r.middle_clicked() {
                            action = Some(Action::Close(i));
                        }
                        ui.spacing_mut().item_spacing.x = spacing;
                        ui.separator();
                    }
                    if ui
                        .small_button(icon::PLUS)
                        .on_hover_text("Nouvelle console (Ctrl+T)")
                        .clicked()
                    {
                        action = Some(Action::NewConsole);
                    }
                });
            });
        match action {
            Some(Action::Activate(i)) => self.tabs.active = i,
            Some(Action::Close(i)) => self.close_tab(i),
            Some(Action::NewConsole) => match self.current_connection() {
                Some(c) => {
                    self.new_console(&c, String::new());
                }
                None => self.error("Ouvrez d'abord une connexion"),
            },
            None => {}
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        if self.tabs.list.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Ouvrez une connexion dans l'explorateur").weak());
            });
            return;
        }
        self.tab_bar(ui);
        ui.separator();

        let Some(connection) = self.tabs.active().map(|t| t.connection.clone()) else {
            return;
        };
        if !self.sessions.open.contains_key(&connection) {
            ui.horizontal(|ui| {
                ui.colored_label(ERROR, format!("{} Déconnecté", icon::PLUGS));
                if self.sessions.connecting.contains(&connection) {
                    ui.spinner();
                } else if ui.button("Reconnecter").clicked() {
                    self.connect(&connection);
                }
            });
            ui.separator();
        }
        let connections: Vec<_> = self
            .sessions
            .open
            .keys()
            .map(|name| (name.clone(), self.color_of(name)))
            .collect();
        let shortcuts = self.dialog.is_none();
        let index = self.tabs.active;
        let sessions = &self.sessions;
        let Some(tab) = self.tabs.list.get_mut(index) else {
            return;
        };
        let mut data_action = None;
        let mut ddl_action = None;
        let action = match &mut tab.kind {
            TabKind::Console(c) => {
                let name = tab.connection.as_str();
                let tables = || sessions.tables_of(name);
                c.show(
                    ui,
                    ConsoleContext {
                        tab: tab.id,
                        connection: name,
                        connections: &connections,
                        connected: sessions.open.contains_key(name),
                        running: tab.runs.script.is_some(),
                        shortcuts,
                        tables: &tables,
                    },
                )
            }
            TabKind::Data(d) => {
                data_action = d.show(
                    ui,
                    DataContext {
                        tab: tab.id,
                        connected: sessions.open.contains_key(&tab.connection),
                        submitting: tab.runs.submit.is_some(),
                        loading: tab.runs.page.is_some(),
                    },
                );
                None
            }
            TabKind::Ddl(d) => {
                ddl_action = d.show(ui, sessions.open.contains_key(&tab.connection));
                None
            }
        };
        let id = tab.id;
        if let Some(action) = action {
            self.console_action(index, action);
        }
        if let Some(action) = data_action {
            self.data_action(id, action);
        }
        if let Some(DdlAction::Refresh) = ddl_action {
            self.reload_ddl(id);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for ev in self.worker.poll() {
            self.handle_event(ev);
        }
        status::poll_updater(self);
        self.shortcuts(ctx);

        status::update_banner(self, ctx);
        status::status_bar(self, ctx);
        egui::SidePanel::left("explorer")
            .resizable(true)
            .default_width(280.0)
            .width_range(180.0..=600.0)
            .show(ctx, |ui| super::explorer::show(self, ui));
        if self.show_value_panel {
            egui::SidePanel::right("value")
                .resizable(true)
                .default_width(320.0)
                .width_range(200.0..=800.0)
                .show(ctx, |ui| super::value_panel::show(self, ui));
        }
        egui::CentralPanel::default().show(ctx, |ui| self.central(ui));
        dialogs::show(self, ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            self.on_close();
        }
    }
}
