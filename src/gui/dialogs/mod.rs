//! Modal dialogs (connection form, confirmations, …).

pub mod connection;

use super::app::App;
use super::history_popup::{HistoryAction, HistoryPopup};
use super::tabs::TabKind;
use super::worker::Event;
use connection::{ConnectionForm, FormAction};

pub enum Dialog {
    Connection(Box<ConnectionForm>),
    /// Delete the connection with this name.
    ConfirmDeleteConnection(String),
    /// Query history (Ctrl+Alt+E) of the active console.
    History(HistoryPopup),
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
        keep = false;
    }
    if keep {
        app.dialog = Some(dialog);
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
