//! `CREATE TABLE` of a table: read-only highlighted SQL, copy and refresh.

use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Galley, RichText};
use egui_phosphor::regular as icon;

use crate::gui::editor::highlight_job;
use crate::gui::theme::ERROR;
use crate::gui::worker::Event;

pub struct DdlTab {
    /// Qualified, quoted table name.
    pub table: String,
    /// `None` while loading.
    pub ddl: Option<Result<String, String>>,
    /// Highlighted layout of the DDL for (dark mode, font size).
    job: Option<(bool, f32, LayoutJob)>,
}

/// What the tab asks the app to do.
pub enum DdlAction {
    Refresh,
}

impl DdlTab {
    pub fn new(table: String) -> Self {
        Self {
            table,
            ddl: None,
            job: None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, connected: bool) -> Option<DdlAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(&self.table).strong());
            let loading = self.ddl.is_none();
            if ui
                .add_enabled(
                    connected && !loading,
                    egui::Button::new(icon::ARROWS_CLOCKWISE),
                )
                .on_hover_text("Rafraîchir")
                .clicked()
            {
                action = Some(DdlAction::Refresh);
            }
            if let Some(Ok(sql)) = &self.ddl {
                if ui.button(format!("{} Copier", icon::COPY)).clicked() {
                    ui.ctx().copy_text(sql.clone());
                }
            }
        });
        ui.separator();
        match &self.ddl {
            None => {
                ui.spinner();
            }
            Some(Err(e)) => {
                ui.colored_label(ERROR, e);
            }
            Some(Ok(sql)) => {
                let dark = ui.visuals().dark_mode;
                let size = egui::TextStyle::Monospace.resolve(ui.style()).size;
                if !matches!(&self.job, Some((d, s, _)) if *d == dark && *s == size) {
                    self.job = Some((dark, size, highlight_job(sql, &[], dark, size)));
                }
                let job = self.job.as_ref().map(|(_, _, j)| j);
                let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| -> Arc<Galley> {
                    let mut job = match job {
                        Some(j) if j.text == text => j.clone(),
                        _ => highlight_job(text, &[], dark, size),
                    };
                    job.wrap.max_width = wrap_width;
                    ui.fonts(|f| f.layout_job(job))
                };
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut sql.as_str())
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .layouter(&mut layouter),
                        );
                    });
            }
        }
        action
    }

    /// Forget the DDL before reloading it.
    pub fn reload(&mut self) {
        self.ddl = None;
        self.job = None;
    }

    pub fn on_event(&mut self, ev: Event) {
        if let Event::Ddl { outcome, .. } = ev {
            self.ddl = Some(outcome);
            self.job = None;
        }
    }
}
