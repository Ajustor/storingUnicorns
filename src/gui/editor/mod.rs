//! SQL editor: a multiline `TextEdit` with syntax highlighting and an
//! autocompletion popup. Each console has its own editor id, so cursors and
//! selections are kept per console.

pub mod completion;

use std::sync::{Arc, Weak};

use egui::epaint::mutex::Mutex;
use egui::epaint::TextureAtlas;
use egui::text::{CCursor, CCursorRange, LayoutJob, LayoutSection};
use egui::{FontId, Galley, TextFormat};

use crate::engine::sql::lexer::tokenize_spans;

use super::theme::kind_color;
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

/// Above this size the text is laid out plain: highlighting a huge script
/// costs more than it helps.
pub const HIGHLIGHT_MAX_BYTES: usize = 200 * 1024;

/// Coloured layout of `text`, one section per run of same-coloured SQL
/// tokens (plain text above [`HIGHLIGHT_MAX_BYTES`]).
pub fn highlight_job(text: &str, known_columns: &[String], dark: bool, size: f32) -> LayoutJob {
    let font = FontId::monospace(size);
    let plain = |font: FontId| {
        LayoutJob::single_section(
            text.to_string(),
            TextFormat {
                font_id: font,
                ..Default::default()
            },
        )
    };
    if text.len() > HIGHLIGHT_MAX_BYTES {
        return plain(font);
    }
    let mut job = LayoutJob {
        text: text.to_string(),
        ..Default::default()
    };
    let mut covered = 0;
    for (kind, range) in tokenize_spans(text, known_columns) {
        // The tokenizer must never drop text; fall back to plain text if it does.
        if range.start != covered {
            return plain(font);
        }
        covered = range.end;
        let color = kind_color(kind, dark);
        match job.sections.last_mut() {
            Some(last) if last.format.color == color => last.byte_range.end = range.end,
            _ => job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: range,
                format: TextFormat {
                    font_id: font.clone(),
                    color,
                    ..Default::default()
                },
            }),
        }
    }
    if covered != text.len() {
        return plain(font);
    }
    job
}

/// The last galley of a highlighted text field, reused while its inputs
/// (text, columns, theme, font size, wrap width, font atlas) are unchanged:
/// a focused `TextEdit` repaints for every cursor blink and mouse move.
#[derive(Clone, Default)]
pub struct HighlightCache {
    key: u64,
    /// Font atlas the galley was laid out with: it is rebuilt (and the
    /// glyphs move) when it fills up or the scale changes.
    atlas: Weak<Mutex<TextureAtlas>>,
    galley: Option<Arc<Galley>>,
}

