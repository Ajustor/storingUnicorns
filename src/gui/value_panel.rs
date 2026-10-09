//! Right panel showing the focused cell in full: JSON pretty-printed, edits
//! applied to the owning grid's pending edits with "Appliquer".

use std::hash::{DefaultHasher, Hash, Hasher};

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::models::{is_null, Column, NULL_CELL};

use super::app::App;
use super::grid::changes::{PendingEdits, RowRef};
use super::tabs::{TabId, TabKind};
use super::theme::{ACCENT, ERROR};

/// Pretty-printed form of `s` when it is a JSON object or array.
pub fn pretty_json(s: &str) -> Option<String> {
    let value = parse_json(s)?;
    serde_json::to_string_pretty(&value).ok()
}

/// Compact form of `s` when it is a JSON object or array.
pub fn compact_json(s: &str) -> Option<String> {
    let value = parse_json(s)?;
    serde_json::to_string(&value).ok()
}

fn parse_json(s: &str) -> Option<serde_json::Value> {
    let t = s.trim_start();
    if !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    serde_json::from_str(s).ok()
}

/// Which cell the panel shows: (tab, console result id or 0, row, column).
type CellKey = (TabId, u64, RowRef, usize);

/// Editing buffer of the panel, reset whenever the focused cell or its value
/// changes (JSON is parsed once per reset, not per frame).
#[derive(Default)]
pub struct ValuePanel {
    key: Option<CellKey>,
    value_hash: u64,
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
    fn sync(&mut self, key: CellKey, value: &str) {
        let mut h = DefaultHasher::new();
        value.hash(&mut h);
        let hash = h.finish();
        if self.key == Some(key) && self.value_hash == hash {
            return;
        }
        self.key = Some(key);
        self.value_hash = hash;
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
}

/// The focused cell of the active tab and what is needed to edit it.
struct Focus<'a> {
    key: CellKey,
    column: Option<&'a Column>,
    rows: &'a [Vec<String>],
    edits: &'a mut PendingEdits,
    row: RowRef,
    col: usize,
    editable: bool,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.strong("Valeur");
    ui.separator();
    let panel = &mut app.value_panel;
    let Some(f) = focus(&mut app.tabs) else {
        panel.key = None;
        ui.label(RichText::new("Sélectionnez une cellule.").weak());
        return;
    };
    panel.sync(f.key, f.edits.value(f.rows, f.row, f.col));

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
                .on_hover_text("Écrire la valeur dans les modifications en attente")
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
            let hint = if panel.null { "NULL" } else { "" };
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

/// `focus` on the tabs only, so the panel state can be borrowed alongside.
fn focus(tabs: &mut super::tabs::Tabs) -> Option<Focus<'_>> {
    let tab = tabs.active_mut()?;
    let id = tab.id;
    let (result_id, result, grid, editable) = match &mut tab.kind {
        TabKind::Data(d) => {
            let editable = d.editable();
            (0, d.result.as_ref()?, &mut d.grid, editable)
        }
        TabKind::Console(c) => {
            let r = c.results.get_mut(c.active_result)?;
            (r.id, &r.result, &mut r.grid, r.editable)
        }
        TabKind::Ddl(_) => return None,
    };
    let (row, col) = grid.selected?;
    let editable = editable && !grid.edits.is_deleted(row);
    Some(Focus {
        key: (id, result_id, row, col),
        column: result.columns.get(col),
        rows: &result.rows,
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
    fn sync_resets_only_when_cell_or_value_changes() {
        let mut p = ValuePanel::default();
        let key = (1, 0, RowRef::Base(0), 2);
        p.sync(key, r#"{"a":1}"#);
        assert!(p.json && !p.null);
        assert_eq!(p.buffer, "{\n  \"a\": 1\n}");
        p.buffer.push_str("edit");
        p.sync(key, r#"{"a":1}"#);
        assert!(p.dirty(), "same cell and value keep the buffer");
        p.sync((1, 0, RowRef::Base(1), 2), r#"{"a":1}"#);
        assert!(!p.dirty(), "another cell resets it");
        p.sync((1, 0, RowRef::Base(1), 2), NULL_CELL);
        assert!(p.null && !p.json);
        assert_eq!(p.buffer, "");
        p.sync((1, 0, RowRef::Base(1), 2), "NULL");
        assert!(!p.null, "the text NULL is a value");
        assert_eq!(p.buffer, "NULL");
    }

    #[test]
    fn to_apply_compacts_json_and_rejects_invalid_json() {
        let mut p = ValuePanel::default();
        p.sync((1, 0, RowRef::Base(0), 0), r#"[1, 2]"#);
        p.buffer = "[1,\n 2, 3]".into();
        assert_eq!(p.to_apply(), Ok("[1,2,3]".into()));
        p.buffer = "[1,".into();
        assert!(p.to_apply().is_err());
        p.sync((1, 0, RowRef::Base(0), 1), "plain");
        p.buffer = "plain text".into();
        assert_eq!(p.to_apply(), Ok("plain text".into()));
    }
}
