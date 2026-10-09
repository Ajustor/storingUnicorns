//! Right panel showing the focused cell in full: JSON pretty-printed, edits
//! applied to the owning grid's pending edits with "Appliquer" (or when the
//! focus moves to another cell).

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::models::{is_null, Column, NULL_CELL};

use super::app::App;
use super::grid::changes::{PendingEdits, RowRef};
use super::tabs::data::busy_hint;
use super::tabs::{TabId, TabKind, Tabs};
use super::theme::{ACCENT, ERROR};

/// Pretty-printed form of `s` when it is a JSON object or array.
pub fn pretty_json(s: &str) -> Option<String> {
    reformat_json(s, true)
}

/// Compact form of `s` when it is a JSON object or array.
pub fn compact_json(s: &str) -> Option<String> {
    reformat_json(s, false)
}

/// `s` re-indented (two spaces, like `serde_json::to_string_pretty`) or
/// compacted when it is a valid JSON object or array. Only the whitespace
/// between tokens changes: numbers are kept exactly as written (no f64
/// round trip losing digits of a big integer or a decimal), and so are
/// strings and the order of keys.
fn reformat_json(s: &str, pretty: bool) -> Option<String> {
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    // Validates without building values (nor parsing numbers).
    serde_json::from_str::<serde::de::IgnoredAny>(s).ok()?;
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    let newline = |out: &mut String, depth: usize| {
        if pretty {
            out.push('\n');
            for _ in 0..depth {
                out.push_str("  ");
            }
        }
    };
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                out.push('"');
                while let Some(c) = chars.next() {
                    out.push(c);
                    if c == '\\' {
                        if let Some(escaped) = chars.next() {
                            out.push(escaped);
                        }
                    } else if c == '"' {
                        break;
                    }
                }
            }
            '{' | '[' => {
                out.push(ch);
                while chars.next_if(|c| c.is_ascii_whitespace()).is_some() {}
                match chars.next_if(|c| matches!(c, '}' | ']')) {
                    // Empty: `{}` / `[]` on one line.
                    Some(close) => out.push(close),
                    None => {
                        depth += 1;
                        newline(&mut out, depth);
                    }
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(ch);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => {
                out.push(':');
                if pretty {
                    out.push(' ');
                }
            }
            c if c.is_ascii_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}

/// Which cell the panel shows: (tab, console result id or 0, row, column).
type CellKey = (TabId, u64, RowRef, usize);

/// Cheap identity of a shown value: its address and length, and the grid
/// generation. A value that changes is a new string (an edit, a new page),
/// so multi-megabyte values are never hashed or compared every frame.
type ValueId = (usize, usize, u64);

fn value_id(value: &str, generation: u64) -> ValueId {
    (value.as_ptr() as usize, value.len(), generation)
}

/// Editing buffer of the panel, reset whenever the focused cell or its value
/// changes (JSON is reformatted once per reset, not per frame).
#[derive(Default)]
pub struct ValuePanel {
    key: Option<CellKey>,
    value: Option<ValueId>,
    /// Text shown in the editor.
    buffer: String,
    /// `buffer` as loaded; the buffer is dirty when it differs.
    initial: String,
    json: bool,
    null: bool,
    error: Option<String>,
}

impl ValuePanel {
    /// Reload the buffer from `value` when the cell or its value changed.
    fn sync(&mut self, key: CellKey, value: &str, generation: u64) {
        let id = value_id(value, generation);
        if self.key == Some(key) && self.value == Some(id) {
            return;
        }
        self.key = Some(key);
        self.value = Some(id);
        self.error = None;
        self.null = is_null(value);
        let (text, json) = match pretty_json(value) {
            Some(p) => (p, true),
            None if self.null => (String::new(), false),
            None => (value.to_string(), false),
        };
        self.json = json;
        self.initial = text.clone();
        self.buffer = text;
    }

    fn dirty(&self) -> bool {
        self.buffer != self.initial
    }

    /// Value to write on "Appliquer" (compact JSON when it was JSON).
    fn to_apply(&self) -> Result<String, String> {
        if self.json {
            compact_json(&self.buffer).ok_or_else(|| "JSON invalide".to_string())
        } else {
            Ok(self.buffer.clone())
        }
    }

    /// The focus moved from the shown cell to `target`: the changed buffer
    /// not applied yet, to write to the cell it was for (its key and the
    /// grid generation it was loaded under). The panel then forgets it.
    fn take_unapplied(
        &mut self,
        target: Option<CellKey>,
    ) -> Option<(CellKey, u64, Result<String, String>)> {
        let key = self.key.filter(|k| Some(*k) != target)?;
        let generation = self.value.map_or(0, |v| v.2);
        let pending = self.dirty().then(|| self.to_apply());
        self.key = None;
        self.value = None;
        self.buffer = self.initial.clone();
        pending.map(|p| (key, generation, p))
    }
}

/// A cell of a grid and what is needed to edit it.
struct Focus<'a> {
    key: CellKey,
    column: Option<&'a Column>,
    rows: &'a [Vec<String>],
    edits: &'a mut PendingEdits,
    row: RowRef,
    col: usize,
    editable: bool,
    generation: u64,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Valeur");
    ui.separator();
    let target = focused_key(&app.tabs);
    // Moving the focus applies what was typed for the previous cell.
    if let Some((key, generation, value)) = app.value_panel.take_unapplied(target) {
        let cell = cell(&mut app.tabs, key).filter(|f| f.generation == generation);
        let failure = match (value, cell) {
            (Ok(v), Some(f)) if f.editable => {
                f.edits.set(f.rows, f.row, f.col, v);
                None
            }
            (Err(e), _) => Some(e),
            _ => Some("la cellule n'est plus modifiable".to_string()),
        };
        if let Some(e) = failure {
            app.error(format!("Valeur non appliquée : {e}"));
        }
    }
    let panel = &mut app.value_panel;
    let Some(f) = target.and_then(|key| cell(&mut app.tabs, key)) else {
        panel.key = None;
        ui.label(RichText::new("Sélectionnez une cellule.").weak());
        return;
    };
    panel.sync(f.key, f.edits.value(f.rows, f.row, f.col), f.generation);

    ui.horizontal(|ui| {
        match f.column {
            Some(c) => {
                ui.label(RichText::new(&c.name).strong());
                ui.label(RichText::new(&c.type_name).weak());
            }
            None => {
                ui.label(RichText::new(format!("Colonne {}", f.col + 1)).strong());
            }
        }
        if panel.json {
            ui.label(RichText::new("JSON").small().color(ACCENT))
                .on_hover_text("Affiché indenté, écrit compact");
        }
    });

    let mut set = None;
    if f.editable {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(panel.dirty(), egui::Button::new("Appliquer"))
                .on_hover_text(
                    "Écrire la valeur dans les modifications en attente \
                     (aussi en changeant de cellule)",
                )
                .clicked()
            {
                match panel.to_apply() {
                    Ok(v) => set = Some(v),
                    Err(e) => panel.error = Some(e),
                }
            }
            if ui
                .add_enabled(panel.dirty(), egui::Button::new("Rétablir"))
                .clicked()
            {
                panel.buffer = panel.initial.clone();
                panel.error = None;
            }
            if ui
                .add_enabled(
                    !panel.null,
                    egui::Button::new(format!("{} Mettre à NULL", icon::PROHIBIT)),
                )
                .clicked()
            {
                set = Some(NULL_CELL.to_string());
            }
        });
    }
    if let Some(e) = &panel.error {
        ui.colored_label(ERROR, e);
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let hint = if panel.null {
                "NULL"
            } else if f.edits.is_default(f.row, f.col) {
                "DEFAULT"
            } else {
                ""
            };
            fn edit<'t>(text: &'t mut dyn egui::TextBuffer, hint: &str) -> egui::TextEdit<'t> {
                egui::TextEdit::multiline(text)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(12)
                    .hint_text(RichText::new(hint).weak().italics())
            }
            if f.editable {
                ui.add(edit(&mut panel.buffer, hint));
            } else {
                ui.add(edit(&mut panel.buffer.as_str(), hint));
            }
        });

    if let Some(v) = set {
        f.edits.set(f.rows, f.row, f.col, v);
    }
}

