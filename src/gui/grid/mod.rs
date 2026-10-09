//! Data grid widget (plan 3b, Task 7) and its pending edits model.
pub mod changes;

use std::cmp::Ordering;

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, Key, Modifiers, RichText, Sense, Stroke, StrokeKind};
use egui_extras::{Column as TableColumn, TableBuilder};
use egui_phosphor::regular as icon;

use crate::engine::models::{display_cell, is_null, Column, QueryResult, NULL_CELL};
use crate::engine::ops::rows::detect_system_columns;
use crate::gui::theme::{ACCENT, ERROR, SUCCESS};

use changes::{PendingEdits, RowRef};

/// Indices of `rows` with a cell containing `filter` (case-insensitive),
/// matched against the values shown: pending edits applied, NULL as "NULL".
pub fn matching_rows(rows: &[Vec<String>], edits: &PendingEdits, filter: &str) -> Vec<usize> {
    let needle = filter.to_lowercase();
    (0..rows.len())
        .filter(|&i| {
            needle.is_empty()
                || (0..rows[i].len()).any(|c| {
                    let shown = display_cell(edits.value(rows, RowRef::Base(i), c));
                    shown.to_lowercase().contains(&needle)
                })
        })
        .collect()
}

/// A row as tab-separated values (tabs and newlines inside cells become spaces).
pub fn to_tsv(row: &[String]) -> String {
    row.iter()
        .map(|c| c.replace(['\t', '\n'], " "))
        .collect::<Vec<_>>()
        .join("\t")
}

/// Sort key of a cell: NULL first, then numbers (numerically), then text
/// (case-insensitively). A total order, so sorting mixed columns is sound.
enum SortKey {
    Null,
    Number(f64),
    Text(String),
}

impl SortKey {
    fn of(cell: Option<&String>) -> Self {
        match cell.map(String::as_str) {
            None => Self::Null,
            Some(v) if is_null(v) => Self::Null,
            Some(v) => match v.trim().parse::<f64>() {
                Ok(n) if !n.is_nan() => Self::Number(n),
                _ => Self::Text(v.to_lowercase()),
            },
        }
    }

    fn order(&self, other: &Self) -> Ordering {
        use SortKey::*;
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Null, _) => Ordering::Less,
            (_, Null) => Ordering::Greater,
            (Number(a), Number(b)) => a.total_cmp(b),
            (Number(_), Text(_)) => Ordering::Less,
            (Text(_), Number(_)) => Ordering::Greater,
            (Text(a), Text(b)) => a.cmp(b),
        }
    }
}

/// Every row index, ordered by column `col` (stable).
pub fn sorted_view(rows: &[Vec<String>], col: usize, asc: bool) -> Vec<usize> {
    let mut keyed: Vec<(SortKey, usize)> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (SortKey::of(r.get(col)), i))
        .collect();
    keyed.sort_by(|a, b| {
        let o = a.0.order(&b.0);
        if asc {
            o
        } else {
            o.reverse()
        }
    });
    keyed.into_iter().map(|(_, i)| i).collect()
}

/// Local sort after a click on column `col`: asc → desc → none; another
/// column starts ascending.
pub fn next_sort(current: Option<(usize, bool)>, col: usize) -> Option<(usize, bool)> {
    match current {
        Some((c, true)) if c == col => Some((col, false)),
        Some((c, false)) if c == col => None,
        _ => Some((col, true)),
    }
}

/// State of one grid, kept by its owner (console result tab, data tab)
/// across frames.
pub struct GridState {
    /// Focused cell.
    pub selected: Option<(RowRef, usize)>,
    /// Cell being edited and its text.
    pub editing: Option<(RowRef, usize, String)>,
    /// The edited cell was NULL, or a new row's untouched cell: its editor
    /// starts empty and, left empty, keeps the cell as it was.
    editing_blank: bool,
    pub edits: PendingEdits,
    /// Client-side view: indices of base rows after filter/sort (consoles only).
    pub view: Vec<usize>,
    pub filter: String,
    /// Client-side sort (consoles): column, ascending.
    pub sort: Option<(usize, bool)>,
    view_dirty: bool,
    /// Rows (address, length) the view was computed for: a replaced result
    /// recomputes it even if the owner forgot `new_result`.
    view_source: (usize, usize),
    /// The cell editor has not been drawn yet: on its first frame it takes
    /// the focus and selects its whole text. Focus is never requested before
    /// the editor exists (AccessKit panics on a focused id missing from its
    /// tree, e.g. when the edit starts after the table was drawn).
    select_all: bool,
    /// Scroll the focused cell into view (after a keyboard move).
    scroll_to_selected: bool,
    /// Bumped whenever row references may point to other rows (a new
    /// result, dropped or removed new rows): a `RowRef` taken under another
    /// generation must not be written to.
    pub generation: u64,
}

