use std::time::Instant;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    Frame,
};
use tachyonfx::{fx, Duration, Effect, EffectRenderer, HslConvertable, Interpolation, Shader};

use crate::services::{ActivePanel, AppState, DialogMode};

// ─── Startup panel reveal ──────────────────────────────────────────────────

/// Number of panels that get animated
const NUM_PANELS: usize = 6;

/// Panel animation state for the startup reveal effect.
/// Each UI panel appears one-by-one in a random order.
pub struct PanelAnimations {
    effects: Vec<Effect>,
    _order: Vec<usize>,
    last_frame: Instant,
}

impl PanelAnimations {
    /// Create a new set of panel animations with randomized reveal order.
    pub fn new() -> Self {
        let mut order: Vec<usize> = (0..NUM_PANELS).collect();
        simple_shuffle(&mut order);

        let delay_per_panel_ms: u32 = 200;
        let reveal_ms: u32 = 600;

        let effects: Vec<Effect> = (0..NUM_PANELS)
            .map(|panel_idx| {
                let position = order.iter().position(|&o| o == panel_idx).unwrap() as u32;
                let delay_ms = position * delay_per_panel_ms;

                fx::prolong_start(
                    delay_ms,
                    fx::sweep_in(
                        match panel_idx % 4 {
                            0 => fx::Direction::LeftToRight,
                            1 => fx::Direction::UpToDown,
                            2 => fx::Direction::DownToUp,
                            _ => fx::Direction::RightToLeft,
                        },
                        15,
                        3,
                        Color::Black,
                        (reveal_ms, Interpolation::CubicOut),
                    ),
                )
            })
            .collect();

        Self {
            effects,
            _order: order,
            last_frame: Instant::now(),
        }
    }

    pub fn all_done(&self) -> bool {
        self.effects.iter().all(|e| e.done())
    }

    #[allow(dead_code)]
    pub fn any_running(&self) -> bool {
        self.effects.iter().any(|e| e.running())
    }

    pub fn apply(&mut self, frame: &mut Frame, state: &AppState) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame);
        let tick = Duration::from_millis(elapsed.as_millis() as u32);
        self.last_frame = now;

        let areas = compute_panel_areas(frame.area(), state);

        for (i, effect) in self.effects.iter_mut().enumerate() {
            if effect.running() {
                if let Some(&area) = areas.get(i) {
                    if area.width > 0 && area.height > 0 {
                        frame.render_effect(effect, area, tick);
                    }
                }
            }
        }
    }
}

// ─── Modal open animation ──────────────────────────────────────────────────

/// Tracks a one-shot animation when a modal opens.
pub struct ModalAnimation {
    effect: Effect,
    last_frame: Instant,
}

impl ModalAnimation {
    /// Create a new modal open animation for the given dialog mode.
    pub fn new(_dialog_mode: DialogMode) -> Self {
        let effect = fx::coalesce((400, Interpolation::CubicOut));
        Self {
            effect,
            last_frame: Instant::now(),
        }
    }

    #[allow(dead_code)]
    pub fn done(&self) -> bool {
        self.effect.done()
    }

    /// Apply the modal animation to the frame. Must be called AFTER modal is rendered.
    pub fn apply(&mut self, frame: &mut Frame, area: Rect) {
        if self.effect.done() {
            return;
        }
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_frame);
        let tick = Duration::from_millis(elapsed.as_millis() as u32);
        self.last_frame = now;

        if area.width > 0 && area.height > 0 {
            frame.render_effect(&mut self.effect, area, tick);
        }
    }
}

// ─── Neon border effect ────────────────────────────────────────────────────

/// Returns true for box-drawing / border characters that should receive neon colouring.
fn is_border_char(s: &str) -> bool {
    matches!(
        s,
        "│" | "─" | "┐" | "┘" | "└" | "┌" | "┬" | "┴" | "├" | "┤" | "┼"
            | "═" | "║" | "╔" | "╗" | "╚" | "╝" | "╠" | "╣" | "╦" | "╩" | "╬"
            | "╭" | "╮" | "╯" | "╰"
            | "▏" | "▕" | "▁" | "▔"
    )
}

/// Pulse speed: controls how fast the neon breathes (radians per millisecond)
const NEON_PULSE_SPEED: f32 = 0.003;