/// The focused cell of the active tab.
fn focused_key(tabs: &Tabs) -> Option<CellKey> {
    let tab = tabs.active()?;
    match &tab.kind {
        TabKind::Data(d) => {
            d.result.as_ref()?;
            let (row, col) = d.grid.selected?;
            Some((tab.id, 0, row, col))
        }
        TabKind::Console(c) => {
            let r = c.results.get(c.active_result)?;
            let (row, col) = r.grid.selected?;
            Some((tab.id, r.id, row, col))
        }
        TabKind::Ddl(_) => None,
    }
}

/// Cell `key` when it still exists. Borrows the tabs only, so the panel
/// state can be borrowed alongside.
fn cell(tabs: &mut Tabs, key: CellKey) -> Option<Focus<'_>> {
    let (tab_id, result_id, row, col) = key;
    let tab = tabs.find(tab_id)?;
    let runs = &tab.runs;
    // Read-only while a submit or a reload is pending (see `busy_hint`).
    let (result, grid, editable) = match &mut tab.kind {
        TabKind::Data(d) => {
            let busy = busy_hint(runs.submit.is_some(), runs.page.is_some());
            let editable = d.editable() && busy.is_none();
            (d.result.as_ref()?, &mut d.grid, editable)
        }
        TabKind::Console(c) => {
            let index = c.results.iter().position(|r| r.id == result_id)?;
            let lock = c.result_lock(index, runs.script.is_some());
            let r = &mut c.results[index];
            (&r.result, &mut r.grid, r.editable && lock.is_none())
        }
        TabKind::Ddl(_) => return None,
    };
    let exists = match row {
        RowRef::Base(i) => i < result.rows.len(),
        RowRef::New(k) => k < grid.edits.new_rows(),
    };
    if !exists || col >= result.columns.len() {
        return None;
    }
    let editable = editable && !grid.edits.is_deleted(row);
    Some(Focus {
        key,
        column: result.columns.get(col),
        rows: &result.rows,
        generation: grid.generation,
        edits: &mut grid.edits,
        row,
        col,
        editable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_json_only_for_objects_and_arrays() {
        assert_eq!(
            pretty_json(r#"{"a":1,"b":[true,null]}"#).as_deref(),
            Some("{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null\n  ]\n}")
        );
        assert_eq!(pretty_json("  [1,2]").as_deref(), Some("[\n  1,\n  2\n]"));
        assert_eq!(pretty_json("42"), None, "scalars are not shown as JSON");
        assert_eq!(pretty_json("\"text\""), None);
        assert_eq!(pretty_json("{not json"), None);
        assert_eq!(pretty_json("[1,"), None);
        assert_eq!(pretty_json(""), None);
    }

    #[test]
    fn compact_json_round_trips_pretty_output() {
        let src = r#"{"c":"x y","a":1,"b":[true,null]}"#; // key order kept
        assert_eq!(
            compact_json(&pretty_json(src).unwrap()).as_deref(),
            Some(src)
        );
        assert_eq!(compact_json("{ }").as_deref(), Some("{}"));
        assert_eq!(compact_json("{\"a\":"), None);
        assert_eq!(compact_json("hello"), None);
    }

    #[test]
    fn json_reformatting_keeps_numbers_and_strings_as_written() {
        let src = r#"{"id": 12345678901234567890123, "price": 1.10, "e": 1E400, "n": -0.0}"#;
        assert_eq!(
            compact_json(src).as_deref(),
            Some(r#"{"id":12345678901234567890123,"price":1.10,"e":1E400,"n":-0.0}"#)
        );
        let s = r#"{"s": "a, b: {c} [d] \"q\" \\", "empty": {}, "list": [ ]}"#;
        assert_eq!(
            pretty_json(s).as_deref(),
            Some(
                "{\n  \"s\": \"a, b: {c} [d] \\\"q\\\" \\\\\",\n  \"empty\": {},\n  \"list\": []\n}"
            )
        );
        assert_eq!(compact_json(&pretty_json(s).unwrap()), compact_json(s));
    }

    #[test]
    fn sync_resets_only_when_cell_or_value_changes() {
        let mut p = ValuePanel::default();
        let key = (1, 0, RowRef::Base(0), 2);
        let json = r#"{"a":1}"#.to_string();
        p.sync(key, &json, 0);
        assert!(p.json && !p.null);
        assert_eq!(p.buffer, "{\n  \"a\": 1\n}");
        p.buffer.push_str("edit");
        p.sync(key, &json, 0);
        assert!(p.dirty(), "same cell and value keep the buffer");
        p.sync(key, &json, 1);
        assert!(!p.dirty(), "a new grid generation reloads it");
        p.buffer.push_str("edit");
        let other = json.clone();
        p.sync(key, &other, 1);
        assert!(!p.dirty(), "another value (another string) reloads it");
        p.sync((1, 0, RowRef::Base(1), 2), &json, 1);
        assert!(!p.dirty(), "another cell resets it");
        p.sync((1, 0, RowRef::Base(1), 2), NULL_CELL, 1);
        assert!(p.null && !p.json);
        assert_eq!(p.buffer, "");
        p.sync((1, 0, RowRef::Base(1), 2), "NULL", 1);
        assert!(!p.null, "the text NULL is a value");
        assert_eq!(p.buffer, "NULL");
    }

    #[test]
    fn moving_the_focus_hands_over_the_unapplied_buffer() {
        let mut p = ValuePanel::default();
        let a = (1, 0, RowRef::Base(0), 0);
        let b = (1, 0, RowRef::Base(1), 0);
        assert_eq!(p.take_unapplied(Some(b)), None, "nothing shown");
        p.sync(a, "x", 3);
        assert_eq!(p.take_unapplied(Some(a)), None, "same cell");
        assert_eq!(p.take_unapplied(Some(b)), None, "unchanged buffer");
        p.sync(a, "x", 3);
        p.buffer = "typed".into();
        assert_eq!(p.take_unapplied(Some(a)), None, "still on that cell");
        assert_eq!(p.take_unapplied(None), Some((a, 3, Ok("typed".into()))));
        assert_eq!(p.key, None);
        assert_eq!(p.take_unapplied(None), None, "handed over once");
        p.sync(b, "[1]", 0);
        p.buffer = "[1,".into();
        assert_eq!(
            p.take_unapplied(Some(a)),
            Some((b, 0, Err("JSON invalide".into())))
        );
    }

    #[test]
    fn to_apply_compacts_json_and_rejects_invalid_json() {
        let mut p = ValuePanel::default();
        p.sync((1, 0, RowRef::Base(0), 0), r#"[1, 2]"#, 0);
        p.buffer = "[1,\n 2, 3]".into();
        assert_eq!(p.to_apply(), Ok("[1,2,3]".into()));
        p.buffer = "[1,".into();
        assert!(p.to_apply().is_err());
        p.sync((1, 0, RowRef::Base(0), 1), "plain", 0);
        p.buffer = "plain text".into();
        assert_eq!(p.to_apply(), Ok("plain text".into()));
    }
}