impl Default for GridState {
    fn default() -> Self {
        Self {
            selected: None,
            editing: None,
            editing_blank: false,
            edits: PendingEdits::default(),
            view: Vec::new(),
            filter: String::new(),
            sort: None,
            view_dirty: true,
            view_source: (0, 0),
            select_all: false,
            scroll_to_selected: false,
            generation: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridAction {
    None,
    /// Header clicked on a server-sorted grid (data tab): column index.
    SortBy(usize),
    /// Apply `edits` (Ctrl+Entrée or the Submit button); a cell being
    /// edited has been committed first.
    Submit,
    /// The pending edits were dropped by the grid (Ctrl+Z or Revert).
    Revert,
    SelectionChanged,
    /// "Exporter…" in the footer.
    Export,
}

pub struct GridOptions<'a> {
    pub result: &'a QueryResult,
    pub editable: bool,
    /// true → header clicks emit SortBy (server side); false → sort locally.
    pub server_sort: bool,
    /// Sort arrow of a server-sorted grid.
    pub sort_indicator: Option<(usize, bool)>,
}

/// Row shown at display position `d`: the view, then the new rows.
fn row_ref(view: &[usize], d: usize) -> RowRef {
    if d < view.len() {
        RowRef::Base(view[d])
    } else {
        RowRef::New(d - view.len())
    }
}

fn display_pos(view: &[usize], r: RowRef) -> Option<usize> {
    match r {
        RowRef::Base(i) => view.iter().position(|&v| v == i),
        RowRef::New(k) => Some(view.len() + k),
    }
}

const AMBER: Color32 = Color32::from_rgb(0xF2, 0xA6, 0x4C);
/// Longest text laid out in a cell (the value panel shows it all).
const MAX_CELL_CHARS: usize = 256;

fn edited_bg() -> Color32 {
    AMBER.gamma_multiply(0.25)
}

fn new_bg() -> Color32 {
    SUCCESS.gamma_multiply(0.15)
}

fn plural_rows(n: usize) -> String {
    format!("{n} {}", if n == 1 { "ligne" } else { "lignes" })
}

/// Where typing starts in a new row: the first column that is neither
/// filled by the database (`detect_system_columns`) nor part of the key.
pub fn first_input_column(columns: &[Column]) -> usize {
    let system = detect_system_columns(columns);
    (0..columns.len())
        .find(|i| !system.contains(i) && !columns[*i].is_primary_key)
        .or_else(|| (0..columns.len()).find(|i| !system.contains(i)))
        .unwrap_or(0)
}

/// Text shown for a cell and whether it is NULL (drawn weak and italic):
/// the text "NULL" is an ordinary value.
pub fn cell_display(value: &str) -> (&str, bool) {
    (display_cell(value), is_null(value))
}

fn cell_text(value: &str, deleted: bool) -> RichText {
    let text = if let (shown, true) = cell_display(value) {
        RichText::new(shown).weak().italics()
    } else {
        match value.char_indices().nth(MAX_CELL_CHARS) {
            Some((end, _)) => RichText::new(format!("{}…", &value[..end])),
            None => RichText::new(value),
        }
    };
    if deleted {
        text.color(ERROR).strikethrough()
    } else {
        text
    }
}

/// What a click or the context menu asked for, applied after the table.
enum CellAction {
    Select(RowRef, usize),
    Edit(RowRef, usize),
    CopyValue(RowRef, usize),
    CopyRow(RowRef),
    SetNull(RowRef, usize),
    /// Set the empty string.
    SetEmpty(RowRef, usize),
    /// A new row's cell back to its default.
    SetDefault(RowRef, usize),
    AddRow,
    ToggleDelete(RowRef),
}

impl GridState {
    /// The owner replaced the result (refresh, new page): drop the edits
    /// and the focus, keep the filter and the sort.
    pub fn new_result(&mut self) {
        self.selected = None;
        self.editing = None;
        self.edits.clear();
        self.view_dirty = true;
        self.generation += 1;
    }

    fn ensure_view(&mut self, rows: &[Vec<String>]) {
        let source = (rows.as_ptr() as usize, rows.len());
        if !self.view_dirty && self.view_source == source {
            return;
        }
        let filtered = matching_rows(rows, &self.edits, &self.filter);
        self.view = match self.sort {
            Some((col, asc)) if filtered.len() == rows.len() => sorted_view(rows, col, asc),
            Some((col, asc)) => {
                let mut keep = vec![false; rows.len()];
                for &i in &filtered {
                    keep[i] = true;
                }
                let mut view = sorted_view(rows, col, asc);
                view.retain(|&i| keep[i]);
                view
            }
            None => filtered,
        };
        self.view_dirty = false;
        self.view_source = source;
    }

    fn row_exists(&self, rows: &[Vec<String>], r: RowRef) -> bool {
        match r {
            RowRef::Base(i) => i < rows.len(),
            RowRef::New(k) => k < self.edits.new_rows(),
        }
    }

    /// Forget a focus or an edit on a row that no longer exists.
    fn clamp(&mut self, rows: &[Vec<String>], ncols: usize) {
        if let Some((r, c)) = self.selected {
            if c >= ncols || !self.row_exists(rows, r) {
                self.selected = None;
            }
        }
        if let Some((r, c, _)) = &self.editing {
            if *c >= ncols || !self.row_exists(rows, *r) {
                self.editing = None;
            }
        }
    }

    fn total_rows(&self) -> usize {
        self.view.len() + self.edits.new_rows()
    }

    fn select(&mut self, r: RowRef, c: usize) {
        self.selected = Some((r, c));
        self.scroll_to_selected = true;
    }

    /// Move the focus by `dr` rows / `dc` columns; `wrap` continues on the
    /// next/previous row past the last/first column (Tab).
    fn move_focus(&mut self, ncols: usize, dr: isize, dc: isize, wrap: bool) {
        let total = self.total_rows();
        if total == 0 || ncols == 0 {
            return;
        }
        let Some((pos, col)) = self
            .selected
            .and_then(|(r, c)| Some((display_pos(&self.view, r)?, c)))
        else {
            self.select(row_ref(&self.view, 0), 0);
            return;
        };
        let (mut pos, mut col) = (pos as isize + dr, col as isize + dc);
        if wrap {
            if col >= ncols as isize {
                col = 0;
                pos += 1;
            } else if col < 0 {
                col = ncols as isize - 1;
                pos -= 1;
            }
        }
        let pos = pos.clamp(0, total as isize - 1) as usize;
        let col = col.clamp(0, ncols as isize - 1) as usize;
        self.select(row_ref(&self.view, pos), col);
    }

    fn start_edit(&mut self, ctx: &egui::Context, rows: &[Vec<String>], r: RowRef, c: usize) {
        if self.edits.is_deleted(r) {
            return;
        }
        let value = self.edits.value(rows, r, c);
        self.editing_blank = is_null(value) || self.edits.is_default(r, c);
        let text = if is_null(value) { "" } else { value };
        self.editing = Some((r, c, text.to_string()));
        self.select(r, c);
        self.select_all = true;
        ctx.request_repaint();
    }

    /// Write the open cell editor's text into `edits` and close it, so that
    /// pending work is complete before deciding whether it can be dropped.
    pub fn commit_editing(&mut self, rows: &[Vec<String>]) {
        self.commit_edit(rows);
    }

    fn commit_edit(&mut self, rows: &[Vec<String>]) {
        if let Some((r, c, text)) = self.editing.take() {
            if !(self.editing_blank && text.is_empty()) {
                self.edits.set(rows, r, c, text);
            }
        }
    }

    /// Alt+Insert: a new row, editing its first cell that the database
    /// does not seem to fill itself (`detect_system_columns`: only a hint,
    /// any cell can be typed).
    fn add_row(&mut self, ctx: &egui::Context, result: &QueryResult) {
        self.commit_edit(&result.rows);
        let r = self.edits.add_row(result.columns.len());
        self.start_edit(ctx, &result.rows, r, first_input_column(&result.columns));
    }

    /// Mark or unmark row `r` deleted; a new row is removed, and the later
    /// new rows shift up: the focus and an open cell editor follow their row
    /// (they are dropped when on the removed row).
    fn toggle_delete(&mut self, r: RowRef) {
        self.edits.toggle_delete(r);
        let RowRef::New(removed) = r else {
            return;
        };
        self.generation += 1;
        let follow = |row: RowRef| match row {
            RowRef::New(j) if j == removed => None,
            RowRef::New(j) if j > removed => Some(RowRef::New(j - 1)),
            other => Some(other),
        };
        self.selected = self.selected.and_then(|(row, c)| Some((follow(row)?, c)));
        self.editing = self
            .editing
            .take()
            .and_then(|(row, c, text)| Some((follow(row)?, c, text)));
    }

    /// Drop every pending edit and the open cell editor.
    pub fn discard_edits(&mut self) {
        self.revert();
    }

    fn revert(&mut self) {
        self.editing = None;
        self.edits.clear();
        self.generation += 1;
    }

    /// Keys while a cell is being edited. The cell editor loses its focus on
    /// Escape before this runs, hence Escape is checked before the focus.
    fn editing_keys(
        &mut self,
        ctx: &egui::Context,
        (focus_id, edit_id): (egui::Id, egui::Id),
        result: &QueryResult,
    ) -> GridAction {
        let rows = &result.rows;
        let (escape, submit, enter, back, tab) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::COMMAND, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Tab),
            )
        });
        let focus_grid = || ctx.memory_mut(|m| m.request_focus(focus_id));
        if escape {
            self.editing = None;
            focus_grid();
        } else if submit {
            self.commit_edit(rows);
            focus_grid();
            return GridAction::Submit;
        } else if enter {
            self.commit_edit(rows);
            focus_grid();
        } else if tab || back {
            self.commit_edit(rows);
            self.move_focus(result.columns.len(), 0, if back { -1 } else { 1 }, true);
            if let Some((r, c)) = self.selected {
                self.start_edit(ctx, rows, r, c);
            }
        } else if !self.select_all && !ctx.memory(|m| m.has_focus(edit_id)) {
            // Clicked elsewhere, or scrolled away: focus loss commits.
            self.commit_edit(rows);
        }
        GridAction::None
    }

    /// Keys while the grid itself has the focus.
    fn grid_keys(
        &mut self,
        ctx: &egui::Context,
        result: &QueryResult,
        editable: bool,
    ) -> GridAction {
        let ncols = result.columns.len();
        let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let moves = [
            (Key::ArrowUp, -1, 0),
            (Key::ArrowDown, 1, 0),
            (Key::ArrowLeft, 0, -1),
            (Key::ArrowRight, 0, 1),
            (Key::PageUp, -20, 0),
            (Key::PageDown, 20, 0),
        ];
        for (k, dr, dc) in moves {
            if key(Modifiers::NONE, k) {
                self.move_focus(ncols, dr, dc, false);
            }
        }
        if key(Modifiers::SHIFT, Key::Tab) {
            self.move_focus(ncols, 0, -1, true);
        }
        if key(Modifiers::NONE, Key::Tab) {
            self.move_focus(ncols, 0, 1, true);
        }
        let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if let (true, Some((r, c))) = (copy, self.selected) {
            ctx.copy_text(display_cell(self.edits.value(&result.rows, r, c)).to_string());
        }
        if !editable {
            return GridAction::None;
        }
        if key(Modifiers::ALT, Key::Insert) {
            self.add_row(ctx, result);
            return GridAction::None;
        }
        if let Some((r, c)) = self.selected {
            if key(Modifiers::NONE, Key::F2) || key(Modifiers::NONE, Key::Enter) {
                self.start_edit(ctx, &result.rows, r, c);
            } else if key(Modifiers::COMMAND, Key::Delete) {
                self.toggle_delete(r);
            }
        }
        if key(Modifiers::COMMAND, Key::Enter) && !self.edits.is_empty() {
            return GridAction::Submit;
        }
        if key(Modifiers::COMMAND, Key::Z) && !self.edits.is_empty() {
            self.revert();
            return GridAction::Revert;
        }
        GridAction::None
    }
}

