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
use tachyonfx::{fx, Duration, Effect, EffectRenderer, HslConvertable, Interpolation, Shader};

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
            let elapsed_ms = now.duration_since(start).as_millis();
            render_splash_content(f, area, elapsed_ms);

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
/// `elapsed_ms` drives the rainbow neon cycling on the unicorn art.
fn render_splash_content(frame: &mut ratatui::Frame, area: Rect, elapsed_ms: u128) {
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

    // Render unicorn art with rainbow neon effect
    // Each non-space character gets a hue based on its position + time
    let base_hue = (elapsed_ms as f32 * 0.15) % 360.0;
    let mut char_index: usize = 0;

    let art_styled: Vec<Line> = art_lines
        .iter()
        .map(|line| {
            let spans: Vec<Span> = line
                .chars()
                .map(|ch| {
                    if ch != ' ' {
                        let hue = (base_hue + char_index as f32 * 8.0) % 360.0;
                        // Pulsating brightness for neon glow effect
                        let pulse = ((elapsed_ms as f32 * 0.003 + char_index as f32 * 0.1).sin()
                            + 1.0)
                            * 0.5;
                        let lightness = 50.0 + pulse * 25.0;
                        let color = Color::from_hsl(hue, 100.0, lightness);
                        char_index += 1;
                        Span::styled(
                            ch.to_string(),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Span::raw(ch.to_string())
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect();

    let art_paragraph = Paragraph::new(art_styled).alignment(Alignment::Center);
    frame.render_widget(art_paragraph, content_chunks[0]);

    // Render app name with rainbow neon too
    let name_spans: Vec<Span> = APP_NAME
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let hue = (base_hue + i as f32 * 25.0) % 360.0;
            let pulse = ((elapsed_ms as f32 * 0.004 + i as f32 * 0.5).sin() + 1.0) * 0.5;
            let lightness = 55.0 + pulse * 20.0;
            let color = Color::from_hsl(hue, 100.0, lightness);
            Span::styled(
                ch.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    let name_line = Line::from(name_spans);

    let version_line = Line::from(vec![Span::styled(
        format!("v{}", APP_VERSION),
        Style::default().fg(Color::DarkGray),
    )]);

    let name_paragraph = Paragraph::new(vec![name_line, version_line]).alignment(Alignment::Center);
    frame.render_widget(name_paragraph, content_chunks[2]);
}
