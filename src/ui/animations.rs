use std::time::Instant;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Color,
    Frame,
};
use tachyonfx::{fx, Duration, Effect, EffectRenderer, Interpolation, Shader};

use crate::services::AppState;

/// Number of panels that get animated
const NUM_PANELS: usize = 6;

/// Panel animation state for the startup reveal effect.
/// Each UI panel appears one-by-one in a random order.
pub struct PanelAnimations {
    /// One effect per panel, indexed by PanelSlot
    effects: Vec<Effect>,
    /// Order in which panels appear (shuffled indices into PanelSlot)
    _order: Vec<usize>,
    /// Last frame timestamp for computing tick deltas
    last_frame: Instant,
}

/// Identifies each animatable panel slot.
/// 0=Connections, 1=Tables, 2=QueryEditor, 3=Results, 4=StatusBar, 5=HelpBar
const _PANEL_CONNECTIONS: usize = 0;
const _PANEL_TABLES: usize = 1;
const _PANEL_QUERY_EDITOR: usize = 2;
const _PANEL_RESULTS: usize = 3;
const _PANEL_STATUS_BAR: usize = 4;
const _PANEL_HELP_BAR: usize = 5;

impl PanelAnimations {
    /// Create a new set of panel animations with randomized reveal order.
    pub fn new() -> Self {
        // Determine random order using a simple time-seeded shuffle
        let mut order: Vec<usize> = (0..NUM_PANELS).collect();
        simple_shuffle(&mut order);

        // Create staggered effects: each panel gets a delay based on its
        // position in the shuffled order, then a reveal effect.
        let delay_per_panel_ms: u32 = 200;
        let reveal_ms: u32 = 600;

        let effects: Vec<Effect> = (0..NUM_PANELS)
            .map(|panel_idx| {
                // Find this panel's position in the reveal order
                let position = order.iter().position(|&o| o == panel_idx).unwrap() as u32;
                let delay_ms = position * delay_per_panel_ms;

                // Use prolong_start to keep the panel hidden (in initial effect state)
                // during the delay, then reveal with coalesce
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

    /// Returns true if all panel animations have completed.
    pub fn all_done(&self) -> bool {
        self.effects.iter().all(|e| e.done())
    }

    /// Returns true if any animation is still running (needs continuous redraw).
    pub fn any_running(&self) -> bool {
        self.effects.iter().any(|e| e.running())
    }

    /// Apply all running panel effects to the frame.
    /// This must be called AFTER render_ui has rendered all widgets.
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

/// Compute the panel areas matching the layout in render_ui.
/// Returns areas in PanelSlot order: [Connections, Tables, QueryEditor, Results, StatusBar, HelpBar]
fn compute_panel_areas(size: Rect, state: &AppState) -> Vec<Rect> {
    // Main vertical split: content + status bar + help bar
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),   // Content
            Constraint::Length(1), // Status bar
            Constraint::Length(1), // Help bar
        ])
        .split(size);

    // Content area: left sidebar + right main area
    let sidebar_constraint = Constraint::Percentage(state.sidebar_width);
    let main_constraint = Constraint::Percentage(100 - state.sidebar_width);

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([sidebar_constraint, main_constraint])
        .split(main_chunks[0]);

    // Left sidebar: connections + tables
    let sidebar_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(content_chunks[0]);

    // Right side: query editor + results
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
        sidebar_chunks[0], // Connections
        sidebar_chunks[1], // Tables
        right_chunks.0,    // QueryEditor
        results_area,      // Results
        main_chunks[1],    // StatusBar
        main_chunks[2],    // HelpBar
    ]
}

/// Simple Fisher-Yates shuffle using a time-based seed.
fn simple_shuffle(slice: &mut [usize]) {
    let mut seed: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    for i in (1..slice.len()).rev() {
        // Simple LCG random
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (seed >> 33) as usize % (i + 1);
        slice.swap(i, j);
    }
}