/// Virtualised data grid: sortable header, local filter (unless
/// `server_sort`), focused cell, inline editing into `state.edits` when
/// `editable`, and a footer with the row count and Submit / Revert.
pub fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    state: &mut GridState,
    opts: GridOptions,
) -> GridAction {
    let result = opts.result;
    let rows = &result.rows;
    let ncols = result.columns.len();
    if ncols == 0 {
        return GridAction::None;
    }
    let ctx = ui.ctx().clone();
    let focus_id = id.with("focus");
    let edit_id = id.with("edit");
    let before = state.selected;
    state.ensure_view(rows);
    state.clamp(rows, ncols);
    if !opts.editable {
        // Became read-only while a cell was edited: keep what was typed.
        state.commit_editing(rows);
    }

    // Keyboard target of the grid; it senses no clicks.
    ui.interact(
        ui.available_rect_before_wrap(),
        focus_id,
        Sense::focusable_noninteractive(),
    );
    let grid_focused = ctx.memory(|m| m.has_focus(focus_id));
    if grid_focused {
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                focus_id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            )
        });
    }
    let mut action = if state.editing.is_some() {
        state.editing_keys(&ctx, (focus_id, edit_id), result)
    } else if grid_focused {
        state.grid_keys(&ctx, result, opts.editable)
    } else {
        GridAction::None
    };

    if !opts.server_sort {
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut state.filter)
                    .id(id.with("filter"))
                    .hint_text(format!("{} Filtrer", icon::FUNNEL))
                    .desired_width(220.0),
            );
            if r.changed() {
                state.view_dirty = true;
            }
        });
        state.ensure_view(rows);
    }

    let footer_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0;
    let table_h = (ui.available_height() - footer_h).max(40.0);
    let sort_shown = if opts.server_sort {
        opts.sort_indicator
    } else {
        state.sort
    };
    let mut header_clicked = None;
    let mut cell_action = None;
    let scroll_pos = if std::mem::take(&mut state.scroll_to_selected) {
        state
            .selected
            .and_then(|(r, _)| display_pos(&state.view, r))
    } else {
        None
    };
    let total = state.total_rows();
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let num_w = 8.0 * (total.max(1).ilog10() as f32 + 1.0) + 18.0;
    ui.allocate_ui(egui::vec2(ui.available_width(), table_h), |ui| {
        let GridState {
            selected,
            editing,
            edits,
            view,
            select_all,
            ..
        } = &mut *state;
        egui::ScrollArea::horizontal()
            .id_salt(id.with("h"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut table = TableBuilder::new(ui)
                    .id_salt(id.with("table"))
                    .striped(true)
                    .resizable(true)
                    .sense(Sense::click())
                    .auto_shrink([false, false])
                    .max_scroll_height(f32::INFINITY)
                    // Default 200 px would push the footer out of a short area.
                    .min_scrolled_height(0.0)
                    .animate_scrolling(false)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(TableColumn::exact(num_w))
                    .columns(TableColumn::initial(140.0).at_least(40.0).clip(true), ncols);
                if let Some(pos) = scroll_pos {
                    table = table.scroll_to_row(pos, None);
                }
                table
                    .header(row_h + 2.0, |mut header| {
                        header.col(|ui| {
                            ui.label(RichText::new("#").weak());
                        });
                        for (c, column) in result.columns.iter().enumerate() {
                            header.col(|ui| {
                                let mut text = String::new();
                                if column.is_primary_key {
                                    text.push_str(icon::KEY);
                                    text.push(' ');
                                }
                                text.push_str(&column.name);
                                if let Some((_, asc)) = sort_shown.filter(|(sc, _)| *sc == c) {
                                    text.push(' ');
                                    text.push_str(if asc {
                                        icon::CARET_UP
                                    } else {
                                        icon::CARET_DOWN
                                    });
                                }
                                let r = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).strong())
                                            .selectable(false)
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_text(format!(
                                        "{} · cliquer pour trier",
                                        column.type_name
                                    ));
                                if r.clicked() {
                                    header_clicked = Some(c);
                                }
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(row_h, total, |mut row| {
                            let d = row.index();
                            let r = row_ref(view, d);
                            let is_new = matches!(r, RowRef::New(_));
                            let deleted = edits.is_deleted(r);
                            row.set_selected(selected.is_some_and(|(sr, _)| sr == r));
                            row.col(|ui| {
                                if is_new {
                                    ui.label(RichText::new(icon::PLUS).color(SUCCESS));
                                } else {
                                    ui.label(RichText::new((d + 1).to_string()).weak());
                                }
                            });
                            for c in 0..ncols {
                                let (_, resp) = row.col(|ui| {
                                    let rect = ui.max_rect();
                                    if is_new {
                                        ui.painter().rect_filled(rect, 0.0, new_bg());
                                    } else if edits.is_edited(r, c) {
                                        ui.painter().rect_filled(rect, 0.0, edited_bg());
                                    }
                                    match editing.as_mut() {
                                        Some((er, ec, text)) if *er == r && *ec == c => {
                                            let mut out = egui::TextEdit::singleline(text)
                                                .id(edit_id)
                                                .frame(false)
                                                .lock_focus(true)
                                                .margin(egui::vec2(2.0, 0.0))
                                                .desired_width(f32::INFINITY)
                                                .show(ui);
                                            if std::mem::take(select_all) {
                                                out.response.request_focus();
                                                let n = text.chars().count();
                                                out.state.cursor.set_char_range(Some(
                                                    CCursorRange::two(
                                                        CCursor::new(0),
                                                        CCursor::new(n),
                                                    ),
                                                ));
                                                out.state.store(ui.ctx(), edit_id);
                                            }
                                        }
                                        _ => {
                                            let value = edits.value(rows, r, c);
                                            let text = if edits.is_default(r, c) {
                                                RichText::new("DEFAULT").weak().italics()
                                            } else {
                                                cell_text(value, deleted)
                                            };
                                            ui.add(
                                                egui::Label::new(text).selectable(false).truncate(),
                                            );
                                        }
                                    }
                                    if *selected == Some((r, c)) {
                                        ui.painter().rect_stroke(
                                            rect.shrink(1.0),
                                            2.0,
                                            Stroke::new(1.5, ACCENT),
                                            StrokeKind::Inside,
                                        );
                                        if scroll_pos.is_some() {
                                            ui.scroll_to_rect(rect, None);
                                        }
                                    }
                                });
                                if resp.double_clicked() {
                                    cell_action = Some(CellAction::Edit(r, c));
                                } else if resp.clicked() || resp.secondary_clicked() {
                                    cell_action = Some(CellAction::Select(r, c));
                                }
                                resp.context_menu(|ui| {
                                    context_menu(ui, r, c, deleted, opts.editable, &mut cell_action)
                                });
                            }
                        });
                    });
            });
    });

    match footer(ui, state, result, opts.editable, &mut cell_action) {
        GridAction::Submit => {
            state.commit_edit(rows);
            action = GridAction::Submit;
        }
        GridAction::None => {}
        a => action = a,
    }

    if let Some(c) = header_clicked {
        if opts.server_sort {
            action = GridAction::SortBy(c);
        } else {
            state.sort = next_sort(state.sort, c);
            state.view_dirty = true;
        }
    }
    let focus_grid = || ctx.memory_mut(|m| m.request_focus(focus_id));
    match cell_action {
        Some(CellAction::Select(r, c)) => {
            let other = |e: &(RowRef, usize, String)| (e.0, e.1) != (r, c);
            if state.editing.as_ref().is_some_and(other) {
                state.commit_edit(rows);
            }
            if state.editing.is_none() {
                focus_grid();
            }
            state.selected = Some((r, c));
        }
        Some(CellAction::Edit(r, c)) => {
            if opts.editable {
                state.commit_edit(rows);
                state.start_edit(&ctx, rows, r, c);
            } else {
                state.selected = Some((r, c));
                focus_grid();
            }
        }
        Some(CellAction::CopyValue(r, c)) => {
            ctx.copy_text(display_cell(state.edits.value(rows, r, c)).to_string());
        }
        Some(CellAction::CopyRow(r)) => {
            let row: Vec<String> = (0..ncols)
                .map(|c| display_cell(state.edits.value(rows, r, c)).to_string())
                .collect();
            ctx.copy_text(to_tsv(&row));
        }
        Some(CellAction::SetEmpty(r, c)) => {
            state.commit_edit(rows);
            state.edits.set(rows, r, c, String::new());
            state.selected = Some((r, c));
            focus_grid();
        }
        Some(CellAction::SetDefault(r, c)) => {
            state.commit_edit(rows);
            state.edits.reset_default(r, c);
            state.selected = Some((r, c));
            focus_grid();
        }
        Some(CellAction::SetNull(r, c)) => {
            state.commit_edit(rows);
            state.edits.set(rows, r, c, NULL_CELL.into());
            state.selected = Some((r, c));
            focus_grid();
        }
        Some(CellAction::AddRow) => state.add_row(&ctx, result),
        Some(CellAction::ToggleDelete(r)) => {
            state.commit_edit(rows);
            state.toggle_delete(r);
            focus_grid();
        }
        None => {}
    }
    state.clamp(rows, ncols);
    if action == GridAction::None && state.selected != before {
        action = GridAction::SelectionChanged;
    }
    action
}

