//! Table data editor tab. Minimal body until plan 3b, Task 8.

use crate::gui::worker::Event;

pub struct DataTab {
    /// Qualified, quoted table name.
    pub table: String,
}

impl DataTab {
    pub fn new(table: String) -> Self {
        Self { table }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.heading(&self.table);
        ui.label(egui::RichText::new("Éditeur de données à venir.").weak());
    }

    /// Pages, counts and submits (Task 8).
    pub fn on_event(&mut self, _ev: Event) {}
}
