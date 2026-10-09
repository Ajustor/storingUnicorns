//! The result strip of a console: one tab per result and the "Sortie" log.

use egui::{Color32, RichText};
use egui_phosphor::regular as icon;

use crate::engine::ops::query::editable_table;
use crate::engine::sql::statements::extract_table_from_query;
use crate::gui::grid::{self, GridAction, GridOptions};
use crate::gui::rows::fixed_row;
use crate::gui::theme::{ACCENT, ERROR};

use super::{read_only_hint, ConsoleAction, ConsoleContext, ConsoleTab, ResultTab};

impl ConsoleTab {
    pub(super) fn results_area(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        cx: &ConsoleContext,
    ) -> Option<ConsoleAction> {
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (i, r) in self.results.iter_mut().enumerate() {
                let spacing = ui.spacing().item_spacing.x;
                ui.spacing_mut().item_spacing.x = 2.0;
                if r.connection != cx.connection {
                    // Read on the connection the console was bound to before.
                    let color = (cx.color_of)(&r.connection);
                    ui.label(
                        RichText::new(&r.connection)
                            .small()
                            .color(Color32::BLACK)
                            .background_color(color),
                    )
                    .on_hover_text("Connexion de ce résultat");
                }
                let label = format!("{} {}", icon::TABLE, r.title);
                if ui
                    .selectable_label(i == self.active_result, label)
                    .on_hover_text(&r.sql)
                    .clicked()
                {
                    self.active_result = i;
                }
                let pin = if r.pinned {
                    RichText::new(icon::PUSH_PIN).color(ACCENT)
                } else {
                    RichText::new(icon::PUSH_PIN).weak()
                };
                if ui
                    .small_button(pin)
                    .on_hover_text(if r.pinned {
                        "Désépingler"
                    } else {
                        "Épingler (gardé à la prochaine exécution)"
                    })
                    .clicked()
                {
                    r.pinned = !r.pinned;
                }
                if ui.small_button(icon::X).on_hover_text("Fermer").clicked() {
                    close = Some(i);
                }
                ui.spacing_mut().item_spacing.x = spacing;
                ui.separator();
            }
            let errors = self.log.iter().rev().take_while(|l| !l.ok).count();
            let label = if errors > 0 {
                RichText::new(format!("{} Sortie", icon::LIST_BULLETS)).color(ERROR)
            } else {
                RichText::new(format!("{} Sortie", icon::LIST_BULLETS))
            };
            if ui
                .selectable_label(self.active_result >= self.results.len(), label)
                .clicked()
            {
                self.active_result = self.results.len();
            }
        });
        if let Some(i) = close {
            self.results.remove(i);
            if self.active_result > i || self.active_result > self.results.len() {
                self.active_result = self.active_result.saturating_sub(1);
            }
            self.refresh_known_columns();
        }
        ui.separator();
        let active = self.active_result;
        let lock = self.result_lock(active, cx.running);
        match self.results.get_mut(active) {
            Some(r) => result_view(ui, id, active, r, lock),
            None => self.log_view(ui, id),
        }
    }

    /// "Sortie": every execution, newest last. Clicking a line searches its
    /// SQL in the history.
    fn log_view(&self, ui: &mut egui::Ui, id: egui::Id) -> Option<ConsoleAction> {
        if self.log.is_empty() {
            ui.label(RichText::new("Exécutez une requête (Ctrl+Entrée) ou le script (F5).").weak());
            return None;
        }
        let mut action = None;
        // Only the visible lines are laid out.
        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt(id.with("log"))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show_rows(ui, row_height, self.log.len(), |ui, range| {
                for i in range {
                    let line = &self.log[i];
                    fixed_row(ui, row_height, i, |ui, _| {
                        ui.label(RichText::new(&line.time).weak());
                        let msg = RichText::new(&line.message);
                        ui.label(if line.ok { msg } else { msg.color(ERROR) });
                        if !line.sql.is_empty() {
                            let r = ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(&line.preview).monospace().weak(),
                                    )
                                    .truncate()
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Chercher dans l'historique");
                            if r.clicked() {
                                action = Some(ConsoleAction::History(line.sql.clone()));
                            }
                        }
                    });
                }
            });
        action
    }
}

/// Grid of result tab `r` (at `index`); its Submit becomes a console action.
fn result_view(
    ui: &mut egui::Ui,
    id: egui::Id,
    index: usize,
    r: &mut ResultTab,
    lock: Option<&str>,
) -> Option<ConsoleAction> {
    if let Some(e) = &r.error {
        ui.colored_label(
            ERROR,
            format!(
                "{} Submit annulé (transaction annulée) : {e}",
                icon::WARNING
            ),
        );
    }
    if let Some(hint) = read_only_hint(&r.sql, &r.result) {
        ui.label(RichText::new(format!("{} {hint}", icon::LOCK)).weak());
    }
    if let (Some(hint), true) = (lock, r.editable) {
        ui.label(RichText::new(format!("{} {hint}", icon::HOURGLASS)).weak());
    }
    let action = grid::show(
        ui,
        id.with(("result", r.id)),
        &mut r.grid,
        GridOptions {
            result: &r.result,
            editable: r.editable && lock.is_none(),
            server_sort: false,
            sort_indicator: None,
        },
    );
    if action == GridAction::Export {
        return Some(ConsoleAction::Export {
            connection: r.connection.clone(),
            result: r.result.clone(),
            table: extract_table_from_query(&r.sql).unwrap_or_else(|| "table".into()),
        });
    }
    if action != GridAction::Submit || r.grid.edits.is_empty() {
        return None;
    }
    let table = editable_table(&r.sql, &r.result)?;
    let columns = r.result.columns.clone();
    Some(ConsoleAction::Submit {
        result_index: index,
        table,
        changes: r.grid.edits.to_row_changes(&r.result.rows),
        columns,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use chrono::Local;

    use super::*;
    use crate::gui::tabs::console::LogLine;

    /// "Sortie" with 2 000 lines, idle frames.
    /// `cargo test --release -- --ignored bench_console_log --nocapture`
    #[test]
    #[ignore]
    fn bench_console_log() {
        let mut c = ConsoleTab::new(String::new(), 0);
        for i in 0..2000 {
            c.push_log(LogLine::new(
                Local::now(),
                format!("SELECT * FROM t WHERE id = {i}"),
                "1 ligne en 3 ms".into(),
                true,
            ));
        }
        let ctx = egui::Context::default();
        let frame = || {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 400.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    std::hint::black_box(c.log_view(ui, egui::Id::new("bench")));
                });
            });
            std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
        };
        for _ in 0..3 {
            frame();
        }
        let n = 30;
        let t = Instant::now();
        for _ in 0..n {
            frame();
        }
        eprintln!(
            "console log, 2 000 lines: {:.2} ms/frame",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }
}
