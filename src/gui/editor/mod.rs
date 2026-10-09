//! SQL editor: a multiline `TextEdit` with syntax highlighting and an
//! autocompletion popup. Each console has its own editor id, so cursors and
//! selections are kept per console.

pub mod completion;

use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{FontId, Galley, TextFormat};

use crate::engine::sql::lexer::{tokenize_sql, SqlToken};

use super::theme::token_color;
use completion::Completion;

/// Byte offset of the `chars`-th character (end of text when past it).
pub fn char_to_byte(s: &str, chars: usize) -> usize {
    s.char_indices()
        .nth(chars)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Number of characters before byte offset `byte` (clamped to the text).
pub fn byte_to_char(s: &str, byte: usize) -> usize {
    let mut byte = byte.min(s.len());
    while !s.is_char_boundary(byte) {
        byte -= 1;
    }
    s[..byte].chars().count()
}

fn token_text(t: &SqlToken) -> &str {
    match t {
        SqlToken::Keyword(s)
        | SqlToken::Function(s)
        | SqlToken::String(s)
        | SqlToken::Number(s)
        | SqlToken::Operator(s)
        | SqlToken::Comment(s)
        | SqlToken::Identifier(s)
        | SqlToken::Column(s)
        | SqlToken::Punctuation(s)
        | SqlToken::Whitespace(s) => s,
    }
}

/// Coloured layout of `text`, one section per SQL token.
pub fn highlight_job(text: &str, known_columns: &[String], dark: bool, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = FontId::monospace(size);
    let mut covered = 0;
    for token in tokenize_sql(text, known_columns) {
        let s = token_text(&token);
        covered += s.len();
        job.append(
            s,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: token_color(&token, dark),
                ..Default::default()
            },
        );
    }
    // The tokenizer must never drop text; fall back to plain text if it does.
    if covered != text.len() {
        job = LayoutJob::single_section(
            text.to_string(),
            TextFormat {
                font_id: font,
                ..Default::default()
            },
        );
    }
    job
}

/// Move the cursor of editor `id` to `byte` in `text`.
pub fn set_cursor(ctx: &egui::Context, id: egui::Id, text: &str, byte: usize) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let c = CCursor::new(byte_to_char(text, byte));
    state.cursor.set_char_range(Some(CCursorRange::one(c)));
    state.store(ctx, id);
}

/// Cursor and non-empty selection (byte offsets, sorted) of editor `id`, as
/// stored by its last frame (kept while the editor is unfocused).
pub fn cursor_and_selection(
    ctx: &egui::Context,
    id: egui::Id,
    text: &str,
) -> Option<(usize, Option<(usize, usize)>)> {
    let range = egui::TextEdit::load_state(ctx, id)?.cursor.char_range()?;
    let cursor = char_to_byte(text, range.primary.index);
    let (a, b) = (range.primary.index, range.secondary.index);
    let selection = (a != b).then(|| (char_to_byte(text, a.min(b)), char_to_byte(text, a.max(b))));
    Some((cursor, selection))
}

/// What the editor needs from its console.
pub struct EditorContext<'a> {
    /// Columns of the console's last results (highlighting and completion).
    pub known_columns: &'a [String],
    /// Tables of the console's connection; only called when completing.
    pub tables: &'a dyn Fn() -> Vec<String>,
}

pub struct EditorOutput {
    /// Byte offset of the cursor.
    pub cursor: usize,
    /// Selected byte range, when not empty.
    pub selection: Option<(usize, usize)>,
    /// The text was changed this frame (typing or completion).
    pub changed: bool,
}

/// Draw the editor filling `min_height` (more when the text is longer).
pub fn show(
    ui: &mut egui::Ui,
    text: &mut String,
    id: egui::Id,
    min_height: f32,
    completion: &mut Completion,
    cx: EditorContext,
) -> EditorOutput {
    let dark = ui.visuals().dark_mode;
    let size = egui::TextStyle::Monospace.resolve(ui.style()).size;
    let columns = cx.known_columns;
    let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| -> Arc<Galley> {
        let mut job = highlight_job(text, columns, dark, size);
        job.wrap.max_width = wrap_width;
        ui.fonts(|f| f.layout_job(job))
    };
    let len = text.len();
    // Navigation keys go to the popup while it is open.
    let triggered = completion.intercept_keys(ui, text, id);
    let output = egui::TextEdit::multiline(text)
        .id(id)
        .code_editor()
        .desired_width(f32::INFINITY)
        .min_size(egui::vec2(0.0, min_height))
        .hint_text("SELECT * FROM …")
        .layouter(&mut layouter)
        .show(ui);
    let changed = output.response.changed();
    completion.after_edit(ui, &output, text, id, &cx, triggered);

    let (cursor, selection) =
        cursor_and_selection(ui.ctx(), id, text).unwrap_or((text.len(), None));
    EditorOutput {
        cursor,
        selection,
        changed: changed || text.len() != len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_to_byte_offsets() {
        let s = "é;SELECT";
        assert_eq!(char_to_byte(s, 0), 0);
        assert_eq!(char_to_byte(s, 1), 2);
        assert_eq!(char_to_byte(s, 99), s.len());
        assert_eq!(byte_to_char(s, 2), 1);
        assert_eq!(byte_to_char(s, 1), 0, "inside a character");
        assert_eq!(byte_to_char(s, 99), 8);
    }

    #[test]
    fn highlight_job_covers_whole_text() {
        let text = "SELECT a FROM t -- c\nWHERE x = 'y'";
        let job = highlight_job(text, &[], true, 13.0);
        assert_eq!(job.text, text);
        assert!(job.sections.len() > 5);
    }
}