fn context_menu(
    ui: &mut egui::Ui,
    r: RowRef,
    c: usize,
    deleted: bool,
    editable: bool,
    action: &mut Option<CellAction>,
) {
    let mut item = |ui: &mut egui::Ui, label: String, a: CellAction| {
        if ui.button(label).clicked() {
            *action = Some(a);
            ui.close_menu();
        }
    };
    item(
        ui,
        format!("{} Copier la valeur", icon::COPY),
        CellAction::CopyValue(r, c),
    );
    item(
        ui,
        format!("{} Copier la ligne", icon::COPY),
        CellAction::CopyRow(r),
    );
    if !editable {
        return;
    }
    ui.separator();
    if !deleted {
        item(
            ui,
            format!("{} Modifier (F2)", icon::PENCIL_SIMPLE),
            CellAction::Edit(r, c),
        );
        item(
            ui,
            format!("{} Mettre à NULL", icon::PROHIBIT),
            CellAction::SetNull(r, c),
        );
        item(
            ui,
            format!("{} Chaîne vide", icon::TEXT_AA),
            CellAction::SetEmpty(r, c),
        );
        if matches!(r, RowRef::New(_)) {
            item(
                ui,
                format!("{} Valeur par défaut", icon::ARROW_COUNTER_CLOCKWISE),
                CellAction::SetDefault(r, c),
            );
        }
    }
    item(
        ui,
        format!("{} Ajouter une ligne (Alt+Inser)", icon::PLUS),
        CellAction::AddRow,
    );
    let label = if deleted {
        format!("{} Annuler la suppression", icon::ARROW_COUNTER_CLOCKWISE)
    } else {
        format!("{} Supprimer la ligne (Ctrl+Suppr)", icon::TRASH)
    };
    item(ui, label, CellAction::ToggleDelete(r));
}