impl HighlightCache {
    /// Highlighted galley of `text` for a layouter.
    pub fn layout(
        &mut self,
        ui: &egui::Ui,
        text: &str,
        known_columns: &[String],
        wrap_width: f32,
    ) -> Arc<Galley> {
        let dark = ui.visuals().dark_mode;
        let size = egui::TextStyle::Monospace.resolve(ui.style()).size;
        let key = egui::util::hash((
            text,
            known_columns,
            dark,
            size.to_bits(),
            wrap_width.to_bits(),
        ));
        let atlas = ui.fonts(|f| f.texture_atlas());
        if let Some(galley) = &self.galley {
            if self.key == key && Weak::ptr_eq(&self.atlas, &Arc::downgrade(&atlas)) {
                return galley.clone();
            }
        }
        let mut job = highlight_job(text, known_columns, dark, size);
        job.wrap.max_width = wrap_width;
        let galley = ui.fonts(|f| f.layout_job(job));
        *self = Self {
            key,
            atlas: Arc::downgrade(&atlas),
            galley: Some(galley.clone()),
        };
        galley
    }
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
    let columns = cx.known_columns;
    let cache_id = id.with("highlight");
    let mut cache: HighlightCache = ui.data(|d| d.get_temp(cache_id)).unwrap_or_default();
    let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| -> Arc<Galley> {
        cache.layout(ui, text, columns, wrap_width)
    };
    if text.len() > HIGHLIGHT_MAX_BYTES {
        ui.label(
            egui::RichText::new("Coloration désactivée (gros script)")
                .small()
                .weak(),
        );
    }
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
    ui.data_mut(|d| d.insert_temp(cache_id, cache));
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
        let mut end = 0;
        for s in &job.sections {
            assert_eq!(s.byte_range.start, end);
            end = s.byte_range.end;
        }
        assert_eq!(end, text.len());
    }

    #[test]
    fn highlight_job_merges_same_colour_runs() {
        // Identifiers and whitespace share a colour: one section.
        let job = highlight_job("a b c", &[], true, 13.0);
        assert_eq!(job.sections.len(), 1);
    }

    #[test]
    fn huge_text_is_not_highlighted() {
        let text = "SELECT 1;\n".repeat(HIGHLIGHT_MAX_BYTES / 10 + 1);
        let job = highlight_job(&text, &[], true, 13.0);
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.text.len(), text.len());
    }

    #[test]
    fn highlight_cache_reuses_the_galley_until_an_input_changes() {
        let ctx = egui::Context::default();
        let mut cache = HighlightCache::default();
        let mut galleys = Vec::new();
        for (text, cols) in [
            ("SELECT a", &[][..]),
            ("SELECT a", &[][..]),
            ("SELECT b", &[][..]),
        ]
        .into_iter()
        .chain([("SELECT b", &["b".to_string()][..])])
        {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    galleys.push(cache.layout(ui, text, cols, 100.0));
                });
            });
        }
        assert!(Arc::ptr_eq(&galleys[0], &galleys[1]), "same inputs");
        assert!(!Arc::ptr_eq(&galleys[1], &galleys[2]), "text changed");
        assert!(!Arc::ptr_eq(&galleys[2], &galleys[3]), "columns changed");
    }
    /// ~`bytes` of realistic SQL (several statements per line kind).
    fn big_script(bytes: usize) -> String {
        let chunk =
            "SELECT u.id, u.name, COUNT(*) AS n FROM users u JOIN orders o ON o.user_id = u.id
                     WHERE u.created_at > '2024-01-01' AND o.total >= 12.5 -- recent
                     GROUP BY u.id, u.name /* grouped */ ORDER BY n DESC;
";
        chunk.repeat(bytes / chunk.len() + 1)
    }

    /// `cargo test --release -- --ignored bench --nocapture`
    #[test]
    #[ignore]
    fn bench_highlight_job_190kb() {
        let text = big_script(190 * 1024);
        let cols = vec!["id".to_string(), "name".to_string(), "total".to_string()];
        let _ = highlight_job(&text, &cols, true, 13.0);
        let n = 5;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(highlight_job(&text, &cols, true, 13.0));
        }
        eprintln!(
            "highlight_job 190 KB: {:.2} ms",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }

    /// Frame time of a focused editor holding a 1 MB script (no edit, so a
    /// cache can be hit), layout + tessellation.
    #[test]
    #[ignore]
    fn bench_editor_frame() {
        for size in [190 * 1024, 1 << 20] {
            bench_editor_frame_of(big_script(size));
        }
    }

    fn bench_editor_frame_of(mut text: String) {
        let ctx = egui::Context::default();
        let cols = vec!["id".to_string(), "name".to_string()];
        let mut completion = Completion::default();
        let tables = Vec::new;
        let id = egui::Id::new("bench_editor");
        let mut frame = || {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        show(
                            ui,
                            &mut text,
                            id,
                            400.0,
                            &mut completion,
                            EditorContext {
                                known_columns: &cols,
                                tables: &tables,
                            },
                        )
                    });
                });
            });
            std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
        };
        frame();
        ctx.memory_mut(|m| m.request_focus(id));
        frame();
        let n = 10;
        let t = std::time::Instant::now();
        for _ in 0..n {
            frame();
        }
        eprintln!(
            "editor frame, {} KB script: {:.2} ms",
            text.len() / 1024,
            t.elapsed().as_secs_f64() * 1000.0 / n as f64
        );
    }
}