/// Render a pulsing blue neon border effect around the given rectangle.
/// The entire border breathes in and out uniformly.
pub fn render_neon_border(frame: &mut Frame, area: Rect, elapsed_ms: u128) {
    if area.width < 2 || area.height < 2 {
        return;
    }

    let buf = frame.buffer_mut();

    // Smooth sine pulse: 0.0 to 1.0
    let pulse = ((elapsed_ms as f32 * NEON_PULSE_SPEED).sin() + 1.0) * 0.5;

    // Lightness oscillates between dim (25) and bright (55)
    let lightness = 25.0 + pulse * 30.0;
    // Saturation oscillates between subtle (40) and vivid (85)
    let saturation = 40.0 + pulse * 45.0;

    let color = Color::from_hsl(210.0, saturation, lightness);

    let x0 = area.x;
    let x1 = area.x + area.width.saturating_sub(1);
    let y0 = area.y;
    let y1 = area.y + area.height.saturating_sub(1);

    // Top & bottom edges — only color box-drawing characters
    for x in x0..=x1 {
        if is_border_char(buf[(x, y0)].symbol()) {
            buf[(x, y0)].set_fg(color);
        }
        if y1 > y0 && is_border_char(buf[(x, y1)].symbol()) {
            buf[(x, y1)].set_fg(color);
        }
    }
    // Left & right edges (excluding corners already done)
    for y in (y0 + 1)..y1 {
        if is_border_char(buf[(x0, y)].symbol()) {
            buf[(x0, y)].set_fg(color);
        }
        if x1 > x0 && is_border_char(buf[(x1, y)].symbol()) {
            buf[(x1, y)].set_fg(color);
        }
    }
}

// ─── Utilities ─────────────────────────────────────────────────────────────

/// Compute the area of the currently open modal dialog.
pub fn compute_modal_area(frame_area: Rect, dialog_mode: DialogMode) -> Rect {
    use crate::ui::modals::centered_rect;
    match dialog_mode {
        DialogMode::NewConnection | DialogMode::EditConnection => centered_rect(60, 70, frame_area),
        DialogMode::EditRow | DialogMode::AddRow => centered_rect(70, 80, frame_area),
        DialogMode::SchemaModify => centered_rect(70, 80, frame_area),
        DialogMode::Export | DialogMode::Import => centered_rect(50, 30, frame_area),
        DialogMode::DeleteConfirm => centered_rect(40, 20, frame_area),
        DialogMode::None => Rect::default(),
    }
}

/// Compute the area of the active panel given the current state.
pub fn compute_active_panel_area(frame_area: Rect, state: &AppState) -> Rect {
    let areas = compute_panel_areas(frame_area, state);
    match state.active_panel {
        ActivePanel::Connections => areas[0],
        ActivePanel::Tables => areas[1],
        ActivePanel::QueryEditor => areas[2],
        ActivePanel::Results => areas[3],
    }
}

/// Compute the panel areas matching the layout in render_ui.
/// Returns: [Connections, Tables, QueryEditor, Results, StatusBar, HelpBar]
fn compute_panel_areas(size: Rect, state: &AppState) -> Vec<Rect> {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

    let sidebar_constraint = Constraint::Percentage(state.sidebar_width);
    let main_constraint = Constraint::Percentage(100 - state.sidebar_width);

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([sidebar_constraint, main_constraint])
        .split(main_chunks[0]);

    let sidebar_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(content_chunks[0]);

    let right_chunks = if state.should_show_results() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(state.query_editor_height),
                Constraint::Percentage(100 - state.query_editor_height),
            ])
            .split(content_chunks[1]);
        (chunks[0], Some(chunks[1]))
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(100)])
            .split(content_chunks[1]);
        (chunks[0], None)
    };

    let results_area = right_chunks.1.unwrap_or(Rect::default());

    vec![
        sidebar_chunks[0],
        sidebar_chunks[1],
        right_chunks.0,
        results_area,
        main_chunks[1],
        main_chunks[2],
    ]
}

/// Simple Fisher-Yates shuffle using a time-based seed.
fn simple_shuffle(slice: &mut [usize]) {
    let mut seed: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    for i in (1..slice.len()).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (seed >> 33) as usize % (i + 1);
        slice.swap(i, j);
    }
}
