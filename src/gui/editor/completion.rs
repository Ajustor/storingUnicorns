//! Autocompletion popup of the SQL editor (tables of the connection, columns
//! of the last results, keywords).

use egui::{Key, Modifiers};

use crate::engine::sql::lexer::{extract_token, get_completions};

use super::{char_to_byte, set_cursor, EditorContext};

/// Maximum number of suggestions shown.
const MAX_ITEMS: usize = 50;

#[derive(Default)]
pub struct Completion {
    pub items: Vec<String>,
    pub selected: usize,
    pub open: bool,
    /// Byte offset of the cursor the items were computed for.
    cursor: usize,
}

/// Replace the token ending at `cursor` with `suggestion`; returns the new
/// text and cursor (bytes).
pub fn apply_suggestion(text: &str, cursor: usize, suggestion: &str) -> (String, usize) {
    let cursor = cursor.min(text.len());
    let (start, _, _) = extract_token(&text[..cursor]);
    let mut out = String::with_capacity(text.len() + suggestion.len());
    out.push_str(&text[..start]);
    out.push_str(suggestion);
    out.push_str(&text[cursor..]);
    (out, start + suggestion.len())
}

impl Completion {
    fn refresh(&mut self, text: &str, cursor: usize, cx: &EditorContext) {
        self.items = get_completions(text, cursor, cx.known_columns, &(cx.tables)());
        self.items.truncate(MAX_ITEMS);
        self.selected = 0;
        self.cursor = cursor;
        self.open = !self.items.is_empty();
    }

    /// Called before the TextEdit: navigation keys go to the popup while it
    /// is open. Returns whether Ctrl+Space was pressed.
    pub fn intercept_keys(&mut self, ui: &mut egui::Ui, text: &mut String, id: egui::Id) -> bool {
        let focused = ui.memory(|m| m.has_focus(id));
        if !focused {
            self.open = false;
            return false;
        }
        let trigger = ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space));
        if !self.open {
            return trigger;
        }
        let n = self.items.len();
        let accept = ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                self.selected = (self.selected + 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                self.selected = (self.selected + n - 1) % n;
            }
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                self.open = false;
            }
            i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab)
        });
        if accept && self.open {
            self.accept_selected(ui.ctx(), text, id);
        }
        trigger
    }

    fn accept_selected(&mut self, ctx: &egui::Context, text: &mut String, id: egui::Id) {
        self.open = false;
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let (new_text, cursor) = apply_suggestion(text, self.cursor, item);
        *text = new_text;
        set_cursor(ctx, id, text, cursor);
    }

    /// After the TextEdit: (re)compute suggestions and draw the popup under
    /// the cursor.
    pub fn after_edit(
        &mut self,
        ui: &mut egui::Ui,
        out: &egui::text_edit::TextEditOutput,
        text: &mut String,
        id: egui::Id,
        cx: &EditorContext,
        triggered: bool,
    ) {
        let Some(range) = out.cursor_range else {
            self.open = false;
            return;
        };
        let cursor = char_to_byte(text, range.primary.ccursor.index);
        let typed_word_char = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::Text(t)
                    if t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.'))
            })
        });
        if triggered || (out.response.changed() && typed_word_char) {
            self.refresh(text, cursor, cx);
        } else if out.response.changed() || cursor != self.cursor {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let rect = out.galley.pos_from_cursor(&range.primary);
        let pos = out.galley_pos + rect.left_bottom().to_vec2();
        let mut clicked = None;
        egui::Area::new(id.with("completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_height(220.0);
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (i, item) in self.items.iter().enumerate() {
                            let r = ui.selectable_label(i == self.selected, item);
                            if i == self.selected {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
                });
            });
        if let Some(i) = clicked {
            self.selected = i;
            self.accept_selected(ui.ctx(), text, id);
            ui.memory_mut(|m| m.request_focus(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_replaces_current_word() {
        let (text, cursor) = apply_suggestion("SELECT na FROM t", 9, "name");
        assert_eq!(text, "SELECT name FROM t");
        assert_eq!(cursor, 11);
    }

    #[test]
    fn apply_at_end_of_text() {
        let (text, cursor) = apply_suggestion("SELECT * FROM us", 16, "users");
        assert_eq!(text, "SELECT * FROM users");
        assert_eq!(cursor, 19);
    }

    #[test]
    fn apply_after_a_space_inserts() {
        let (text, cursor) = apply_suggestion("SELECT * FROM ", 14, "users");
        assert_eq!(text, "SELECT * FROM users");
        assert_eq!(cursor, 19);
    }
}
