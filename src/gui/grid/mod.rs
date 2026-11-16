//! Data grid widget: drawing and cell actions. Its rows (filter, sort) are in
//! `view`, focus / editing / keys in `state`, pending edits in `changes`.
pub mod changes;
mod paste;
mod selection;
mod state;
mod view;

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, RichText, Sense, Stroke, StrokeKind};
use egui_extras::{Column as TableColumn, TableBuilder};
use egui_phosphor::regular as icon;

use crate::engine::models::{is_null, Column, QueryResult, NULL_CELL};
use crate::engine::nosql::is_absent;
use crate::engine::ops::rows::detect_system_columns;
use crate::gui::fk::{self, CellLink, ColumnLink};
use crate::gui::shortcuts::{hint_here, is_mac, label_here};
use crate::gui::theme::{ACCENT, ERROR, SUCCESS};

use changes::{PendingEdits, RowRef};
use selection::{DragMode, Selection};
pub use state::FOLLOW_LINK;
use state::{clear_cells, delete_row, edit_cell, ADD_ROW, REVERT, SUBMIT};
use view::{display_pos, next_sort, row_ref};

/// A row as tab-separated values (tabs and newlines inside cells become spaces).
pub fn to_tsv(row: &[String]) -> String {
    row.iter()
        .map(|c| c.replace(['\t', '\n'], " "))
        .collect::<Vec<_>>()
        .join("\t")
}

/// State of one grid, kept by its owner (console result tab, data tab)
/// across frames.
pub struct GridState {
    /// Focused cell.
    pub selected: Option<(RowRef, usize)>,
    /// Cells selected with it (Shift+click / arrows, row numbers, headers,
    /// Ctrl+A), copied as CSV.
    pub selection: Option<Selection>,
    /// A mouse drag selecting cells or rows, until the button is released.
    drag: Option<DragMode>,
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
            selection: None,
            drag: None,
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
    /// Follow the foreign key of a cell: preview the linked row, or open
    /// it in a filtered tab (`new_tab`: Ctrl+click, Cmd on macOS).
    Follow {
        row: RowRef,
        col: usize,
        new_tab: bool,
    },
    /// "Enregistrements liés": the rows of incoming key `fk` pointing at
    /// `row`.
    Referencing {
        row: RowRef,
        fk: usize,
    },
}

pub struct GridOptions<'a> {
    pub result: &'a QueryResult,
    pub editable: bool,
    /// true → header clicks emit SortBy (server side); false → sort locally.
    pub server_sort: bool,
    /// Sort arrow of a server-sorted grid.
    pub sort_indicator: Option<(usize, bool)>,
    /// Foreign key of each column (empty: none known).
    pub links: &'a [Option<ColumnLink>],
    /// Labels of the keys of other tables pointing at this one, for the
    /// "Enregistrements liés" menu of a row.
    pub incoming: &'a [String],
}

const AMBER: Color32 = Color32::from_rgb(0xF2, 0xA6, 0x4C);
/// Longest text laid out in a cell (the value panel shows it all).
const MAX_CELL_CHARS: usize = 256;

fn selected_bg() -> Color32 {
    ACCENT.gamma_multiply(0.22)
}

/// Shortcuts named in hints (handled by `state::grid_keys` / egui's Copy).
const SELECT_ALL: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::A);
const COPY: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::C);

/// How far `v` is past `range` (negative before it, 0 inside).
fn overshoot(v: f32, range: egui::Rangef) -> f32 {
    if v > range.max {
        v - range.max
    } else if v < range.min {
        v - range.min
    } else {
        0.0
    }
}

/// Scroll of one frame (`dt` s) while a drag is `over` points past an
/// edge: faster the further out.
fn edge_scroll(over: f32, dt: f32) -> f32 {
    over.signum() * over.abs().min(150.0) * 12.0 * dt
}

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

/// Text shown for a cell and whether it is NULL or a missing field of a
/// document (drawn weak and italic): the text "NULL" is an ordinary value.
pub fn cell_display(value: &str) -> (&str, bool) {
    (display_cell(value), is_null(value) || is_absent(value))
}

