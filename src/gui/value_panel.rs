//! Right panel showing the focused cell in full: JSON pretty-printed, edits
//! applied to the owning grid's pending edits with "Appliquer" (or when the
//! focus moves to another cell).

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::models::{is_null, Column, ForeignKeyInfo, QueryResult, NULL_CELL};

use super::app::links::LinkOpen;
use super::app::App;
use super::fk::{self, CellLink, ColumnLink, RowState};
use super::grid::changes::{PendingEdits, RowRef};
use super::tabs::data::{busy_hint, DataAction};
use super::tabs::{TabId, TabKind, Tabs};
use super::theme::{ACCENT, ERROR};

/// Warnings of the preview (several matching rows).
const AMBER: egui::Color32 = egui::Color32::from_rgb(0xF2, 0xA6, 0x4C);

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
pub type CellKey = (TabId, u64, RowRef, usize);

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

/// The focus moved to `target`: write what was typed for the previous
/// cell into its pending edits. Why it couldn't be, if so.
fn hand_over(panel: &mut ValuePanel, tabs: &mut Tabs, target: Option<CellKey>) -> Option<String> {
    let (key, generation, value) = panel.take_unapplied(target)?;
    let cell = cell(tabs, key).filter(|f| f.generation == generation);
    match (value, cell) {
        (Ok(v), Some(f)) if f.editable => {
            f.edits.set(f.rows, f.row, f.col, v);
            None
        }
        (Err(e), _) => Some(e),
        _ => Some("la cellule n'est plus modifiable".to_string()),
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let target = focused_key(&app.tabs);
    // Moving the focus applies what was typed for the previous cell, also
    // when the move opens a preview (a link clicked on another cell).
    if let Some(e) = hand_over(&mut app.value_panel, &mut app.tabs, target) {
        app.error(format!("Valeur non appliquée : {e}"));
    }
    // Focusing another cell than the one the links were followed from
    // closes the preview.
    if app
        .fk_preview
        .as_ref()
        .is_some_and(|p| p.origin.is_some() && p.origin != target)
    {
        app.fk_preview = None;
    }
    if app.fk_preview.is_some() {
        if let Some(action) = preview_ui(app, ui) {
            preview_act(app, action);
        }
        return;
    }
    ui.strong("Valeur");
    ui.separator();
    let link = target.and_then(|key| focused_link(app, key));
    let mut follow = None;
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
        if f.column.is_some_and(|c| c.is_primary_key) {
            fk::pk_badge(ui);
        }
        if let Some(l) = &link {
            fk::fk_badge(ui, &l.tooltip());
        }
        if panel.json {
            ui.label(RichText::new("JSON").small().color(ACCENT))
                .on_hover_text("Affiché indenté, écrit compact");
        }
    });
    if let Some(l) = &link {
        let color = fk::link_color(ui.visuals().dark_mode);
        let (tab, _, row, col) = f.key;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("→ {}", l.target)).color(color));
            if ui
                .button(format!("{} Voir la ligne liée", icon::LINK))
                .on_hover_text("Aperçu ici (Alt+Entrée dans la grille)")
                .clicked()
            {
                follow = Some((tab, row, col, false));
            }
            if ui
                .button(icon::ARROW_SQUARE_OUT)
                .on_hover_text(format!("Ouvrir {} dans un onglet (Ctrl+clic)", l.table))
                .clicked()
            {
                follow = Some((tab, row, col, true));
            }
        });
    }

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
    if let Some((tab, row, col, new_tab)) = follow {
        app.data_action(tab, DataAction::Follow { row, col, new_tab });
    }
}

/// The foreign key of the focused cell of a data tab, when its value links
/// somewhere: where it points.
fn focused_link(app: &App, key: CellKey) -> Option<ColumnLink> {
    let (tab_id, _, row, col) = key;
    let tab = app.tabs.list.iter().find(|t| t.id == tab_id)?;
    let TabKind::Data(d) = &tab.kind else {
        return None;
    };
    let details = app
        .sessions
        .get(&tab.connection)?
        .details
        .get(&d.table)?
        .as_ref()
        .ok()?;
    let result = d.result.as_ref()?;
    let link = fk::link_of(&details.foreign_keys, &result.columns.get(col)?.name)?;
    let value = d.saved_values(row)?.into_iter().nth(col)?;
    let saved = !d.grid.edits.is_deleted(row);
    let edited = d.grid.edits.is_edited(row, col);
    (fk::cell_link(Some(&link), is_null(&value), saved, edited) == CellLink::Active).then_some(link)
}

