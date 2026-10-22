//! Autocompletion popup of the SQL fields: the columns of the tables a
//! statement references (or of the displayed table, in the table view's
//! WHERE / ORDER BY bars), the connection's tables, keywords.
//!
//! Columns not loaded yet are requested from the app (`take_requests`); the
//! popup is computed again when they arrive, while the cursor stays put.

use egui::{Key, Modifiers};

use crate::engine::sql::completion::{
    complete, complete_for_table, token_start, BarMode, ColumnInfo, Completions, Context,
    Suggestion, TableRef,
};

use super::{char_to_byte, set_cursor};

/// Maximum number of suggestions shown.
const MAX_ITEMS: usize = 100;
/// Popup width bounds: at least this, at most this share of the window.
const MIN_WIDTH: f32 = 220.0;
const MAX_WIDTH_SHARE: f32 = 0.6;
/// Space between a label and its detail.
const DETAIL_GAP: f32 = 16.0;

/// What completion reads from the connection.
pub trait CompletionSource {
    /// Identifier quotes of the engine.
    fn quotes(&self) -> (char, char);
    /// Known tables ("schema.table" or "table"); only called when completing.
    fn tables(&self) -> Vec<String>;
    /// Columns of `table`; `None` when not loaded yet (they can be requested).
    fn columns(&self, table: &TableRef) -> Option<Vec<ColumnInfo>>;
}

/// A source knowing nothing (tests).
#[cfg(test)]
pub struct NoSource;

#[cfg(test)]
impl CompletionSource for NoSource {
    fn quotes(&self) -> (char, char) {
        ('"', '"')
    }
    fn tables(&self) -> Vec<String> {
        Vec::new()
    }
    fn columns(&self, _: &TableRef) -> Option<Vec<ColumnInfo>> {
        Some(Vec::new())
    }
}

