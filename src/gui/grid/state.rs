//! Focus, cell editing and keyboard handling of a grid.

use super::changes::RowRef;
use crate::engine::models::{display_cell, is_null, QueryResult};
use egui::{Key, Modifiers};

use super::view::{display_pos, row_ref};
use super::{first_input_column, GridAction, GridState};

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

    fn row_exists(&self, rows: &[Vec<String>], r: RowRef) -> bool {
        match r {
            RowRef::Base(i) => i < rows.len(),
            RowRef::New(k) => k < self.edits.new_rows(),
        }
    }

    /// Forget a focus or an edit on a row that no longer exists.
    pub(super) fn clamp(&mut self, rows: &[Vec<String>], ncols: usize) {
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

    pub(super) fn total_rows(&self) -> usize {
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

    pub(super) fn start_edit(
        &mut self,
        ctx: &egui::Context,
        rows: &[Vec<String>],
        r: RowRef,
        c: usize,
    ) {
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

    pub(super) fn commit_edit(&mut self, rows: &[Vec<String>]) {
        if let Some((r, c, text)) = self.editing.take() {
            if !(self.editing_blank && text.is_empty()) {
                self.edits.set(rows, r, c, text);
            }
        }
    }

    /// Alt+Insert: a new row, editing its first cell that the database
    /// does not seem to fill itself (`detect_system_columns`: only a hint,
    /// any cell can be typed).
    pub(super) fn add_row(&mut self, ctx: &egui::Context, result: &QueryResult) {
        self.commit_edit(&result.rows);
        let r = self.edits.add_row(result.columns.len());
        self.start_edit(ctx, &result.rows, r, first_input_column(&result.columns));
    }

    /// Mark or unmark row `r` deleted; a new row is removed, and the later
    /// new rows shift up: the focus and an open cell editor follow their row
    /// (they are dropped when on the removed row).
    pub(super) fn toggle_delete(&mut self, r: RowRef) {
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

    pub(super) fn revert(&mut self) {
        self.editing = None;
        self.edits.clear();
        self.generation += 1;
    }

    /// Keys while a cell is being edited. The cell editor loses its focus on
    /// Escape before this runs, hence Escape is checked before the focus.
    pub(super) fn editing_keys(
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
    pub(super) fn grid_keys(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{Column, NULL_CELL};

    fn col(values: &[&str]) -> Vec<Vec<String>> {
        values.iter().map(|v| vec![v.to_string()]).collect()
    }

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec!["1".into(), "Alice".into()],
            vec!["2".into(), "bob".into()],
            vec!["3".into(), "Carol".into()],
        ]
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
