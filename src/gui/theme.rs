use egui::{Color32, ThemePreference};

use crate::engine::sql::lexer::TokenKind;

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
    add_symbol_fallback(&mut fonts);
    fonts
}

/// System fonts covering the symbols and emoji egui's bundled fonts lack
/// (✓, 🦄, …, drawn as boxes otherwise), first found wins.
const SYMBOL_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\seguisym.ttf",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];

/// Appends the first readable [`SYMBOL_FONTS`] as the last fallback of
/// both families (the bundled fonts keep drawing what they cover).
fn add_symbol_fallback(fonts: &mut egui::FontDefinitions) {
    let Some(data) = SYMBOL_FONTS
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .find(|d| looks_like_font(d))
    else {
        return;
    };
    const NAME: &str = "system-symbols";
    fonts.font_data.insert(
        NAME.into(),
        std::sync::Arc::new(egui::FontData::from_owned(data)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(NAME.into());
    }
}

/// Whether `bytes` hold a TrueType/OpenType font whose table directory fits
/// in the file: egui panics on a font it cannot parse, so a truncated or
/// foreign file is skipped.
fn looks_like_font(bytes: &[u8]) -> bool {
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    if !matches!(
        bytes.get(..4),
        Some([0, 1, 0, 0]) | Some(b"OTTO") | Some(b"true")
    ) {
        return false;
    }
    let Some(tables) = u16_at(4) else {
        return false;
    };
    tables > 0
        && (0..tables as usize).all(|t| {
            let record = 12 + t * 16;
            match (u32_at(record + 8), u32_at(record + 12)) {
                (Some(offset), Some(len)) => (offset as u64 + len as u64) <= bytes.len() as u64,
                _ => false,
            }
        })
}

/// Colour of a SQL token kind for the current background.
pub fn kind_color(kind: TokenKind, dark: bool) -> Color32 {
    let (d, l) = match kind {
        TokenKind::Keyword => (
            Color32::from_rgb(0xC6, 0x8B, 0xFF),
            Color32::from_rgb(0x7A, 0x2E, 0xC9),
        ),
        TokenKind::Function => (
            Color32::from_rgb(0xF2, 0xC9, 0x4C),
            Color32::from_rgb(0x9A, 0x6A, 0x00),
        ),
        TokenKind::String => (
            Color32::from_rgb(0x8C, 0xD3, 0x8C),
            Color32::from_rgb(0x1E, 0x7B, 0x34),
        ),
        TokenKind::Number => (
            Color32::from_rgb(0x6C, 0xC7, 0xE8),
            Color32::from_rgb(0x00, 0x6E, 0x96),
        ),
        TokenKind::Operator => (
            Color32::from_rgb(0xF0, 0x7C, 0x7C),
            Color32::from_rgb(0xB0, 0x2A, 0x2A),
        ),
        TokenKind::Comment => (Color32::from_gray(0x80), Color32::from_gray(0x80)),
        TokenKind::Column => (
            Color32::from_rgb(0x7F, 0xD8, 0xC8),
            Color32::from_rgb(0x00, 0x7A, 0x66),
        ),
        TokenKind::Identifier | TokenKind::Whitespace => {
            (Color32::from_gray(0xE0), Color32::from_gray(0x20))
        }
        TokenKind::Punctuation => (Color32::from_gray(0xA0), Color32::from_gray(0x60)),
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
        let kw = kind_color(TokenKind::Keyword, true);
        let id = kind_color(TokenKind::Identifier, true);
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

    #[test]
    fn looks_like_font_checks_the_magic_and_the_table_directory() {
        // sfnt header: version, numTables = 1, then one table record whose
        // data (offset 28, length 4) ends exactly at the end of the file.
        let mut font = vec![0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        font.extend_from_slice(b"cmap");
        font.extend_from_slice(&[0; 4]);
        font.extend_from_slice(&28u32.to_be_bytes());
        font.extend_from_slice(&4u32.to_be_bytes());
        font.extend_from_slice(&[0; 4]);
        assert!(looks_like_font(&font));
        assert!(!looks_like_font(&font[..font.len() - 1]), "truncated");
        assert!(!looks_like_font(b"<html>not a font</html>"));
        assert!(!looks_like_font(&[]));
        let mut no_tables = font[..12].to_vec();
        no_tables[5] = 0;
        assert!(!looks_like_font(&no_tables));
    }

    #[test]
    fn the_symbol_fallback_comes_after_the_bundled_fonts() {
        let mut fonts = egui::FontDefinitions::default();
        let before = fonts.families[&egui::FontFamily::Monospace].clone();
        add_symbol_fallback(&mut fonts);
        let after = &fonts.families[&egui::FontFamily::Monospace];
        assert_eq!(after[..before.len()], before[..], "bundled fonts first");
        assert!(after.len() <= before.len() + 1);
    }

    /// Characters seen as boxes in the history and the grid ("✓", emoji)
    /// are drawn once the system fallback is loaded.
    #[cfg(windows)]
    #[test]
    fn symbols_and_emoji_have_glyphs_on_windows() {
        let fonts = egui::epaint::text::Fonts::new(1.0, 2048, fonts());
        for family in [
            egui::FontId::monospace(13.0),
            egui::FontId::proportional(13.0),
        ] {
            for s in ["é", "ç", "🦄", "✓", "⚠", "│"] {
                assert!(fonts.has_glyphs(&family, s), "{s} in {family:?}");
            }
        }
    }
}