/// What a field completes.
#[derive(Clone, Copy)]
pub enum Scope<'a> {
    /// A script (console): the tables of the statement under the cursor,
    /// else the columns of the last results.
    Script { fallback_columns: &'a [String] },
    /// A WHERE / ORDER BY fragment of one table (table view).
    Table { table: &'a TableRef, mode: BarMode },
}

/// Suggestions for `cursor` (bytes) in `text`.
pub fn compute(
    text: &str,
    cursor: usize,
    source: &dyn CompletionSource,
    scope: Scope,
) -> Completions {
    let tables = source.tables();
    let columns = |t: &TableRef| source.columns(t);
    let fallback: &[String] = match scope {
        Scope::Script { fallback_columns } => fallback_columns,
        Scope::Table { .. } => &[],
    };
    let cx = Context {
        quotes: source.quotes(),
        tables: &tables,
        columns: &columns,
        fallback_columns: fallback,
    };
    match scope {
        Scope::Script { .. } => complete(text, cursor, &cx),
        Scope::Table { table, mode } => complete_for_table(text, cursor, mode, table, &cx),
    }
}

#[derive(Default)]
pub struct Completion {
    pub items: Vec<Suggestion>,
    pub selected: usize,
    pub open: bool,
    /// Byte offset of the cursor the items were computed for.
    cursor: usize,
    /// Tables whose columns were missing when the items were computed: the
    /// items are computed again when one arrives (same cursor).
    waiting: Vec<TableRef>,
    /// Tables to load, taken by the app.
    requests: Vec<TableRef>,
}

/// Replace the token ending at `cursor` with `suggestion`; returns the new
/// text and cursor (bytes).
pub fn apply_suggestion(text: &str, cursor: usize, suggestion: &str) -> (String, usize) {
    // Same token as the one completed (quotes in literals ignored).
    let start = token_start(text, cursor);
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut out = String::with_capacity(text.len() + suggestion.len());
    out.push_str(&text[..start]);
    out.push_str(suggestion);
    out.push_str(&text[cursor..]);
    (out, start + suggestion.len())
}

impl Completion {
    /// Compute the items for `cursor` and open the popup when there are any.
    pub fn refresh(
        &mut self,
        text: &str,
        cursor: usize,
        source: &dyn CompletionSource,
        scope: Scope,
    ) {
        let Completions { mut items, missing } = compute(text, cursor, source, scope);
        items.truncate(MAX_ITEMS);
        self.items = items;
        self.selected = 0;
        self.cursor = cursor;
        self.open = !self.items.is_empty();
        for t in &missing {
            if !self.requests.contains(t) {
                self.requests.push(t.clone());
            }
        }
        self.waiting = missing;
    }

    /// Columns awaited by the last refresh arrived while the cursor stayed at
    /// `cursor`: compute the items again. Returns whether it did.
    pub fn poll(
        &mut self,
        text: &str,
        cursor: usize,
        source: &dyn CompletionSource,
        scope: Scope,
    ) -> bool {
        if self.waiting.is_empty() || cursor != self.cursor {
            return false;
        }
        if !self.waiting.iter().any(|t| source.columns(t).is_some()) {
            return false;
        }
        self.refresh(text, cursor, source, scope);
        true
    }

    /// Close the popup and stop waiting for columns.
    pub fn close(&mut self) {
        self.open = false;
        self.waiting.clear();
    }

    /// Tables whose columns the app should load (taken once).
    pub fn take_requests(&mut self) -> Vec<TableRef> {
        std::mem::take(&mut self.requests)
    }

    /// Called before the TextEdit: navigation keys go to the popup while it
    /// is open. Returns whether Ctrl+Space was pressed.
    pub fn intercept_keys(&mut self, ui: &mut egui::Ui, text: &mut String, id: egui::Id) -> bool {
        let focused = ui.memory(|m| m.has_focus(id));
        if !focused {
            self.close();
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
                self.close();
            }
            i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab)
        });
        if accept && self.open {
            self.accept_selected(ui.ctx(), text, id);
        }
        trigger
    }

    fn accept_selected(&mut self, ctx: &egui::Context, text: &mut String, id: egui::Id) {
        self.close();
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let (new_text, cursor) = apply_suggestion(text, self.cursor, &item.insert);
        *text = new_text;
        set_cursor(ctx, id, text, cursor);
    }

    /// After the TextEdit: (re)compute suggestions and draw the popup under
    /// the cursor.
    #[allow(clippy::too_many_arguments)]
    pub fn after_edit(
        &mut self,
        ui: &mut egui::Ui,
        out: &egui::text_edit::TextEditOutput,
        text: &mut String,
        id: egui::Id,
        source: &dyn CompletionSource,
        scope: Scope,
        triggered: bool,
    ) {
        let Some(range) = out.cursor_range else {
            self.close();
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
            self.refresh(text, cursor, source, scope);
        } else if out.response.changed() || cursor != self.cursor {
            self.close();
        } else if self.poll(text, cursor, source, scope) {
            ui.ctx().request_repaint();
        }
        if !self.open {
            return;
        }
        let rect = out.galley.pos_from_cursor(&range.primary);
        let pos = out.galley_pos + rect.left_bottom().to_vec2();
        if let Some(i) = self.popup(ui, id, pos) {
            self.selected = i;
            self.accept_selected(ui.ctx(), text, id);
            ui.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Draw the items at `pos`: label, then its detail dimmed and right
    /// aligned; as wide as the widest item (within bounds). Returns the
    /// clicked item.
    fn popup(&self, ui: &egui::Ui, id: egui::Id, pos: egui::Pos2) -> Option<usize> {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let small = egui::TextStyle::Small.resolve(ui.style());
        let text_width = |s: &str, f: &egui::FontId| {
            ui.fonts(|fonts| {
                fonts
                    .layout_no_wrap(s.to_string(), f.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            })
        };
        let pad = ui.spacing().button_padding.x;
        let widest = self
            .items
            .iter()
            .map(|s| {
                let detail = if s.detail.is_empty() {
                    0.0
                } else {
                    DETAIL_GAP + text_width(&s.detail, &small)
                };
                text_width(&s.label, &font) + detail
            })
            .fold(0.0, f32::max);
        let max = ui.ctx().screen_rect().width() * MAX_WIDTH_SHARE;
        let width = popup_width(widest + 2.0 * pad, max);
        let row_height = font.size.max(ui.spacing().interact_size.y);
        let mut clicked = None;
        egui::Area::new(id.with("completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, item) in self.items.iter().enumerate() {
                                let (rect, r) = ui.allocate_exact_size(
                                    egui::vec2(width, row_height),
                                    egui::Sense::click(),
                                );
                                let selected = i == self.selected;
                                let visuals = ui.visuals();
                                if selected {
                                    ui.painter()
                                        .rect_filled(rect, 2.0, visuals.selection.bg_fill);
                                } else if r.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        2.0,
                                        visuals.widgets.hovered.weak_bg_fill,
                                    );
                                }
                                let color = if selected {
                                    visuals.selection.stroke.color
                                } else {
                                    visuals.text_color()
                                };
                                let painter = ui.painter_at(rect);
                                painter.text(
                                    rect.left_center() + egui::vec2(pad, 0.0),
                                    egui::Align2::LEFT_CENTER,
                                    &item.label,
                                    font.clone(),
                                    color,
                                );
                                if !item.detail.is_empty() {
                                    painter.text(
                                        rect.right_center() - egui::vec2(pad, 0.0),
                                        egui::Align2::RIGHT_CENTER,
                                        &item.detail,
                                        small.clone(),
                                        visuals.weak_text_color(),
                                    );
                                }
                                if selected {
                                    r.scroll_to_me(None);
                                }
                                if r.clicked() {
                                    clicked = Some(i);
                                }
                            }
                        });
                });
            });
        clicked
    }
}

