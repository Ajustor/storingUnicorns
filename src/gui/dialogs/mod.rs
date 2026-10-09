//! Modal dialogs (connection form, confirmations, …).

pub mod connection;
pub mod structure;
pub mod transfer;

use super::app::App;
use super::history_popup::{HistoryAction, HistoryPopup};
use super::table_search::TableSearch;
use super::tabs::console::RunKind;
use super::tabs::{CloseRisk, TabId, TabKind};
use super::worker::Event;
use crate::engine::ops::transfer::qualified;
use connection::{ConnectionForm, FormAction};
use structure::StructureDialog;
use transfer::BatchDialog;

pub enum Dialog {
    Connection(Box<ConnectionForm>),
    /// Delete the connection with this name.
    ConfirmDeleteConnection(String),
    /// Query history (Ctrl+Alt+E) of the active console.
    History(HistoryPopup),
    /// Table search (Ctrl+N) over every open connection.
    TableSearch(TableSearch),
    /// "Abandonner les modifications ?" before data tab `TabId` changes page.
    DiscardEdits(TabId),
    /// Close tab `id`, which would lose `risk`.
    CloseTab {
        id: TabId,
        risk: CloseRisk,
    },
    /// Run console `id` again although its unpinned results (replaced by
    /// the run) have pending edits.
    ConfirmRerun {
        id: TabId,
        kind: RunKind,
    },
    /// Quit although this many tabs have pending edits.
    ConfirmQuit(usize),
    /// Columns of one table of one connection.
    Structure(StructureDialog),
    /// Empty these (qualified) tables of `connection`.
    ConfirmTruncate {
        connection: String,
        tables: Vec<String>,
    },
    /// Batch export / import / truncate on one connection.
    Batch(BatchDialog),
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut dialog) = app.dialog.take() else {
        return;
    };
    let mut keep = true;
    let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
        ui.set_min_width(420.0);
        match &mut dialog {
            Dialog::Connection(form) => match form.ui(ui) {
                FormAction::None => {}
                FormAction::Cancel => keep = false,
                FormAction::Test => match form.validate(&app.config.connections) {
                    Ok(cfg) => {
                        form.error = None;
                        form.testing = true;
                        form.test_result = None;
                        app.worker.test_connection(cfg);
                    }
                    Err(e) => form.error = Some(e),
                },
                FormAction::Save => match form.validate(&app.config.connections) {
                    Ok(cfg) => {
                        app.save_connection(form.original_name.clone(), cfg);
                        keep = false;
                    }
                    Err(e) => form.error = Some(e),
                },
            },
            Dialog::History(popup) => {
                ui.set_min_width(640.0);
                if let HistoryAction::Insert(sql) = popup.ui(ui, &app.history) {
                    if let Some(tab) = app.tabs.active_mut() {
                        if let TabKind::Console(c) = &mut tab.kind {
                            c.insert(ui.ctx(), tab.id, &sql);
                        }
                    }
                    keep = false;
                }
            }
            Dialog::TableSearch(search) => {
                ui.set_min_width(520.0);
                let color_of = |name: &str| app.color_of(name);
                if let Some((connection, schema, table)) = search.ui(ui, &color_of) {
                    let quotes = app.sessions.get(&connection).map(|s| s.quotes);
                    if let Some(quotes) = quotes {
                        app.open_data(&connection, &qualified(&schema, &table, quotes), &table);
                    }
                    keep = false;
                }
            }
            Dialog::DiscardEdits(id) => {
                let id = *id;
                ui.heading("Abandonner les modifications ?");
                ui.label("Les modifications en attente seront perdues.");
                ui.add_space(8.0);
                let mut answer = None;
                ui.horizontal(|ui| {
                    if ui.button("Submit").clicked() {
                        answer = Some(Discard::Submit);
                    }
                    if ui.button("Abandonner").clicked() {
                        answer = Some(Discard::Drop);
                    }
                    if ui.button("Annuler").clicked() {
                        answer = Some(Discard::Cancel);
                    }
                });
                if let Some(answer) = answer {
                    keep = false;
                    answer_discard(app, id, answer);
                }
            }
            Dialog::CloseTab { id, risk } => {
                let id = *id;
                if let Some(answer) = close_tab_ui(ui, *risk) {
                    keep = false;
                    match answer {
                        Discard::Submit => app.submit_and_close(id),
                        Discard::Drop => app.close_tab_id(id),
                        Discard::Cancel => {}
                    }
                }
            }
            Dialog::ConfirmRerun { id, kind } => {
                ui.heading("Abandonner les modifications ?");
                ui.label(
                    "L'exécution remplace les résultats non épinglés : \
                     leurs modifications en attente seront perdues.",
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Abandonner").clicked() {
                        app.run_console(*id, kind.clone());
                        keep = false;
                    }
                    if ui.button("Annuler").clicked() {
                        keep = false;
                    }
                });
            }
            Dialog::ConfirmQuit(n) => {
                ui.heading("Quitter ?");
                ui.label(format!("{n} onglet(s) ont des modifications non envoyées."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Fermer quand même").clicked() {
                        app.quit_confirmed = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        keep = false;
                    }
                    if ui.button("Annuler").clicked() {
                        keep = false;
                    }
                });
            }
            Dialog::Structure(d) => keep = structure::ui(app, ui, d),
            Dialog::ConfirmTruncate { connection, tables } => {
                keep = transfer::confirm_truncate_ui(app, ui, connection, tables)
            }
            Dialog::Batch(b) => keep = transfer::batch_ui(app, ui, b),
            Dialog::ConfirmDeleteConnection(name) => {
                ui.heading("Supprimer la connexion");
                ui.label(format!(
                    "Supprimer « {name} » ? Cette action est définitive."
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Supprimer").clicked() {
                        app.delete_connection(name);
                        keep = false;
                    }
                    if ui.button("Annuler").clicked() {
                        keep = false;
                    }
                });
            }
        }
    });
    // Escape or a click outside cancels the dialog.
    if modal.should_close() {
        if let (true, Dialog::DiscardEdits(id)) = (keep, &dialog) {
            answer_discard(app, *id, Discard::Cancel);
        }
        keep = false;
    }
    // A dialog may open another one in its place (batch → confirmation).
    if keep && app.dialog.is_none() {
        app.dialog = Some(dialog);
    }
}

