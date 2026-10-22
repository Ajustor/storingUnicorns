//! Sizing and rows shared by the modal lists (history, table search) and
//! the combo boxes: widths follow the content instead of egui's narrow
//! defaults.

use std::sync::Arc;

use egui::{Galley, Rect, Response, Sense, TextStyle, TextWrapMode, WidgetText};

/// Widest a combo box grows to fit its longest label.
pub const COMBO_MAX: f32 = 360.0;

/// Width of a modal list (history, table search): comfortable, never wider
/// than 900 points nor than 80 % of the window, and at least 420 points
/// when the window allows it.
pub fn modal_width(screen_width: f32) -> f32 {
    let floor = 420.0f32.min(screen_width - 32.0).max(0.0);
    (screen_width * 0.8).min(900.0).max(floor)
}

/// Height of the list area of a modal list: half the window, between 200
/// and 420 points.
pub fn modal_list_height(screen_height: f32) -> f32 {
    (screen_height * 0.5).clamp(200.0, 420.0)
}

/// `extra` (padding, icon) plus the widest of `text_widths`, kept within
/// `min..=max`.
pub fn fit_width(
    text_widths: impl IntoIterator<Item = f32>,
    extra: f32,
    min: f32,
    max: f32,
) -> f32 {
    let widest = text_widths.into_iter().fold(0.0f32, f32::max);
    (widest + extra).clamp(min, max.max(min))
}

/// Width for a `ComboBox` showing `labels` (the selected text and the
/// items), so that the longest fits on one line in the button and in the
/// list, up to [`COMBO_MAX`]; egui's `combo_width` at least.
pub fn combo_width<S: AsRef<str>>(ui: &egui::Ui, labels: impl IntoIterator<Item = S>) -> f32 {
    let font = TextStyle::Button.resolve(ui.style());
    let widths: Vec<f32> = labels
        .into_iter()
        .map(|l| {
            ui.fonts(|f| {
                f.layout_no_wrap(l.as_ref().to_owned(), font.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            })
        })
        .collect();
    let s = ui.spacing();
    // The button's padding and arrow, and the list's margins around items.
    let extra = s.icon_width + s.icon_spacing + 4.0 * s.button_padding.x + 2.0 * s.item_spacing.x;
    fit_width(widths, extra, s.combo_width, COMBO_MAX)
}

/// Widths of the main text and of the side text (e.g. a connection name)
/// sharing a row of `total` with `gap` between them: the side text keeps
/// its width up to 40 % of the row, the main text gets the rest.
pub fn split_row(total: f32, side: f32, gap: f32) -> (f32, f32) {
    if side <= 0.0 {
        return (total, 0.0);
    }
    let side = side.min(total * 0.4);
    ((total - side - gap).max(0.0), side)
}

/// `text` on one line, elided with "…" past `width`.
pub fn one_line(
    ui: &egui::Ui,
    text: impl Into<WidgetText>,
    width: f32,
    style: TextStyle,
) -> Arc<Galley> {
    text.into()
        .into_galley(ui, Some(TextWrapMode::Truncate), width.max(0.0), style)
}

/// Full-width clickable row of `height`: a soft background and an accent
/// bar on the left when `selected` (the text keeps its normal colours, so
/// several lines stay readable), a hover background otherwise. Returns its
/// response and its rect, for the caller to paint the content.
pub fn selectable_row(ui: &mut egui::Ui, selected: bool, height: f32) -> (Response, Rect) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let radius = visuals.widgets.hovered.corner_radius;
        if selected {
            ui.painter()
                .rect_filled(rect, radius, visuals.selection.bg_fill.gamma_multiply(0.35));
            let bar = Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
            ui.painter()
                .rect_filled(bar, radius, visuals.selection.stroke.color);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, radius, visuals.widgets.hovered.weak_bg_fill);
        }
    }
    (response, rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_width_is_comfortable_but_bounded() {
        assert_eq!(modal_width(1920.0), 900.0, "capped on large windows");
        assert_eq!(modal_width(1000.0), 800.0, "80 % of the window");
        assert_eq!(modal_width(500.0), 420.0, "readable on small windows");
        assert_eq!(modal_width(300.0), 268.0, "never wider than the window");
    }

    #[test]
    fn modal_list_height_follows_the_window() {
        assert_eq!(modal_list_height(1200.0), 420.0);
        assert_eq!(modal_list_height(600.0), 300.0);
        assert_eq!(modal_list_height(200.0), 200.0);
    }

    #[test]
    fn fit_width_takes_the_widest_label_within_bounds() {
        assert_eq!(fit_width([40.0, 120.0, 80.0], 30.0, 100.0, 360.0), 150.0);
        assert_eq!(fit_width([10.0], 30.0, 100.0, 360.0), 100.0, "minimum");
        assert_eq!(fit_width([900.0], 30.0, 100.0, 360.0), 360.0, "maximum");
        assert_eq!(fit_width([], 30.0, 100.0, 360.0), 100.0, "no label");
    }

    #[test]
    fn split_row_gives_the_name_the_room_the_side_text_does_not_need() {
        assert_eq!(split_row(600.0, 100.0, 8.0), (492.0, 100.0));
        // A long side text is capped at 40 % of the row.
        assert_eq!(split_row(500.0, 400.0, 0.0), (300.0, 200.0));
        assert_eq!(split_row(100.0, 0.0, 8.0), (100.0, 0.0), "no side text");
    }

    #[test]
    fn combo_width_fits_the_longest_label() {
        let ctx = egui::Context::default();
        let mut widths = (0.0, 0.0, 0.0);
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let font = TextStyle::Button.resolve(ui.style());
                let long = "Azure AD interactif (navigateur)";
                let text = ui.fonts(|f| {
                    f.layout_no_wrap(long.into(), font, egui::Color32::WHITE)
                        .size()
                        .x
                });
                widths = (
                    combo_width(ui, ["SQL Auth", long, "Azure CLI"]),
                    combo_width(ui, ["a"]),
                    text,
                );
            });
        });
        let (fitted, short, text) = widths;
        assert!(fitted > text + 20.0, "{fitted} leaves room for the arrow");
        assert!(fitted <= COMBO_MAX);
        assert_eq!(
            short,
            egui::Spacing::default().combo_width,
            "egui's default"
        );
    }
}