/// Width of the popup for items `content` wide: at least [`MIN_WIDTH`], at
/// most `max` (a share of the window), but never below the minimum.
pub fn popup_width(content: f32, max: f32) -> f32 {
    content.min(max).max(MIN_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::sql::completion::Kind;
    use std::cell::RefCell;
    use std::collections::HashMap;

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

    #[test]
    fn apply_ignores_quotes_inside_earlier_strings() {
        let text = r#"SELECT * FROM users WHERE name = 'O"Brien' AND em"#;
        let (out, cursor) = apply_suggestion(text, text.len(), "email");
        assert_eq!(
            out,
            r#"SELECT * FROM users WHERE name = 'O"Brien' AND email"#
        );
        assert_eq!(cursor, out.len());
        // The WHERE bar of the table view.
        let (out, _) = apply_suggestion("name = '[x' AND n", 17, "name");
        assert_eq!(out, "name = '[x' AND name");
    }

    #[test]
    fn apply_after_a_qualifier_keeps_it() {
        let (text, cursor) = apply_suggestion("SELECT u.em FROM users u", 11, "email");
        assert_eq!(text, "SELECT u.email FROM users u");
        assert_eq!(cursor, 14);
    }

    fn col(name: &str, ty: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            type_name: ty.into(),
            primary_key: pk,
        }
    }

    /// Tables `users` and `orders`; only the loaded ones have columns.
    #[derive(Default)]
    struct Fake {
        loaded: RefCell<HashMap<String, Vec<ColumnInfo>>>,
    }

    impl Fake {
        fn load(&self, table: &str) {
            let cols = match table {
                "users" => vec![col("id", "integer", true), col("email", "text", false)],
                _ => vec![col("id", "integer", true), col("user_id", "integer", false)],
            };
            self.loaded.borrow_mut().insert(table.into(), cols);
        }
    }

    impl CompletionSource for Fake {
        fn quotes(&self) -> (char, char) {
            ('"', '"')
        }
        fn tables(&self) -> Vec<String> {
            vec!["orders".into(), "users".into()]
        }
        fn columns(&self, t: &TableRef) -> Option<Vec<ColumnInfo>> {
            self.loaded.borrow().get(&t.name).cloned()
        }
    }

    const NO_RESULTS: Scope = Scope::Script {
        fallback_columns: &[],
    };

    fn labels(c: &Completion) -> Vec<String> {
        c.items.iter().map(|s| s.label.clone()).collect()
    }

    #[test]
    fn popup_lists_the_columns_of_the_aliased_table_with_their_type() {
        let source = Fake::default();
        source.load("users");
        let mut c = Completion::default();
        let text = "SELECT u. FROM users u";
        c.refresh(text, 9, &source, NO_RESULTS);
        assert!(c.open);
        assert_eq!(labels(&c), ["id", "email"]);
        assert_eq!(c.items[0].detail, "PK integer");
        assert_eq!(c.items[1].detail, "text");
        assert!(c.take_requests().is_empty());
    }

    #[test]
    fn join_columns_are_qualified_when_ambiguous() {
        let source = Fake::default();
        source.load("users");
        source.load("orders");
        let mut c = Completion::default();
        let text = "SELECT  FROM users u JOIN orders o ON o.user_id = u.id";
        c.refresh(text, 7, &source, NO_RESULTS);
        let got = labels(&c);
        assert!(got.contains(&"u.id".to_string()), "{got:?}");
        assert!(got.contains(&"o.id".to_string()), "{got:?}");
        assert!(got.contains(&"email".to_string()));
        assert!(got.contains(&"user_id".to_string()));
        let kw = c.items.iter().position(|s| s.kind == Kind::Keyword);
        let last_col = c.items.iter().rposition(|s| s.kind == Kind::Column);
        assert!(last_col < kw, "columns before keywords");
    }

    #[test]
    fn missing_columns_are_requested_once_and_the_popup_refreshed_on_arrival() {
        let source = Fake::default();
        let mut c = Completion::default();
        let text = "SELECT u. FROM users u";
        c.refresh(text, 9, &source, NO_RESULTS);
        assert!(!c.open, "nothing to show yet");
        let req = c.take_requests();
        assert_eq!(req.len(), 1);
        assert_eq!(req[0].name, "users");
        assert!(!c.poll(text, 9, &source, NO_RESULTS), "not arrived");
        assert!(c.take_requests().is_empty(), "polling requests nothing");

        source.load("users");
        assert!(!c.poll(text, 5, &source, NO_RESULTS), "cursor moved");
        assert!(c.poll(text, 9, &source, NO_RESULTS));
        assert!(c.open);
        assert_eq!(labels(&c), ["id", "email"]);
        assert!(!c.poll(text, 9, &source, NO_RESULTS), "nothing awaited");
    }

    #[test]
    fn closing_stops_waiting_for_columns() {
        let source = Fake::default();
        let mut c = Completion::default();
        c.refresh("SELECT u. FROM users u", 9, &source, NO_RESULTS);
        c.close();
        source.load("users");
        assert!(!c.poll("SELECT u. FROM users u", 9, &source, NO_RESULTS));
        assert!(!c.open);
    }

    #[test]
    fn without_tables_the_last_results_columns_are_offered() {
        let fallback = ["total".to_string()];
        let mut c = Completion::default();
        c.refresh(
            "SELECT to",
            9,
            &NoSource,
            Scope::Script {
                fallback_columns: &fallback,
            },
        );
        assert_eq!(c.items[0].label, "total");
    }

    #[test]
    fn table_scope_offers_columns_then_bar_keywords() {
        let source = Fake::default();
        source.load("users");
        let users = TableRef {
            schema: None,
            name: "users".into(),
            alias: None,
        };
        let mut c = Completion::default();
        let scope = Scope::Table {
            table: &users,
            mode: BarMode::OrderBy,
        };
        c.refresh("", 0, &source, scope);
        assert_eq!(
            labels(&c),
            ["id", "email", "ASC", "DESC", "NULLS FIRST", "NULLS LAST"]
        );
        c.refresh("email D", 7, &source, scope);
        assert_eq!(labels(&c), ["DESC"]);
    }

    #[test]
    fn insertion_uses_the_quoted_text() {
        let source = Fake::default();
        source
            .loaded
            .borrow_mut()
            .insert("users".into(), vec![col("Created At", "timestamp", false)]);
        let mut c = Completion::default();
        let text = "SELECT u.Cr FROM users u";
        c.refresh(text, 11, &source, NO_RESULTS);
        assert_eq!(c.items[0].label, "Created At");
        let (out, _) = apply_suggestion(text, 11, &c.items[0].insert);
        assert_eq!(out, "SELECT u.\"Created At\" FROM users u");
    }

    #[test]
    fn popup_width_fits_the_content_within_bounds() {
        assert_eq!(popup_width(100.0, 600.0), MIN_WIDTH);
        assert_eq!(popup_width(400.0, 600.0), 400.0);
        assert_eq!(popup_width(900.0, 600.0), 600.0);
        assert_eq!(popup_width(900.0, 100.0), MIN_WIDTH, "tiny window");
    }
}
