//! Modal dialogs (connection form, confirmations, …).

use super::app::App;
use super::worker::Event;

pub enum Dialog {}

pub fn show(app: &mut App, _ctx: &egui::Context) {
    let Some(dialog) = app.dialog.take() else {
        return;
    };
    match dialog {}
}

/// Outcomes addressed to an open dialog.
pub fn handle_event(_app: &mut App, _ev: Event) {}