/// Row count and pending changes: Submit or Revert (applied here) clicked.
fn footer(
    ui: &mut egui::Ui,
    state: &mut GridState,
    result: &QueryResult,
    editable: bool,
    cell_action: &mut Option<CellAction>,
) -> GridAction {
    let mut action = GridAction::None;
    ui.horizontal(|ui| {
        if editable {
            // Also the only way to add the first row of an empty table.
            if ui
                .small_button(RichText::new(icon::PLUS).color(SUCCESS))
                .on_hover_text("Ajouter une ligne (Alt+Inser)")
                .clicked()
            {
                *cell_action = Some(CellAction::AddRow);
            }
            let selected = state.selected.map(|(r, _)| r);
            if ui
                .add_enabled(
                    selected.is_some(),
                    egui::Button::new(RichText::new(icon::MINUS).color(ERROR)).small(),
                )
                .on_hover_text("Supprimer la ligne sélectionnée (Ctrl+Suppr)")
                .clicked()
            {
                if let Some(r) = selected {
                    *cell_action = Some(CellAction::ToggleDelete(r));
                }
            }
            ui.separator();
        }
        let n = result.rows.len();
        let mut text = if state.view.len() == n {
            plural_rows(n)
        } else {
            format!("{} / {}", state.view.len(), plural_rows(n))
        };
        if result.truncated {
            text.push_str(&format!(" · {n}+ (limité)"));
        }
        ui.label(RichText::new(text).weak());
        if ui
            .small_button(format!("{} Exporter…", icon::EXPORT))
            .on_hover_text("Exporter ces lignes en CSV ou SQL INSERT")
            .clicked()
        {
            action = GridAction::Export;
        }
        if state.edits.is_empty() {
            return;
        }
        ui.separator();
        ui.label(RichText::new(format!("{} modification(s)", state.edits.count())).color(AMBER));
        // Read-only (a submit or a reload is running): nothing to apply or drop.
        if !editable {
            return;
        }
        if ui
            .button(RichText::new(format!("{} Submit", icon::CHECK)).color(SUCCESS))
            .on_hover_text("Appliquer dans une transaction (Ctrl+Entrée)")
            .clicked()
        {
            action = GridAction::Submit;
        }
        if ui
            .button(format!("{} Revert", icon::ARROW_COUNTER_CLOCKWISE))
            .on_hover_text("Abandonner les modifications (Ctrl+Z)")
            .clicked()
        {
            state.revert();
            action = GridAction::Revert;
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec!["1".into(), "Alice".into()],
            vec!["2".into(), "bob".into()],
            vec!["3".into(), "Carol".into()],
        ]
    }

    fn col(values: &[&str]) -> Vec<Vec<String>> {
        values.iter().map(|v| vec![v.to_string()]).collect()
    }

    #[test]
    fn filter_matches_any_cell_case_insensitively() {
        let none = PendingEdits::default();
        assert_eq!(matching_rows(&rows(), &none, "B"), vec![1]);
        assert_eq!(matching_rows(&rows(), &none, "3"), vec![2]);
        assert_eq!(matching_rows(&rows(), &none, ""), vec![0, 1, 2]);
        assert!(matching_rows(&rows(), &none, "zzz").is_empty());
    }

    #[test]
    fn filter_matches_the_values_shown() {
        let r = rows();
        let mut edits = PendingEdits::default();
        edits.set(&r, RowRef::Base(0), 1, "Zoe".into());
        assert_eq!(matching_rows(&r, &edits, "zoe"), vec![0], "edited value");
        assert!(
            matching_rows(&r, &edits, "alice").is_empty(),
            "replaced value"
        );
        let nulls = col(&[NULL_CELL, "x"]);
        let none = PendingEdits::default();
        assert_eq!(
            matching_rows(&nulls, &none, "null"),
            vec![0],
            "shown as NULL"
        );
        assert!(
            matching_rows(&nulls, &none, "\u{E000}").is_empty(),
            "no sentinel"
        );
    }

    #[test]
    fn removing_a_new_row_keeps_focus_and_editor_on_their_rows() {
        let mut g = GridState::default();
        for _ in 0..3 {
            g.edits.add_row(2);
        }
        g.selected = Some((RowRef::New(2), 1));
        g.editing = Some((RowRef::New(1), 0, "typed".into()));
        g.toggle_delete(RowRef::New(0));
        assert_eq!(g.edits.new_rows(), 2);
        assert_eq!(g.selected, Some((RowRef::New(1), 1)), "followed its row");
        assert_eq!(
            g.editing.as_ref().map(|e| (e.0, e.1, e.2.as_str())),
            Some((RowRef::New(0), 0, "typed"))
        );
        // The focused row itself is removed: no focus on another row.
        g.toggle_delete(RowRef::New(1));
        assert_eq!(g.selected, None);
        assert_eq!(g.editing.as_ref().map(|e| e.0), Some(RowRef::New(0)));
        g.toggle_delete(RowRef::New(0));
        assert!(g.editing.is_none());
        // Base rows don't shift.
        g.selected = Some((RowRef::Base(1), 0));
        g.toggle_delete(RowRef::Base(0));
        assert_eq!(g.selected, Some((RowRef::Base(1), 0)));
    }

    #[test]
    fn row_as_tsv() {
        assert_eq!(
            to_tsv(&["a".into(), "b\tc".into(), "d\ne".into()]),
            "a\tb c\td e"
        );
    }

    #[test]
    fn sorts_numbers_numerically_and_text_case_insensitively() {
        let r = col(&["10", "9", "-1.5", "100"]);
        assert_eq!(sorted_view(&r, 0, true), vec![2, 1, 0, 3]);
        assert_eq!(sorted_view(&r, 0, false), vec![3, 0, 1, 2]);
        let r = col(&["b", "A", "c"]);
        assert_eq!(sorted_view(&r, 0, true), vec![1, 0, 2]);
    }

    #[test]
    fn sort_puts_null_first_then_numbers_then_text_and_is_stable() {
        let r = col(&["x", "2", NULL_CELL, "a", "1", "x", NULL_CELL]);
        assert_eq!(sorted_view(&r, 0, true), vec![2, 6, 4, 1, 3, 0, 5]);
        // The text "NULL" is text.
        let t = col(&["NULL", NULL_CELL, "A"]);
        assert_eq!(sorted_view(&t, 0, true), vec![1, 2, 0]);
        // Descending reverses the order of keys, equal keys keep theirs.
        assert_eq!(sorted_view(&r, 0, false), vec![0, 5, 3, 1, 4, 2, 6]);
        // Out-of-range column: unchanged order.
        assert_eq!(sorted_view(&col(&["b", "a"]), 3, true), vec![0, 1]);
    }

    #[test]
    fn null_cells_are_shown_as_null_apart_from_the_text_null() {
        assert_eq!(cell_display(NULL_CELL), ("NULL", true));
        assert_eq!(cell_display("NULL"), ("NULL", false));
        assert_eq!(cell_display(""), ("", false));
    }

    #[test]
    fn editing_a_null_cell_starts_empty_and_keeps_null_unless_typed() {
        let ctx = egui::Context::default();
        let rows = col(&[NULL_CELL, "NULL"]);
        let mut g = GridState::default();
        g.start_edit(&ctx, &rows, RowRef::Base(0), 0);
        assert_eq!(
            g.editing.as_ref().unwrap().2,
            "",
            "no sentinel in the editor"
        );
        g.commit_edit(&rows);
        assert!(g.edits.is_empty(), "left empty: still NULL");
        g.start_edit(&ctx, &rows, RowRef::Base(0), 0);
        g.editing.as_mut().unwrap().2 = "x".into();
        g.commit_edit(&rows);
        assert_eq!(g.edits.value(&rows, RowRef::Base(0), 0), "x");
        // The text "NULL" is edited as text.
        g.start_edit(&ctx, &rows, RowRef::Base(1), 0);
        assert_eq!(g.editing.as_ref().unwrap().2, "NULL");
        g.editing.as_mut().unwrap().2 = "".into();
        g.commit_edit(&rows);
        assert_eq!(g.edits.value(&rows, RowRef::Base(1), 0), "");
    }

    #[test]
    fn new_row_cells_stay_default_unless_typed_and_typing_starts_after_ids() {
        let ctx = egui::Context::default();
        let column = |name: &str, ty: &str| Column {
            name: name.into(),
            type_name: ty.into(),
            nullable: true,
            is_primary_key: name == "id",
        };
        let result = QueryResult {
            columns: vec![
                column("id", "int"),
                column("created_at", "timestamp"),
                column("name", "text"),
            ],
            rows: vec![],
            ..Default::default()
        };
        assert_eq!(first_input_column(&result.columns), 2);
        let mut g = GridState::default();
        g.add_row(&ctx, &result);
        let r = RowRef::New(0);
        assert_eq!(g.editing.as_ref().map(|e| (e.0, e.1)), Some((r, 2)));
        g.commit_edit(&result.rows);
        assert!(g.edits.is_default(r, 2), "left empty: still the default");
        // A system column can be typed, and is then inserted.
        g.start_edit(&ctx, &result.rows, r, 0);
        g.editing.as_mut().unwrap().2 = "42".into();
        g.commit_edit(&result.rows);
        let c = g.edits.to_row_changes(&result.rows);
        assert_eq!(c.inserts, vec![vec![Some("42".into()), None, None]]);
    }

    #[test]
    fn header_clicks_cycle_the_local_sort() {
        assert_eq!(next_sort(None, 2), Some((2, true)));
        assert_eq!(next_sort(Some((2, true)), 2), Some((2, false)));
        assert_eq!(next_sort(Some((2, false)), 2), None);
        assert_eq!(next_sort(Some((2, false)), 1), Some((1, true)));
    }

    #[test]
    fn view_follows_filter_sort_and_a_replaced_result() {
        let r = rows();
        let mut g = GridState {
            sort: Some((1, false)),
            ..Default::default()
        };
        g.ensure_view(&r);
        assert_eq!(g.view, vec![2, 1, 0]);
        g.filter = "o".into();
        g.ensure_view(&r);
        assert_eq!(g.view, vec![2, 1, 0], "not recomputed until dirty");
        g.view_dirty = true;
        g.ensure_view(&r);
        assert_eq!(g.view, vec![2, 1]);
        let other = col(&["o1", "o2", "o3", "o4"]);
        g.ensure_view(&other);
        assert_eq!(g.view.len(), 4, "another result recomputes the view");
    }

    #[test]
    fn keyboard_moves_wrap_on_tab_and_reach_new_rows() {
        let r = rows();
        let mut g = GridState::default();
        g.ensure_view(&r);
        g.move_focus(2, 1, 0, false);
        assert_eq!(g.selected, Some((RowRef::Base(0), 0)), "first move focuses");
        g.move_focus(2, 0, 1, true);
        g.move_focus(2, 0, 1, true);
        assert_eq!(g.selected, Some((RowRef::Base(1), 0)), "Tab wraps");
        g.move_focus(2, 0, -1, true);
        assert_eq!(
            g.selected,
            Some((RowRef::Base(0), 1)),
            "Shift+Tab wraps back"
        );
        g.edits.add_row(2);
        g.move_focus(2, 99, 0, false);
        assert_eq!(
            g.selected,
            Some((RowRef::New(0), 1)),
            "clamped to the new row"
        );
        g.revert();
        g.clamp(&r, 2);
        assert_eq!(g.selected, None, "the new row is gone");
    }
}