/// Confirmation of the closing of a tab that would lose `risk`.
fn close_tab_ui(ui: &mut egui::Ui, risk: CloseRisk) -> Option<Discard> {
    let mut answer = None;
    match risk {
        CloseRisk::Console { text, edits } => {
            ui.heading("Fermer la console ?");
            if text {
                ui.label("Son contenu sera perdu.");
            }
            if edits {
                ui.label(
                    "Des résultats ont des modifications non envoyées : elles seront perdues.",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Fermer").clicked() {
                    answer = Some(Discard::Drop);
                }
                if ui.button("Annuler").clicked() {
                    answer = Some(Discard::Cancel);
                }
            });
        }
        CloseRisk::DataEdits | CloseRisk::None => {
            ui.heading("Abandonner les modifications ?");
            ui.label("Les modifications en attente de cet onglet seront perdues.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Submit")
                    .on_hover_text("Appliquer, puis fermer si tout a réussi")
                    .clicked()
                {
                    answer = Some(Discard::Submit);
                }
                if ui.button("Abandonner").clicked() {
                    answer = Some(Discard::Drop);
                }
                if ui.button("Annuler").clicked() {
                    answer = Some(Discard::Cancel);
                }
            });
        }
    }
    answer
}

/// Answers to "Abandonner les modifications ?".
enum Discard {
    Submit,
    Drop,
    Cancel,
}

fn answer_discard(app: &mut App, id: TabId, answer: Discard) {
    let action = app.tabs.find(id).and_then(|t| match &mut t.kind {
        TabKind::Data(d) => match answer {
            Discard::Submit => Some(d.submit_and_continue()),
            Discard::Drop => d.discard_and_continue(),
            Discard::Cancel => {
                d.confirm = None;
                None
            }
        },
        _ => None,
    });
    if let Some(action) = action {
        app.data_action(id, action);
    }
}

/// Outcomes addressed to an open dialog.
pub fn handle_event(app: &mut App, ev: Event) {
    if let Event::TestFinished(r) = ev {
        if let Some(Dialog::Connection(form)) = &mut app.dialog {
            form.testing = false;
            form.test_result = Some(r);
        }
    }
}
