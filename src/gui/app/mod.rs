//! The application state, the connection lifecycle and the frame loop
//! (`actions`, `events` and `tabbar` hold the rest of `impl App`).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, Key, KeyboardShortcut, Modifiers};

use crate::engine::config::AppConfig;
use crate::engine::models::ConnectionConfig;
use crate::engine::services::history::History;
use crate::engine::services::query_tabs::{QueryTab, QueryTabsState};
use crate::updater::{ExitAction, Updater};

use super::dialogs::{self, connection::same_target, Dialog};
use super::persist::{self, Saver, CONSOLES_DELAY};
use super::sessions::Sessions;
use super::status::{self, Status, StatusKind};
use super::table_search::TableSearch;
use super::tabs::console::ConsoleTab;
use super::tabs::data::{DataAction, DataTab};
use super::tabs::ddl::DdlTab;
use super::tabs::{self, CloseRisk, TabId, TabKind, Tabs};
use super::theme::{self, ThemeChoice};
use super::value_panel::ValuePanel;
use super::worker::Worker;

mod actions;
mod events;
mod tabbar;

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
    pub explorer_tree: super::explorer::tree::Tree,
    pub show_value_panel: bool,
    pub value_panel: ValuePanel,
    pub status: Status,
    /// Import/export/truncate progress, per operation.
    pub progress: status::Progress,
    pub dialog: Option<Dialog>,
    /// Persisted consoles whose connection hasn't been opened yet.
    pending_consoles: Vec<QueryTab>,
    /// Consoles without a connection (written by the TUI) go to the first
    /// connection opened; set once that happened.
    unbound_restored: bool,
    closed: bool,
    /// "Fermer quand même" was answered: the next close request quits even
    /// with pending edits.
    pub quit_confirmed: bool,
    /// Writes the history off the UI thread after each run.
    history_saver: Saver<History>,
    /// Writes `queries.toml` off the UI thread.
    consoles_saver: Saver<QueryTabsState>,
    /// Last change of the consoles not saved yet (saved `CONSOLES_DELAY`
    /// after it).
    consoles_edited: Option<Instant>,
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
            explorer_tree: Default::default(),
            show_value_panel: false,
            value_panel: ValuePanel::default(),
            status,
            progress: status::Progress::default(),
            dialog: None,
            pending_consoles,
            unbound_restored: false,
            closed: false,
            quit_confirmed: false,
            history_saver: Saver::new(|h: &History| {
                if let Err(e) = h.save() {
                    tracing::error!("saving history: {e}");
                }
            }),
            consoles_saver: Saver::new(|s: &QueryTabsState| {
                if let Err(e) = s.save() {
                    tracing::error!("saving consoles: {e}");
                }
            }),
            consoles_edited: None,
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

    /// Report that `connection` must be connected first.
    pub fn not_connected(&mut self, connection: &str) {
        self.error(status::not_connected(connection));
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
            for tab in &mut self.tabs.list {
                if tab.connection == old {
                    tab.connection = new.clone();
                }
                if let TabKind::Console(c) = &mut tab.kind {
                    for r in c.results.iter_mut().filter(|r| r.connection == old) {
                        r.connection = new.clone();
                    }
                }
            }
            for q in &mut self.pending_consoles {
                if q.connection.as_deref() == Some(old.as_str()) {
                    q.connection = Some(new.clone());
                }
            }
            if self.config.last_connection.as_deref() == Some(old.as_str()) {
                self.config.last_connection = Some(new.clone());
            }
            self.consoles_changed();
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

    /// Remove a connection from the configuration, with its session, its
    /// tabs and its saved consoles.
    pub fn delete_connection(&mut self, name: &str) {
        self.disconnect(name);
        for tab in tabs::remove_connection(&mut self.tabs, &mut self.pending_consoles, name) {
            self.worker.cancel(tab.id);
        }
        self.consoles_changed();
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

    /// The consoles changed (text, title, connection, opened or closed):
    /// save them a moment later.
    pub fn consoles_changed(&mut self) {
        self.consoles_edited = Some(Instant::now());
    }

    pub fn new_console(&mut self, connection: &str, query: String) -> TabId {
        self.consoles_changed();
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

    /// Close the tab at `index`, after a confirmation when that would lose
    /// pending edits or a console's text.
    pub fn request_close(&mut self, index: usize) {
        let Some(tab) = self.tabs.list.get_mut(index) else {
            return;
        };
        match tab.close_risk() {
            CloseRisk::None => self.close_tab(index),
            risk => self.dialog = Some(Dialog::CloseTab { id: tab.id, risk }),
        }
    }

    /// Close the tab at `index` right away.
    pub fn close_tab(&mut self, index: usize) {
        if let Some(tab) = self.tabs.close(index) {
            self.worker.cancel(tab.id);
            if matches!(tab.kind, TabKind::Console(_)) {
                self.consoles_changed();
            }
        }
    }

    /// Close tab `id` right away (no-op when already closed).
    pub fn close_tab_id(&mut self, id: TabId) {
        if let Some(index) = self.tabs.list.iter().position(|t| t.id == id) {
            self.close_tab(index);
        }
    }

    /// "Submit" when closing data tab `id`: submit, close on success.
    pub fn submit_and_close(&mut self, id: TabId) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        if let TabKind::Data(d) = &mut tab.kind {
            d.close_after_submit = true;
            self.data_action(id, DataAction::Submit);
        }
    }

    /// Write every console (open or not yet restored) to `queries.toml` now
    /// (after any background save, which would otherwise overwrite it).
    pub fn save_consoles(&mut self) -> anyhow::Result<()> {
        self.consoles_saver.flush();
        self.consoles_edited = None;
        tabs::persisted_consoles(&self.tabs, &self.pending_consoles).save()
    }

    /// Save the consoles in the background once they haven't changed for
    /// `CONSOLES_DELAY`.
    fn autosave_consoles(&mut self, ctx: &egui::Context) {
        for tab in &mut self.tabs.list {
            if let TabKind::Console(c) = &mut tab.kind {
                if std::mem::take(&mut c.edited) {
                    self.consoles_edited = Some(Instant::now());
                }
            }
        }
        let now = Instant::now();
        if persist::should_save(self.consoles_edited, now, CONSOLES_DELAY) {
            self.consoles_edited = None;
            let state = tabs::persisted_consoles(&self.tabs, &self.pending_consoles);
            self.consoles_saver.save(state);
        } else if let Some(left) = persist::save_due_in(self.consoles_edited, now, CONSOLES_DELAY) {
            ctx.request_repaint_after(left);
        }
    }

    fn on_close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        // Background saves first: they must not land after these.
        self.history_saver.flush();
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
        self.autosave_consoles(ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            let pending = if self.quit_confirmed {
                0
            } else {
                self.tabs.with_pending_edits(None)
            };
            if pending > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::ConfirmQuit(pending));
            } else {
                self.on_close();
            }
        }
    }

    /// Final flush, whatever way the app ends (`on_close` runs once).
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.on_close();
    }
}
