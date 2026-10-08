//! Right panel showing the focused cell in full (plan 3b, Task 9).

use super::app::App;

pub fn show(_app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Valeur");
    ui.separator();
    ui.label(egui::RichText::new("Sélectionnez une cellule.").weak());
}