/// Text shown for a cell: NULL as "NULL", a document's missing field
/// (`ABSENT_CELL`) as "—".
pub fn display_cell(value: &str) -> &str {
    if is_absent(value) {
        ABSENT_TEXT
    } else {
        crate::engine::models::display_cell(value)
    }
}

/// How a document's missing field is shown.
pub const ABSENT_TEXT: &str = "—";

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
    /// A drag started on a cell (with Shift: from the current anchor).
    DragCells(RowRef, usize, bool),
    /// A drag started on a row number (with Shift: extend).
    DragRows(RowRef, bool),
    /// Shift+click: select up to this cell.
    Extend(RowRef, usize),
    /// Row number clicked (with Shift: extend).
    SelectRows(RowRef, bool),
    Edit(RowRef, usize),
    CopySelection,
    /// Suppr on a selection: its cells set to NULL.
    NullSelection,
    CopyValue(RowRef, usize),
    CopyRow(RowRef),
    SetNull(RowRef, usize),
    /// Set the empty string.
    SetEmpty(RowRef, usize),
    /// A new row's cell back to its default.
    SetDefault(RowRef, usize),
    AddRow,
    ToggleDelete(RowRef),
    Follow(RowRef, usize, bool),
    Referencing(RowRef, usize),
}

/// The marked header of a column: key icon and FK badge before the name,
/// then the sort arrow.
fn header_job(
    ui: &egui::Ui,
    column: &Column,
    link: Option<&ColumnLink>,
    sort: Option<bool>,
) -> (egui::text::LayoutJob, String) {
    let (marks, hover) = fk::header_marks(column, link);
    let style = ui.style();
    let color = fk::link_color(ui.visuals().dark_mode);
    let mut job = egui::text::LayoutJob::default();
    let mut add = |text: RichText| {
        text.append_to(
            &mut job,
            style,
            egui::FontSelection::Default,
            egui::Align::Center,
        )
    };
    if marks.pk {
        add(RichText::new(format!("{} ", icon::KEY)).color(color));
    }
    if marks.fk.is_some() {
        add(RichText::new(format!("{} FK ", icon::LINK))
            .small()
            .strong()
            .color(color));
    }
    add(RichText::new(&column.name).strong());
    if let Some(asc) = sort {
        add(RichText::new(format!(
            " {}",
            if asc {
                icon::CARET_UP
            } else {
                icon::CARET_DOWN
            }
        )));
    }
    (job, hover)
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
    // Alt+Entrée on a link cell previews the linked row (checked before
    // the grid's keys: Entrée alone would take it to edit the cell).
    let follow_key = match (grid_focused, &state.editing, state.selected) {
        (true, None, Some((r, c))) => {
            let link = opts.links.get(c).and_then(Option::as_ref);
            let blank = state.edits.is_default(r, c) || is_null(state.edits.value(rows, r, c));
            let saved = matches!(r, RowRef::Base(_)) && !state.edits.is_deleted(r);
            let edited = state.edits.is_edited(r, c);
            (fk::cell_link(link, blank, saved, edited) == CellLink::Active
                && ctx.input_mut(|i| i.consume_shortcut(&FOLLOW_LINK)))
            .then_some(GridAction::Follow {
                row: r,
                col: c,
                new_tab: false,
            })
        }
        _ => None,
    };
    let mut action = if let Some(follow) = follow_key {
        follow
    } else if state.editing.is_some() {
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
    let sel_rect = state.selection_rect(ncols);
    let shift = ctx.input(|i| i.modifiers.shift);
    let mut column_clicked = None;
    let mut select_all_clicked = false;
    // Mouse drag: the cell / row under the pointer (held inside the
    // visible body, so that the selection follows an edge scroll).
    let drag = state.drag;
    let pointer = ctx.input(|i| i.pointer.latest_pos());
    let dt = ctx.input(|i| i.stable_dt).min(0.1);
    let mut drag_to = None;
    let mut drag_rows_to = None;
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
        // Screen x range of the focused column, read from its header cell
        // (always drawn, unlike rows outside the viewport).
        let mut focused_x = None;
        egui::ScrollArea::horizontal()
            .id_salt(id.with("h"))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut table = TableBuilder::new(ui)
                    .id_salt(id.with("table"))
                    .striped(true)
                    .resizable(true)
                    .sense(Sense::click_and_drag())
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
                            let r = ui
                                .add(
                                    egui::Label::new(RichText::new("#").weak())
                                        .selectable(false)
                                        .sense(Sense::click()),
                                )
                                .on_hover_text(format!(
                                    "Tout sélectionner ({})",
                                    label_here(&SELECT_ALL)
                                ));
                            select_all_clicked = r.clicked();
                        });
                        for (c, column) in result.columns.iter().enumerate() {
                            header.col(|ui| {
                                if scroll_pos.is_some() && selected.is_some_and(|(_, sc)| sc == c) {
                                    focused_x = Some(ui.max_rect().x_range());
                                }
                                let sort = sort_shown.filter(|(sc, _)| *sc == c).map(|(_, a)| a);
                                let link = opts.links.get(c).and_then(Option::as_ref);
                                let (job, hover) = header_job(ui, column, link, sort);
                                let r = ui
                                    .add(
                                        egui::Label::new(job)
                                            .selectable(false)
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_text(format!(
                                        "{hover} · cliquer pour trier · Maj+clic : sélectionner la colonne"
                                    ));
                                if r.clicked() && shift {
                                    column_clicked = Some(c);
                                } else if r.clicked() {
                                    header_clicked = Some(c);
                                }
                            });
                        }
                    })
                    .body(|mut body| {
                        let clip = body.ui_mut().clip_rect();
                        if let (Some(_), Some(p)) = (drag, pointer) {
                            let dy = overshoot(p.y, clip.y_range());
                            if dy != 0.0 {
                                body.ui_mut().scroll_with_delta_animation(
                                    egui::vec2(0.0, -edge_scroll(dy, dt)),
                                    egui::style::ScrollAnimation::none(),
                                );
                                ctx.request_repaint();
                            }
                        }
                        let inset = egui::vec2(1.0, 1.0);
                        let target = pointer
                            .filter(|_| drag.is_some())
                            .map(|p| p.clamp(clip.min + inset, clip.max - inset));
                        body.rows(row_h, total, |mut row| {
                            let d = row.index();
                            let r = row_ref(view, d);
                            let is_new = matches!(r, RowRef::New(_));
                            let deleted = edits.is_deleted(r);
                            let row_selected = selected.is_some_and(|(sr, _)| sr == r);
                            row.set_selected(row_selected);
                            let whole_row = sel_rect
                                .as_ref()
                                .is_some_and(|s| s.rows.contains(&d) && s.cols.len() == ncols);
                            let (_, num) = row.col(|ui| {
                                let rect = ui.max_rect();
                                if let (Some(DragMode::Rows), Some(t)) = (drag, target) {
                                    if rect.y_range().contains(t.y) {
                                        drag_rows_to = Some(r);
                                    }
                                }
                                if whole_row {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, selected_bg());
                                }
                                if is_new {
                                    ui.add(egui::Label::new(RichText::new(icon::PLUS).color(SUCCESS)).selectable(false));
                                } else {
                                    ui.add(egui::Label::new(RichText::new((d + 1).to_string()).weak()).selectable(false));
                                }
                            });
                            if num.drag_started_by(egui::PointerButton::Primary) {
                                cell_action = Some(CellAction::DragRows(r, shift));
                            } else if num.clicked() {
                                cell_action = Some(CellAction::SelectRows(r, shift));
                            }
                            for c in 0..ncols {
                                let link = opts.links.get(c).and_then(Option::as_ref);
                                let edited_here = editing
                                    .as_ref()
                                    .is_some_and(|(er, ec, _)| (*er, *ec) == (r, c));
                                let blank =
                                    edits.is_default(r, c) || is_null(edits.value(rows, r, c));
                                let saved = matches!(r, RowRef::Base(_)) && !deleted;
                                let active = !edited_here
                                    && fk::cell_link(link, blank, saved, edits.is_edited(r, c))
                                        == CellLink::Active;
                                let mut icon_clicked = false;
                                let in_sel = sel_rect.as_ref().is_some_and(|s| s.contains(d, c));
                                let (_, resp) = row.col(|ui| {
                                    let rect = ui.max_rect();
                                    if let (Some(DragMode::Cells), Some(t)) = (drag, target) {
                                        if rect.contains(t) {
                                            drag_to = Some((r, c));
                                        }
                                    }
                                    if in_sel {
                                        ui.painter().rect_filled(rect, 0.0, selected_bg());
                                    } else if is_new {
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
                                            if let (true, Some(link)) = (active, link) {
                                                icon_clicked =
                                                    link_cell(ui, text, link, row_selected);
                                            } else {
                                                ui.add(
                                                    egui::Label::new(text)
                                                        .selectable(false)
                                                        .truncate(),
                                                );
                                            }
                                        }
                                    }
                                    if *selected == Some((r, c)) {
                                        ui.painter().rect_stroke(
                                            rect.shrink(1.0),
                                            2.0,
                                            Stroke::new(1.5, ACCENT),
                                            StrokeKind::Inside,
                                        );
                                    }
                                });
                                let command = ctx.input(|i| i.modifiers.command);
                                if icon_clicked {
                                    cell_action = Some(CellAction::Follow(r, c, command));
                                } else if active && command && resp.clicked() {
                                    cell_action = Some(CellAction::Follow(r, c, true));
                                } else if resp.drag_started_by(egui::PointerButton::Primary)
                                    && !edited_here
                                {
                                    cell_action = Some(CellAction::DragCells(r, c, shift));
                                } else if resp.double_clicked() {
                                    cell_action = Some(CellAction::Edit(r, c));
                                } else if resp.clicked() && shift {
                                    cell_action = Some(CellAction::Extend(r, c));
                                } else if resp.secondary_clicked() && in_sel {
                                    // Keep the selection for its menu.
                                } else if resp.clicked() || resp.secondary_clicked() {
                                    cell_action = Some(CellAction::Select(r, c));
                                }
                                let menu = MenuCell {
                                    row: r,
                                    col: c,
                                    deleted,
                                    in_selection: in_sel,
                                    editable: opts.editable,
                                    link: link.filter(|_| active),
                                    incoming: if saved { opts.incoming } else { &[] },
                                };
                                resp.context_menu(|ui| context_menu(ui, &menu, &mut cell_action));
                            }
                        });
                    });
                // The table's own scroll area takes every scroll target
                // (and asks for x = 0 when scrolling to a row): bring the
                // focused column into view here, for the horizontal area.
                // Past the left / right edge while dragging: scroll this
                // horizontal area (the table's own one is vertical only).
                if let (Some(DragMode::Cells), Some(p)) = (drag, pointer) {
                    let dx = overshoot(p.x, ui.clip_rect().x_range());
                    if dx != 0.0 {
                        ui.scroll_with_delta_animation(
                            egui::vec2(-edge_scroll(dx, dt), 0.0),
                            egui::style::ScrollAnimation::none(),
                        );
                        ctx.request_repaint();
                    }
                }
                if let Some(x) = focused_x {
                    let y = ui.clip_rect().y_range();
                    // Not animated, like the rows (`animate_scrolling(false)`).
                    ui.scroll_to_rect_animation(
                        egui::Rect::from_x_y_ranges(x, y),
                        None,
                        egui::style::ScrollAnimation::none(),
                    );
                }
            });
    });
    if scroll_pos.is_some() {
        // The new offsets only show on the next frame.
        ctx.request_repaint();
    }

    match footer(ui, state, result, opts.editable, &mut cell_action) {
        GridAction::Submit => {
            state.commit_edit(rows);
            action = GridAction::Submit;
        }
        GridAction::None => {}
        a => action = a,
    }

    if select_all_clicked || column_clicked.is_some() {
        state.commit_edit(rows);
        match column_clicked {
            Some(c) => state.select_column(c),
            None => state.select_all(),
        }
        ctx.memory_mut(|m| m.request_focus(focus_id));
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
            state.selection = None;
        }
        Some(CellAction::DragCells(r, c, extend)) => {
            state.commit_edit(rows);
            if extend {
                state.extend_to(r, c);
            } else {
                state.selected = Some((r, c));
                state.selection = None;
            }
            state.drag = Some(DragMode::Cells);
            focus_grid();
        }
        Some(CellAction::DragRows(r, extend)) => {
            state.commit_edit(rows);
            state.select_rows(r, extend);
            state.drag = Some(DragMode::Rows);
            focus_grid();
        }
        Some(CellAction::Extend(r, c)) => {
            state.commit_edit(rows);
            state.extend_to(r, c);
            focus_grid();
        }
        Some(CellAction::SelectRows(r, extend)) => {
            state.commit_edit(rows);
            state.select_rows(r, extend);
            focus_grid();
        }
        Some(CellAction::CopySelection) => state.copy(&ctx, result),
        Some(CellAction::NullSelection) => {
            state.commit_edit(rows);
            state.null_selected(result);
            focus_grid();
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
            state.selection = None;
            focus_grid();
        }
        Some(CellAction::SetDefault(r, c)) => {
            state.commit_edit(rows);
            state.edits.reset_default(r, c);
            state.selected = Some((r, c));
            state.selection = None;
            focus_grid();
        }
        Some(CellAction::SetNull(r, c)) => {
            state.commit_edit(rows);
            state.edits.set(rows, r, c, NULL_CELL.into());
            state.selected = Some((r, c));
            state.selection = None;
            focus_grid();
        }
        Some(CellAction::AddRow) => state.add_row(&ctx, result),
        Some(CellAction::ToggleDelete(r)) => {
            state.commit_edit(rows);
            state.toggle_delete(r);
            focus_grid();
        }
        Some(CellAction::Follow(r, c, new_tab)) => {
            state.commit_edit(rows);
            state.selected = Some((r, c));
            state.selection = None;
            focus_grid();
            action = GridAction::Follow {
                row: r,
                col: c,
                new_tab,
            };
        }
        Some(CellAction::Referencing(r, fk)) => {
            state.commit_edit(rows);
            action = GridAction::Referencing { row: r, fk };
        }
        None => {}
    }
    if state.drag.is_some() {
        if let Some((r, c)) = drag_to.filter(|&to| state.selected != Some(to)) {
            state.extend_to(r, c);
        }
        if let Some(r) = drag_rows_to.filter(|&to| state.selected.map(|s| s.0) != Some(to)) {
            state.select_rows(r, true);
        }
        if !ctx.input(|i| i.pointer.primary_down()) {
            state.drag = None;
        }
    }
    state.clamp(rows, ncols);
    if action == GridAction::None && state.selected != before {
        action = GridAction::SelectionChanged;
    }
    action
}

