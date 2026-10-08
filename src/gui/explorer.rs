//! Database explorer (left panel), filled in by plan 3b, Task 5.

use egui::RichText;

use super::app::App;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Explorateur");
    ui.separator();
    for c in &app.config.connections {
        ui.label(RichText::new(&c.name));
    }
}
