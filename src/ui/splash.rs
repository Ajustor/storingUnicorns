use std::time::Instant;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};
use tachyonfx::{fx, Duration, Effect, EffectRenderer, Interpolation, Shader};

const UNICORN_ART: &str = r"
              \
               \
                 \\
                  \\
                   >\/7
               _.-(6'  \
              (=___._/` \
                   )  \ |
                  /   / |
                 /    > /
                j    < _\
            _.-' :      ``.
            \ r=._\        `.
           <`\_  \         .`-.
            \ r-7  `-. ._  ' .  `\
             \`,      `-.`7  7)   )
              \/         \|  \'  / `-._
                         ||    .'
                          \\  (
                           >\  >
                       ,.-' >.'
                      <.'_.''
                        <'";

const APP_NAME: &str = "storingUnicorns";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the splash screen animation.
/// Shows a unicorn ASCII art with the app name, using coalesce effect.
/// Exits after the animation completes or on any keypress.
pub fn run_splash_screen<B: Backend>(terminal: &mut Terminal<B>) -> Result<()> {
    // Phase 1: coalesce in (text appears from scrambled chars)
    let mut coalesce_effect = fx::coalesce((1800, Interpolation::CubicOut));
    // Phase 2: dissolve out (text disappears)
    let mut fade_out_effect: Option<Effect> = None;

    let mut last_frame = Instant::now();
    let start = Instant::now();

    loop {
        let now = Instant::now();
        let elapsed_since_last = now.duration_since(last_frame);
        let tick = Duration::from_millis(elapsed_since_last.as_millis() as u32);
        last_frame = now;

        terminal.draw(|f| {
            let area = f.area();
            render_splash_content(f, area);

            if let Some(ref mut fade_out) = fade_out_effect {
                f.render_effect(fade_out, area, tick);
            } else {
                f.render_effect(&mut coalesce_effect, area, tick);
            }
        })?;

        // Check if fade-out is done
        if fade_out_effect.as_ref().is_some_and(|e| e.done()) {
            break;
        }

        // After coalesce is done + brief hold, start fade out
        if coalesce_effect.done() && fade_out_effect.is_none() {
            let total_ms = now.duration_since(start).as_millis();
            if total_ms > 2800 {
                fade_out_effect = Some(fx::dissolve((600, Interpolation::CubicIn)));
            }
        }

        // Poll for key events (skip splash on any key)
        if event::poll(std::time::Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Render the splash screen content (unicorn + app name) centered on screen.
fn render_splash_content(frame: &mut ratatui::Frame, area: Rect) {
    let art_lines: Vec<&str> = UNICORN_ART.lines().collect();
    let art_height = art_lines.len() as u16;
    let name_height = 2; // app name + version
    let total_height = art_height + name_height + 1; // +1 spacing

    // Center vertically
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0), // top padding
            Constraint::Length(total_height),
            Constraint::Min(0), // bottom padding
        ])
        .split(area);

    let center_area = vertical[1];

    // Split center area into art + spacing + name
    let content_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(art_height),
            Constraint::Length(1), // spacing
            Constraint::Length(name_height),
        ])
        .split(center_area);

    // Render unicorn art (centered)
    let art_styled: Vec<Line> = art_lines
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(Color::Magenta))))
        .collect();

    let art_paragraph = Paragraph::new(art_styled).alignment(Alignment::Center);
    frame.render_widget(art_paragraph, content_chunks[0]);

    // Render app name
    let name_line = Line::from(vec![Span::styled(
        APP_NAME,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]);
    let version_line = Line::from(vec![Span::styled(
        format!("v{}", APP_VERSION),
        Style::default().fg(Color::DarkGray),
    )]);

    let name_paragraph = Paragraph::new(vec![name_line, version_line]).alignment(Alignment::Center);
    frame.render_widget(name_paragraph, content_chunks[2]);
}