/// A cell's value drawn as a link: link icon and value in the link
/// colour (the selection's text colour on a selected row), underlined on
/// hover. Whether the icon was clicked.
fn link_cell(ui: &mut egui::Ui, text: RichText, link: &ColumnLink, selected: bool) -> bool {
    let color = if selected {
        ui.visuals().selection.stroke.color
    } else {
        fk::link_color(ui.visuals().dark_mode)
    };
    // Selected: underlined, so it still reads as a link.
    let hovered = selected || ui.rect_contains_pointer(ui.max_rect());
    let hint = format!(
        "{} · clic sur l'icône : aperçu ({}) · {} : ouvrir dans un onglet",
        link.tooltip(),
        label_here(&FOLLOW_LINK),
        fk::tab_click_here()
    );
    let clicked = ui
        .add(
            egui::Label::new(RichText::new(icon::LINK).color(color))
                .selectable(false)
                .sense(Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(&hint)
        .clicked();
    let text = if hovered { text.underline() } else { text };
    ui.add(
        egui::Label::new(text.color(color))
            .selectable(false)
            .truncate(),
    )
    .on_hover_text(hint);
    clicked
}

/// The cell a context menu is opened on.
struct MenuCell<'a> {
    row: RowRef,
    col: usize,
    deleted: bool,
    /// Inside a selection wider than one cell.
    in_selection: bool,
    editable: bool,
    /// Its foreign key, when it links somewhere.
    link: Option<&'a ColumnLink>,
    /// Incoming keys of the row's table (none for unsaved rows).
    incoming: &'a [String],
}

fn context_menu(ui: &mut egui::Ui, m: &MenuCell, action: &mut Option<CellAction>) {
    let (r, c, deleted) = (m.row, m.col, m.deleted);
    let mut item = |ui: &mut egui::Ui, label: String, a: CellAction| {
        if ui.button(label).clicked() {
            *action = Some(a);
            ui.close_menu();
        }
    };
    if let Some(link) = m.link {
        item(
            ui,
            format!(
                "{} Voir la ligne liée ({})",
                icon::LINK,
                label_here(&FOLLOW_LINK)
            ),
            CellAction::Follow(r, c, false),
        );
        item(
            ui,
            format!(
                "{} Ouvrir {} dans un onglet ({})",
                icon::ARROW_SQUARE_OUT,
                link.table,
                fk::tab_click_here()
            ),
            CellAction::Follow(r, c, true),
        );
    }
    if !m.incoming.is_empty() {
        ui.menu_button(
            format!("{} Enregistrements liés", icon::TREE_STRUCTURE),
            |ui| {
                for (i, label) in m.incoming.iter().enumerate() {
                    if ui.button(label).clicked() {
                        *action = Some(CellAction::Referencing(r, i));
                        ui.close_menu();
                    }
                }
            },
        );
    }
    if m.link.is_some() || !m.incoming.is_empty() {
        ui.separator();
    }
    let mut item = |ui: &mut egui::Ui, label: String, a: CellAction| {
        if ui.button(label).clicked() {
            *action = Some(a);
            ui.close_menu();
        }
    };
    if m.in_selection {
        item(
            ui,
            format!(
                "{} Copier la sélection en CSV ({})",
                icon::TABLE,
                label_here(&COPY)
            ),
            CellAction::CopySelection,
        );
    }
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
    if !m.editable {
        return;
    }
    ui.separator();
    if !deleted {
        item(
            ui,
            format!(
                "{} Modifier ({})",
                icon::PENCIL_SIMPLE,
                hint_here(&edit_cell(is_mac()))
            ),
            CellAction::Edit(r, c),
        );
        if m.in_selection {
            item(
                ui,
                format!(
                    "{} Mettre la sélection à NULL ({})",
                    icon::PROHIBIT,
                    hint_here(clear_cells(is_mac()))
                ),
                CellAction::NullSelection,
            );
        } else {
            item(
                ui,
                format!(
                    "{} Mettre à NULL ({})",
                    icon::PROHIBIT,
                    hint_here(clear_cells(is_mac()))
                ),
                CellAction::SetNull(r, c),
            );
        }
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
        format!("{} Ajouter une ligne ({})", icon::PLUS, hint_here(&ADD_ROW)),
        CellAction::AddRow,
    );
    let label = if deleted {
        format!("{} Annuler la suppression", icon::ARROW_COUNTER_CLOCKWISE)
    } else {
        format!(
            "{} Supprimer la ligne ({})",
            icon::TRASH,
            hint_here(delete_row(is_mac()))
        )
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
                .on_hover_text(format!("Ajouter une ligne ({})", hint_here(&ADD_ROW)))
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
                .on_hover_text(format!(
                    "Supprimer la ligne sélectionnée ({})",
                    hint_here(delete_row(is_mac()))
                ))
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
        if let Some(s) = state.selection_rect(result.columns.len()) {
            text.push_str(&format!(
                " · sélection : {} × {} col.",
                plural_rows(s.rows.len()),
                s.cols.len()
            ));
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
            .on_hover_text(format!(
                "Appliquer dans une transaction ({})",
                label_here(&SUBMIT)
            ))
            .clicked()
        {
            action = GridAction::Submit;
        }
        if ui
            .button(format!("{} Revert", icon::ARROW_COUNTER_CLOCKWISE))
            .on_hover_text(format!(
                "Abandonner les modifications ({})",
                label_here(&REVERT)
            ))
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

    /// Arrow keys move the focus out of the viewport, right and down: the
    /// grid scrolls both ways so the focused cell stays on screen.
    #[test]
    fn keyboard_focus_stays_in_view() {
        use crate::engine::models::Column;
        use crate::gui::ui_test::{key, Driver};
        use egui::Key;
        let columns = (0..30)
            .map(|c| Column {
                name: format!("col{c}"),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: false,
            })
            .collect();
        let rows = (0..200)
            .map(|r| (0..30).map(|c| format!("r{r}c{c}")).collect())
            .collect();
        let result = QueryResult {
            columns,
            rows,
            ..QueryResult::default()
        };
        let id = egui::Id::new("grid");
        let mut state = GridState::default();
        let mut d = Driver::new();
        // The focused cell after the frame.
        let mut frame = |d: &mut Driver, events| {
            d.central(events, |ui| {
                show(
                    ui,
                    id,
                    &mut state,
                    GridOptions {
                        result: &result,
                        editable: false,
                        server_sort: false,
                        sort_indicator: None,
                        links: &[],
                        incoming: &[],
                    },
                );
                state.selected
            })
        };
        frame(&mut d, vec![]);
        d.ctx.memory_mut(|m| m.request_focus(id.with("focus")));
        frame(&mut d, vec![]);
        let on_screen = |d: &Driver, label: &str| {
            let p = d.text(label).unwrap_or_else(|| panic!("{label} not drawn"));
            assert!(
                (0.0..1280.0).contains(&p.x) && (0.0..800.0).contains(&p.y),
                "{label} off screen at {p:?}"
            );
        };
        // Press `k` `n` times, each followed by the repaint the grid asked
        // for; the focused cell then.
        let mut press = |d: &mut Driver, k, n| {
            for _ in 0..n {
                frame(d, vec![key(k)]);
            }
            frame(d, vec![])
        };
        // The first arrow focuses the first cell.
        assert_eq!(press(&mut d, Key::ArrowDown, 1), Some((RowRef::Base(0), 0)));
        assert_eq!(
            press(&mut d, Key::ArrowRight, 25),
            Some((RowRef::Base(0), 25))
        );
        on_screen(&d, "r0c25");
        assert_eq!(
            press(&mut d, Key::ArrowDown, 150),
            Some((RowRef::Base(150), 25))
        );
        on_screen(&d, "r150c25");
        assert_eq!(
            press(&mut d, Key::ArrowLeft, 25),
            Some((RowRef::Base(150), 0))
        );
        on_screen(&d, "r150c0");
        assert_eq!(press(&mut d, Key::ArrowUp, 150), Some((RowRef::Base(0), 0)));
        on_screen(&d, "r0c0");
    }

    /// Shift+arrows select a block, Ctrl+A everything: the copy is CSV;
    /// a single focused cell still copies its bare value.
    #[test]
    fn keyboard_selection_is_copied_as_csv() {
        use crate::engine::models::Column;
        use crate::gui::ui_test::{key, Driver};
        use egui::{Event, Key, Modifiers};
        let columns = (0..3)
            .map(|c| Column {
                name: format!("col{c}"),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: false,
            })
            .collect();
        let rows = (0..3)
            .map(|r| (0..3).map(|c| format!("r{r}c{c}")).collect())
            .collect();
        let result = QueryResult {
            columns,
            rows,
            ..QueryResult::default()
        };
        let id = egui::Id::new("grid");
        let mut state = GridState::default();
        let mut d = Driver::new();
        let mut frame = |d: &mut Driver, events| {
            d.central(events, |ui| {
                show(
                    ui,
                    id,
                    &mut state,
                    GridOptions {
                        result: &result,
                        editable: true,
                        server_sort: false,
                        sort_indicator: None,
                        links: &[],
                        incoming: &[],
                    },
                );
            });
            d.copied()
        };
        let with = |m: Modifiers, k: Key| Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        };
        frame(&mut d, vec![]);
        d.ctx.memory_mut(|m| m.request_focus(id.with("focus")));
        frame(&mut d, vec![]);
        frame(&mut d, vec![key(Key::ArrowDown)]);
        assert_eq!(frame(&mut d, vec![Event::Copy]), vec!["r0c0".to_string()]);
        frame(&mut d, vec![with(Modifiers::SHIFT, Key::ArrowRight)]);
        frame(&mut d, vec![with(Modifiers::SHIFT, Key::ArrowDown)]);
        assert_eq!(
            frame(&mut d, vec![Event::Copy]),
            vec!["col0,col1\nr0c0,r0c1\nr1c0,r1c1".to_string()]
        );
        frame(&mut d, vec![with(Modifiers::COMMAND, Key::A)]);
        let all = frame(&mut d, vec![Event::Copy]);
        assert_eq!(all[0].lines().count(), 4, "{all:?}");
        // A plain arrow drops the selection.
        frame(&mut d, vec![key(Key::ArrowRight)]);
        assert_eq!(frame(&mut d, vec![Event::Copy]), vec!["r1c2".to_string()]);
        // Ctrl+V reaches the grid: pasted from the focused cell.
        frame(&mut d, vec![Event::Paste("x\ny\n".into())]);
        let value = |r| {
            state
                .edits
                .value(&result.rows, RowRef::Base(r), 2)
                .to_string()
        };
        assert_eq!((value(1), value(2)), ("x".into(), "y".into()));
    }

    /// Pressing on a cell and dragging selects the block up to the cell
    /// under the pointer; dragging over row numbers selects whole rows.
    #[test]
    fn mouse_drag_selects_cells_and_rows() {
        use crate::engine::models::Column;
        use crate::gui::ui_test::Driver;
        use egui::{Event, Modifiers, PointerButton, Pos2};
        let columns = (0..3)
            .map(|c| Column {
                name: format!("col{c}"),
                type_name: "TEXT".into(),
                nullable: true,
                is_primary_key: false,
            })
            .collect();
        let rows = (0..4)
            .map(|r| (0..3).map(|c| format!("r{r}c{c}")).collect())
            .collect();
        let result = QueryResult {
            columns,
            rows,
            ..QueryResult::default()
        };
        let id = egui::Id::new("grid");
        let mut state = GridState::default();
        let mut d = Driver::new();
        let mut frame = |d: &mut Driver, events| {
            d.central(events, |ui| {
                show(
                    ui,
                    id,
                    &mut state,
                    GridOptions {
                        result: &result,
                        editable: false,
                        server_sort: false,
                        sort_indicator: None,
                        links: &[],
                        incoming: &[],
                    },
                );
                (state.selected, state.selection_rect(3))
            })
        };
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        // Frames of a press at `from`, moves to `to`, release, then none.
        let drag = |from: Pos2, to: Pos2| {
            let mut frames = vec![vec![Event::PointerMoved(from), button(from, true)]];
            for k in 1..=4 {
                let p = from + (to - from) * (k as f32 / 4.0);
                frames.push(vec![Event::PointerMoved(p)]);
            }
            frames.push(vec![button(to, false)]);
            frames.push(vec![]);
            frames
        };
        frame(&mut d, vec![]);
        let at = |d: &Driver, t: &str| d.text(t).unwrap_or_else(|| panic!("{t}"));
        let (from, to) = (at(&d, "r0c0"), at(&d, "r2c1"));
        let (focus, rect) = drag(from, to)
            .into_iter()
            .fold(None, |_, e| Some(frame(&mut d, e)))
            .unwrap();
        assert_eq!(focus, Some((RowRef::Base(2), 1)));
        let rect = rect.expect("a block");
        assert_eq!((rect.rows, rect.cols), (0..3, 0..2));
        // Row numbers: whole rows.
        let (from, to) = (at(&d, "2"), at(&d, "4"));
        let (_, rect) = drag(from, to)
            .into_iter()
            .fold(None, |_, e| Some(frame(&mut d, e)))
            .unwrap();
        let rect = rect.expect("rows");
        assert_eq!((rect.rows, rect.cols), (1..4, 0..3));
        // A plain click still selects one cell.
        let p = at(&d, "r3c2");
        frame(&mut d, crate::gui::ui_test::click(p));
        assert_eq!(frame(&mut d, vec![]), (Some((RowRef::Base(3), 2)), None));
    }

    #[test]
    fn row_as_tsv() {
        assert_eq!(
            to_tsv(&["a".into(), "b\tc".into(), "d\ne".into()]),
            "a\tb c\td e"
        );
    }

    #[test]
    fn missing_fields_are_shown_greyed_apart_from_null() {
        use crate::engine::nosql::ABSENT_CELL;
        assert_eq!(cell_display(ABSENT_CELL), (ABSENT_TEXT, true));
        assert_eq!(display_cell(ABSENT_CELL), "—");
        assert_eq!(display_cell(NULL_CELL), "NULL");
        assert_eq!(cell_display("—"), ("—", false));
    }

    #[test]
    fn null_cells_are_shown_as_null_apart_from_the_text_null() {
        assert_eq!(cell_display(NULL_CELL), ("NULL", true));
        assert_eq!(cell_display("NULL"), ("NULL", false));
        assert_eq!(cell_display(""), ("", false));
    }
}
