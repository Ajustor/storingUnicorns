use ratatui::{
    style::{Color, Modifier, Style},
    Frame,
};

/// Common panel border style based on active state
pub fn panel_style(active: bool) -> Style {
    if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    }
}

/// Highlight style for selected items
pub fn highlight_style() -> Style {
    Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// Selection style for text selection
pub fn selection_style() -> Style {
    Style::default().bg(Color::Blue).fg(Color::White)
}

/// Draw a software cursor (reverse-video cell) instead of using the terminal hardware cursor.
/// This avoids blink-timer resets caused by continuous 30fps redraws.
pub fn draw_cursor(frame: &mut Frame, x: u16, y: u16) {
    let buf = frame.buffer_mut();
    let area = buf.area;
    if x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height {
        let cell = &mut buf[(x, y)];
        cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
    }
}