/// What the preview asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewAction {
    /// Back one step (closes the preview from the first one).
    Back,
    Forward,
    /// Jump to step `i` of the breadcrumb.
    Go(usize),
    Close,
    /// Open the current step in a filtered tab.
    OpenTab,
    /// Follow key `fk` of the shown row (a new tab with Ctrl).
    Follow {
        fk: usize,
        new_tab: bool,
    },
    /// Open the rows of incoming key `fk` pointing at the shown row.
    Referencing(usize),
}

/// The linked-row preview: breadcrumb, the row's columns (its own keys
/// clickable) and the tables pointing at it.
fn preview_ui(app: &App, ui: &mut egui::Ui) -> Option<PreviewAction> {
    let p = app.fk_preview.as_ref()?;
    let step = p.current();
    let details = app
        .sessions
        .get(&p.connection)
        .and_then(|s| s.details.get(&step.target.table))
        .and_then(|r| r.as_ref().ok());
    let dark = ui.visuals().dark_mode;
    let color = fk::link_color(dark);
    let mut action = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon::LINK).color(color));
        ui.strong(format!("{} liée", step.target.title));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(icon::X)
                .on_hover_text("Fermer l'aperçu")
                .clicked()
            {
                action = Some(PreviewAction::Close);
            }
        });
    });
    // Breadcrumb: the origin table, then each followed row.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if ui
            .link(RichText::new(&p.origin_title).weak())
            .on_hover_text("Revenir à la cellule")
            .clicked()
        {
            action = Some(PreviewAction::Close);
        }
        for (i, (title, current)) in p.crumbs().into_iter().enumerate() {
            ui.label(RichText::new(icon::CARET_RIGHT).weak());
            if current {
                ui.label(RichText::new(title).strong().color(color));
            } else if ui.link(title).clicked() {
                action = Some(PreviewAction::Go(i));
            }
        }
    });
    ui.horizontal(|ui| {
        if ui
            .button(format!("{} Retour", icon::ARROW_LEFT))
            .on_hover_text("Étape précédente (la première revient à la cellule)")
            .clicked()
        {
            action = Some(PreviewAction::Back);
        }
        if ui
            .add_enabled(p.can_forward(), egui::Button::new(icon::ARROW_RIGHT))
            .on_hover_text("Étape suivante")
            .clicked()
        {
            action = Some(PreviewAction::Forward);
        }
        if ui
            .button(format!("{} Ouvrir dans un onglet", icon::ARROW_SQUARE_OUT))
            .on_hover_text(format!("{} filtré sur cette clé", step.target.title))
            .clicked()
        {
            action = Some(PreviewAction::OpenTab);
        }
    });
    ui.label(
        RichText::new(&step.target.condition)
            .monospace()
            .weak()
            .small(),
    );
    ui.separator();
    match &step.state {
        RowState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Chargement…").weak());
            });
        }
        RowState::Failed(e) => {
            ui.colored_label(ERROR, e);
        }
        RowState::Missing => {
            let notice = step.state.notice().unwrap_or_default();
            ui.colored_label(ERROR, format!("{} {notice}", icon::LINK_BREAK));
        }
        RowState::Found { result, many } => {
            if let (true, Some(notice)) = (*many, step.state.notice()) {
                ui.label(RichText::new(format!("{} {notice}", icon::WARNING)).color(AMBER));
            }
            let fks = details.map_or(&[][..], |d| &d.foreign_keys[..]);
            let incoming = details.map_or(&[][..], |d| &d.incoming[..]);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(a) = preview_row(ui, result, fks, color) {
                        action = Some(a);
                    }
                    if !incoming.is_empty() {
                        ui.add_space(8.0);
                        ui.strong("Référencée par");
                        for (i, fk) in incoming.iter().enumerate() {
                            let text =
                                format!("{} {}", icon::TREE_STRUCTURE, fk::incoming_label(fk));
                            if ui
                                .link(RichText::new(text).color(color))
                                .on_hover_text("Ouvrir les lignes qui pointent ici dans un onglet")
                                .clicked()
                            {
                                action = Some(PreviewAction::Referencing(i));
                            }
                        }
                    }
                });
        }
    }
    action
}

