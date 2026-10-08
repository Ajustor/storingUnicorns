mod edit_row;
mod export_import;
mod new_connection;
mod schema_dialog;

pub use edit_row::render_edit_row_dialog;
pub use export_import::{
    render_batch_export_dialog, render_batch_import_dialog, render_batch_truncate_dialog,
    render_delete_row_confirm, render_export_dialog, render_import_dialog, render_truncate_confirm,
};
pub use new_connection::render_new_connection_dialog;
pub use schema_dialog::{render_schema_dialog, SchemaAction};

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Helper to create a centered popup area
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
