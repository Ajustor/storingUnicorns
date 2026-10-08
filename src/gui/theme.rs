use egui::{Color32, ThemePreference};

use crate::engine::sql::lexer::SqlToken;

/// Persisted as `AppConfig::theme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            Some("dark") => Self::Dark,
            Some("light") => Self::Light,
            _ => Self::System,
        }
    }

    pub fn to_config(self) -> Option<String> {
        match self {
            Self::System => None,
            Self::Dark => Some("dark".into()),
            Self::Light => Some("light".into()),
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Dark,
            Self::Dark => Self::Light,
            Self::Light => Self::System,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::System => egui_phosphor::regular::CIRCLE_HALF,
            Self::Dark => egui_phosphor::regular::MOON,
            Self::Light => egui_phosphor::regular::SUN,
        }
    }
}

/// Unicorn accent, shared by both themes.
pub const ACCENT: Color32 = Color32::from_rgb(0xB3, 0x6B, 0xFF);
pub const SUCCESS: Color32 = Color32::from_rgb(0x4C, 0xC3, 0x8A);
pub const ERROR: Color32 = Color32::from_rgb(0xE5, 0x5C, 0x6C);

/// Default palette offered in the connection form (DataGrip-like).
pub const CONNECTION_COLORS: [[u8; 3]; 7] = [
    [0xE5, 0x5C, 0x6C], // prod red
    [0xF2, 0xA6, 0x4C], // orange
    [0xF2, 0xC9, 0x4C], // yellow
    [0x4C, 0xC3, 0x8A], // green
    [0x4C, 0x9E, 0xE5], // blue
    [0xB3, 0x6B, 0xFF], // purple
    [0x9A, 0x9A, 0x9A], // grey
];

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Tag colour of a connection; grey when it has none.
pub fn connection_color(color: Option<[u8; 3]>) -> Color32 {
    color.map(rgb).unwrap_or(Color32::GRAY)
}

/// Small filled circle tagging a connection.
pub fn dot(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    response
}

pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(match choice {
        ThemeChoice::System => ThemePreference::System,
        ThemeChoice::Dark => ThemePreference::Dark,
        ThemeChoice::Light => ThemePreference::Light,
    });
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.45);
            style.visuals.selection.stroke.color = ACCENT;
            style.visuals.hyperlink_color = ACCENT;
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        });
    }
}

pub fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    fonts
}

#[allow(dead_code)] // SQL editor (plan 3b, Task 6)
/// Colour of a SQL token for the current background.
pub fn token_color(token: &SqlToken, dark: bool) -> Color32 {
    let (d, l) = match token {
        SqlToken::Keyword(_) => (
            Color32::from_rgb(0xC6, 0x8B, 0xFF),
            Color32::from_rgb(0x7A, 0x2E, 0xC9),
        ),
        SqlToken::Function(_) => (
            Color32::from_rgb(0xF2, 0xC9, 0x4C),
            Color32::from_rgb(0x9A, 0x6A, 0x00),
        ),
        SqlToken::String(_) => (
            Color32::from_rgb(0x8C, 0xD3, 0x8C),
            Color32::from_rgb(0x1E, 0x7B, 0x34),
        ),
        SqlToken::Number(_) => (
            Color32::from_rgb(0x6C, 0xC7, 0xE8),
            Color32::from_rgb(0x00, 0x6E, 0x96),
        ),
        SqlToken::Operator(_) => (
            Color32::from_rgb(0xF0, 0x7C, 0x7C),
            Color32::from_rgb(0xB0, 0x2A, 0x2A),
        ),
        SqlToken::Comment(_) => (Color32::from_gray(0x80), Color32::from_gray(0x80)),
        SqlToken::Column(_) => (
            Color32::from_rgb(0x7F, 0xD8, 0xC8),
            Color32::from_rgb(0x00, 0x7A, 0x66),
        ),
        SqlToken::Identifier(_) | SqlToken::Whitespace(_) => {
            (Color32::from_gray(0xE0), Color32::from_gray(0x20))
        }
        SqlToken::Punctuation(_) => (Color32::from_gray(0xA0), Color32::from_gray(0x60)),
    };
    if dark {
        d
    } else {
        l
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_choice_roundtrips_through_config() {
        for c in [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light] {
            assert_eq!(ThemeChoice::from_config(c.to_config().as_deref()), c);
        }
        assert_eq!(
            ThemeChoice::from_config(Some("garbage")),
            ThemeChoice::System
        );
    }

    #[test]
    fn keywords_and_identifiers_differ() {
        let kw = token_color(&SqlToken::Keyword("SELECT".into()), true);
        let id = token_color(&SqlToken::Identifier("x".into()), true);
        assert_ne!(kw, id);
    }

    #[test]
    fn connection_colors_are_distinct() {
        for (i, a) in CONNECTION_COLORS.iter().enumerate() {
            for b in &CONNECTION_COLORS[i + 1..] {
                assert_ne!(rgb(*a), rgb(*b));
            }
        }
    }
}
