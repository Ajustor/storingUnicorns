//! Fixed-height rows for `ScrollArea::show_rows`, which only lays out the
//! visible rows of a long list but needs them all to be the same height.

use std::hash::Hash;

/// Reserve a full-width row `height` high and draw `add` in it, left to
/// right and vertically centred. The space is reserved first, so the row
/// keeps its height whatever it contains; `salt` keeps the ids of its
/// widgets stable while the list scrolls.
pub fn fixed_row<R>(
    ui: &mut egui::Ui,
    height: f32,
    salt: impl Hash,
    add: impl FnOnce(&mut egui::Ui, egui::Rect) -> R,
) -> R {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(salt)
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    add(&mut child, rect)
}