/// The columns of the first row of `result`: name (with its key badges),
/// type and value; values of foreign key columns are links.
fn preview_row(
    ui: &mut egui::Ui,
    result: &QueryResult,
    fks: &[ForeignKeyInfo],
    color: egui::Color32,
) -> Option<PreviewAction> {
    let row = result.rows.first()?;
    let mut action = None;
    egui::Grid::new("fk_preview_row")
        .num_columns(2)
        .striped(true)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            for (c, value) in result.columns.iter().zip(row) {
                let link = fk::link_of(fks, &c.name);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if c.is_primary_key {
                        fk::pk_badge(ui);
                    }
                    if let Some(l) = &link {
                        fk::fk_badge(ui, &l.tooltip());
                    }
                    ui.label(RichText::new(&c.name).strong())
                        .on_hover_text(&c.type_name);
                });
                let null = is_null(value);
                match (&link, null) {
                    (Some(l), false) => {
                        let r = ui
                            .link(RichText::new(format!("{} {value}", icon::LINK)).color(color))
                            .on_hover_text(format!(
                                "{} · clic : suivre · Ctrl+clic : ouvrir dans un onglet",
                                l.tooltip()
                            ));
                        if r.clicked() {
                            let new_tab = ui.input(|i| i.modifiers.command);
                            action = Some(PreviewAction::Follow { fk: l.fk, new_tab });
                        }
                    }
                    (_, true) => {
                        ui.label(RichText::new("NULL").weak().italics());
                    }
                    (None, false) => {
                        ui.add(egui::Label::new(value.as_str()).truncate())
                            .on_hover_text(value.as_str());
                    }
                }
                ui.end_row();
            }
        });
    action
}

/// Do what the preview asked for.
fn preview_act(app: &mut App, action: PreviewAction) {
    let Some(p) = app.fk_preview.as_mut() else {
        return;
    };
    match action {
        PreviewAction::Back => {
            if !p.back() {
                app.fk_preview = None;
            }
        }
        PreviewAction::Forward => p.forward(),
        PreviewAction::Go(i) => p.go(i),
        PreviewAction::Close => app.fk_preview = None,
        PreviewAction::OpenTab => {
            let (connection, target) = (p.connection.clone(), p.current().target.clone());
            app.open_filtered(&connection, &target);
        }
        PreviewAction::Follow { .. } | PreviewAction::Referencing(_) => {
            let connection = p.connection.clone();
            let step = p.current();
            let source = step.target.table.clone();
            let RowState::Found { result, .. } = &step.state else {
                return;
            };
            let (columns, values) = (result.columns.clone(), result.rows[0].clone());
            let details = app
                .sessions
                .get(&connection)
                .and_then(|s| s.details.get(&source))
                .and_then(|r| r.as_ref().ok());
            match action {
                PreviewAction::Follow { fk, new_tab } => {
                    let Some(key) = details.and_then(|d| d.foreign_keys.get(fk)).cloned() else {
                        return;
                    };
                    let open = if new_tab {
                        LinkOpen::Tab
                    } else {
                        LinkOpen::Preview {
                            origin: None,
                            origin_title: String::new(),
                        }
                    };
                    app.follow_fk(&connection, &source, &key, &columns, &values, open);
                }
                PreviewAction::Referencing(i) => {
                    let Some(key) = details.and_then(|d| d.incoming.get(i)).cloned() else {
                        return;
                    };
                    app.open_referencing(&connection, &source, &key, &columns, &values);
                }
                _ => {}
            }
        }
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
    fn text_typed_for_a_cell_is_applied_when_the_focus_moves() {
        use crate::engine::models::{Column, QueryResult};
        use crate::gui::tabs::data::DataTab;
        let column = |name: &str| Column {
            name: name.into(),
            type_name: "TEXT".into(),
            nullable: true,
            is_primary_key: name == "id",
        };
        let mut d = DataTab::new("t".into(), ('"', '"'));
        d.result = Some(QueryResult {
            columns: vec![column("id"), column("name")],
            rows: vec![vec!["1".into(), "a".into()], vec!["2".into(), "b".into()]],
            primary_key: vec!["id".into()],
            ..Default::default()
        });
        let mut tabs = Tabs::default();
        let id = tabs.add("c".into(), "t".into(), TabKind::Data(Box::new(d)));
        let a = (id, 0, RowRef::Base(0), 1);
        let b = (id, 0, RowRef::Base(1), 1);
        let mut p = ValuePanel::default();
        p.sync(a, "a", 0);
        p.buffer = "typed".into();
        assert_eq!(hand_over(&mut p, &mut tabs, Some(a)), None, "still on it");
        // The focus moves to B (say, its link clicked: a preview opens).
        assert_eq!(hand_over(&mut p, &mut tabs, Some(b)), None);
        let TabKind::Data(d) = &tabs.list[0].kind else {
            unreachable!()
        };
        let rows = &d.result.as_ref().unwrap().rows;
        assert_eq!(d.grid.edits.value(rows, RowRef::Base(0), 1), "typed");
        assert_eq!(hand_over(&mut p, &mut tabs, Some(b)), None, "once");
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
