use std::sync::{Arc, Mutex};

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText};
use egui_phosphor::regular as icon;

use crate::engine::config::AppConfig;
use crate::engine::services::history::History;
use crate::engine::services::query_tabs::{QueryTab, QueryTabsState};
use crate::updater::{ExitAction, Updater};

use super::dialogs::{self, Dialog};
use super::sessions::{Session, Sessions};
use super::status::{self, Status, StatusKind};
use super::tabs::{self, console::ConsoleTab, data::DataTab, ddl::DdlTab, TabId, TabKind, Tabs};
use super::theme::{self, ThemeChoice, ERROR};
use super::worker::{Event, Worker};

pub const NEW_CONSOLE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::T);
pub const CLOSE_TAB: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::W);
pub const SAVE_CONSOLES: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
pub const REFRESH: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);

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
    #[allow(dead_code)] // explorer (plan 3b, Task 5)
    pub explorer_filter: String,
    pub show_value_panel: bool,
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

    #[allow(dead_code)] // explorer (plan 3b, Task 5)
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

    #[allow(dead_code)] // explorer (plan 3b, Task 5)
    /// Activate the data tab of `table` (qualified), opening it if needed.
    pub fn open_data(&mut self, connection: &str, table: &str, title: &str) {
        match self.tabs.find_data(connection, table) {
            Some(i) => self.tabs.active = i,
            None => {
                self.tabs.add(
                    connection.to_string(),
                    title.to_string(),
                    TabKind::Data(DataTab::new(table.to_string())),
                );
            }
        }
    }

    #[allow(dead_code)] // explorer (plan 3b, Task 5)
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
            ev @ (Event::Script { .. }
            | Event::Page { .. }
            | Event::Count { .. }
            | Event::Submitted { .. }
            | Event::Ddl { .. }) => {
                // Tabs closed meanwhile simply drop their outcome.
                if let Some(tab) = ev.tab().and_then(|id| self.tabs.find(id)) {
                    tab.on_event(ev);
                }
            }
            Event::Progress { done, total, label } => self.progress = Some((done, total, label)),
            Event::SchemaApplied { table, outcome, .. } => match outcome {
                Ok(_) => self.success(format!("{table} modifiée")),
                Err(e) => self.error(format!("{table} : {e}")),
            },
            Event::Exported(outcome) => match outcome {
                Ok((n, path)) => {
                    self.success(format!("{n} ligne(s) exportée(s) vers {}", path.display()))
                }
                Err(e) => self.error(format!("Export impossible : {e}")),
            },
            Event::Imported { table, outcome } => {
                self.progress = None;
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
            Event::Batch { kind, report } => {
                self.progress = None;
                if report.errors.is_empty() {
                    self.success(format!(
                        "{kind} : {}/{} table(s)",
                        report.succeeded, report.total
                    ));
                } else {
                    self.error(format!(
                        "{kind} : {} erreur(s), première : {}",
                        report.errors.len(),
                        report.errors[0]
                    ));
                }
            }
            ev @ Event::TestFinished(_) => dialogs::handle_event(self, ev),
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
        if pressed(&REFRESH) {
            // Data tabs reload their page instead (plan 3b, Task 8).
            if let Some(c) = self.tabs.active().map(|t| t.connection.clone()) {
                self.refresh(&c);
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
                        if r.middle_clicked() || ui.small_button(icon::X).clicked() {
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
        let Some(tab) = self.tabs.active_mut() else {
            return;
        };
        let id = tab.id;
        match &mut tab.kind {
            TabKind::Console(c) => c.show(ui, id),
            TabKind::Data(d) => d.show(ui),
            TabKind::Ddl(d) => d.show(ui),
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
