//! The tab bar and the active tab drawn in the central panel.

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::gui::tabs::console::ConsoleContext;
use crate::gui::tabs::data::DataContext;
use crate::gui::tabs::ddl::DdlAction;
use crate::gui::tabs::TabKind;
use crate::gui::theme::{self, ERROR};

use super::App;

impl App {
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
            Some(Action::Close(i)) => self.request_close(i),
            Some(Action::NewConsole) => match self.current_connection() {
                Some(c) => {
                    self.new_console(&c, String::new());
                }
                None => self.error("Ouvrez d'abord une connexion"),
            },
            None => {}
        }
    }

    pub(super) fn central(&mut self, ui: &mut egui::Ui) {
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
        let config = &self.config;
        let color_of = |name: &str| {
            theme::connection_color(
                config
                    .connections
                    .iter()
                    .find(|c| c.name == name)
                    .and_then(|c| c.color),
            )
        };
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
                        color_of: &color_of,
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
