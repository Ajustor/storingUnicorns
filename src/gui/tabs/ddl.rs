//! `CREATE TABLE` of a table, read only. Minimal body until plan 3b, Task 9.

use crate::gui::theme::ERROR;
use crate::gui::worker::Event;

pub struct DdlTab {
    /// Qualified, quoted table name.
    pub table: String,
    /// `None` while loading.
    pub ddl: Option<Result<String, String>>,
}

impl DdlTab {
    pub fn new(table: String) -> Self {
        Self { table, ddl: None }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(&self.table).strong());
        match &self.ddl {
            None => {
                ui.spinner();
            }
            Some(Err(e)) => {
                ui.colored_label(ERROR, e);
            }
            Some(Ok(sql)) => {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(sql).monospace()).selectable(true),
                        );
                    });
            }
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        if let Event::Ddl { outcome, .. } = ev {
            self.ddl = Some(outcome);
        }
    }
}
