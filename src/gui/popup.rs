//! Sizing and rows shared by the modal lists (history, table search).

use std::sync::Arc;

use egui::{Galley, Rect, Response, Sense, TextStyle, TextWrapMode, WidgetText};

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
}
