//! Data grid widget: drawing and cell actions. Its rows (filter, sort) are in
//! `view`, focus / editing / keys in `state`, pending edits in `changes`.
pub mod changes;
mod state;
mod view;

use egui::text::{CCursor, CCursorRange};
use egui::{Color32, RichText, Sense, Stroke, StrokeKind};
use egui_extras::{Column as TableColumn, TableBuilder};
use egui_phosphor::regular as icon;

use crate::engine::models::{display_cell, is_null, Column, QueryResult, NULL_CELL};
use crate::engine::ops::rows::detect_system_columns;
use crate::gui::theme::{ACCENT, ERROR, SUCCESS};

use changes::{PendingEdits, RowRef};
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

    #[test]
    fn row_as_tsv() {
        assert_eq!(
            to_tsv(&["a".into(), "b\tc".into(), "d\ne".into()]),
            "a\tb c\td e"
        );
    }

    #[test]
    fn null_cells_are_shown_as_null_apart_from_the_text_null() {
        assert_eq!(cell_display(NULL_CELL), ("NULL", true));
        assert_eq!(cell_display("NULL"), ("NULL", false));
        assert_eq!(cell_display(""), ("", false));
    }
}
