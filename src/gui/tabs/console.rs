//! SQL console tab. Minimal body until plan 3b, Task 6 (editor, result
//! tabs, output log).

use crate::gui::worker::Event;

use super::TabId;

pub struct ConsoleTab {
    pub query: String,
    /// Cursor position, as persisted in `QueryTab::cursor_position`.
    pub cursor: usize,
}

impl ConsoleTab {
    pub fn new(query: String, cursor: usize) -> Self {
        Self { query, cursor }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, id: TabId) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_sized(
                    ui.available_size(),
                    egui::TextEdit::multiline(&mut self.query)
                        .id(egui::Id::new(("console", id)))
                        .code_editor()
                        .hint_text("SELECT …"),
                );
            });
    }

    /// Script outcomes (Task 6).
    pub fn on_event(&mut self, _ev: Event) {}
}
