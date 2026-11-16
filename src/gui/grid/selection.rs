//! Multi-cell selection of a grid (a rectangle between an anchor and the
//! focused cell, or whole rows / columns) and its copy as CSV.

use std::ops::Range;

use super::changes::RowRef;
use super::view::{display_pos, row_ref};
use super::GridState;
use crate::engine::models::{QueryResult, NULL_CELL};
use crate::engine::nosql::is_absent;
use crate::engine::services::export_import::export_to_csv_quoting_empty;

/// A selection wider than the focused cell: from `anchor` to the focused
/// cell, widened to every column (`all_cols`) or every row (`all_rows`).
/// What a mouse drag started on a grid selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    /// From a cell: the block of cells up to the pointer.
    Cells,
    /// From a row number: the whole rows up to the pointer.
    Rows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (RowRef, usize),
    /// Whole rows (row numbers clicked).
    pub all_cols: bool,
    /// Whole columns (Shift+click on a header).
    pub all_rows: bool,
}

impl Selection {
    pub(super) fn cells(anchor: (RowRef, usize)) -> Self {
        Self {
            anchor,
            all_cols: false,
            all_rows: false,
        }
    }
}

/// Selected display rows and columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelRect {
    pub rows: Range<usize>,
    pub cols: Range<usize>,
}

impl SelRect {
    pub fn contains(&self, d: usize, c: usize) -> bool {
        self.rows.contains(&d) && self.cols.contains(&c)
    }
}

fn span(a: usize, b: usize) -> Range<usize> {
    a.min(b)..a.max(b) + 1
}

impl GridState {
    /// The selected rectangle, when wider than the focused cell alone
    /// (dragged back to its first cell: the focused cell alone).
    pub fn selection_rect(&self, ncols: usize) -> Option<SelRect> {
        let sel = self.selection?;
        let (fr, fc) = self.selected?;
        let (fpos, apos) = (
            display_pos(&self.view, fr)?,
            display_pos(&self.view, sel.anchor.0)?,
        );
        let rows = if sel.all_rows {
            0..self.total_rows()
        } else {
            span(apos, fpos)
        };
        let cols = if sel.all_cols {
            0..ncols
        } else {
            span(sel.anchor.1, fc)
        };
        (rows.len() * cols.len() > 1).then_some(SelRect { rows, cols })
    }

    /// Shift+click on a cell: from the anchor (the focused cell when
    /// nothing is selected yet) to (r, c).
    pub(super) fn extend_to(&mut self, r: RowRef, c: usize) {
        if self.selection.is_none() {
            self.selection = Some(Selection::cells(self.selected.unwrap_or((r, c))));
        }
        self.selected = Some((r, c));
    }

    /// Shift+arrows: the focus moves, the anchor stays.
    pub(super) fn extend_by(&mut self, ncols: usize, dr: isize, dc: isize) {
        let anchor = self.selected;
        self.move_focus(ncols, dr, dc, false);
        if let (None, Some(a)) = (self.selection, anchor) {
            self.selection = Some(Selection::cells(a));
        }
    }

    /// Click on a row number: that row; with Shift, the rows from the
    /// anchor's to `r`.
    pub(super) fn select_rows(&mut self, r: RowRef, extend: bool) {
        let col = self.selected.map_or(0, |(_, c)| c);
        match (extend, self.selected, self.selection) {
            (true, Some(_), Some(sel)) => {
                self.selection = Some(Selection {
                    all_cols: true,
                    all_rows: false,
                    ..sel
                })
            }
            (true, Some(focus), None) => {
                self.selection = Some(Selection {
                    all_cols: true,
                    ..Selection::cells(focus)
                })
            }
            _ => {
                self.selection = Some(Selection {
                    all_cols: true,
                    ..Selection::cells((r, col))
                })
            }
        }
        self.selected = Some((r, col));
    }

    /// Shift+click on a header: that column, or up to it when columns are
    /// already selected.
    pub(super) fn select_column(&mut self, c: usize) {
        if self.total_rows() == 0 {
            return;
        }
        let row = self
            .selected
            .map_or_else(|| row_ref(&self.view, 0), |(r, _)| r);
        match self.selection {
            Some(sel) if sel.all_rows && !sel.all_cols => {}
            _ => {
                self.selection = Some(Selection {
                    all_rows: true,
                    ..Selection::cells((row, c))
                })
            }
        }
        self.selected = Some((row, c));
    }

    /// Ctrl+A: every cell; the focus stays where it is.
    pub(super) fn select_all(&mut self) {
        if self.total_rows() == 0 {
            return;
        }
        let focus = *self
            .selected
            .get_or_insert_with(|| (row_ref(&self.view, 0), 0));
        self.selection = Some(Selection {
            all_cols: true,
            all_rows: true,
            ..Selection::cells(focus)
        });
    }

    /// Suppr: the selected cells (or the focused one) set to NULL, as
    /// pending edits; deleted rows are left alone.
    pub(super) fn null_selected(&mut self, result: &QueryResult) {
        let cells: Vec<(RowRef, usize)> = match self.selection_rect(result.columns.len()) {
            Some(s) => s
                .rows
                .flat_map(|d| s.cols.clone().map(move |c| (d, c)))
                .map(|(d, c)| (row_ref(&self.view, d), c))
                .collect(),
            None => self.selected.into_iter().collect(),
        };
        for (r, c) in cells {
            if !self.edits.is_deleted(r) {
                self.edits.set(&result.rows, r, c, NULL_CELL.into());
            }
        }
    }

    /// The selection as CSV (header line, then the rows in display order;
    /// NULL and missing fields as empty fields, the empty string as `""`),
    /// pending edits applied.
    pub fn selection_csv(&self, result: &QueryResult) -> Option<String> {
        let rect = self.selection_rect(result.columns.len())?;
        let sub = QueryResult {
            columns: rect
                .cols
                .clone()
                .map(|c| result.columns[c].clone())
                .collect(),
            rows: rect
                .rows
                .clone()
                .map(|d| {
                    let r = row_ref(&self.view, d);
                    rect.cols
                        .clone()
                        .map(|c| match self.edits.value(&result.rows, r, c) {
                            v if is_absent(v) => NULL_CELL.to_string(),
                            v => v.to_string(),
                        })
                        .collect()
                })
                .collect(),
            ..Default::default()
        };
        let mut csv = export_to_csv_quoting_empty(&sub);
        if csv.ends_with('\n') {
            csv.pop();
        }
        Some(csv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::Column;

    fn result() -> QueryResult {
        let column = |name: &str| Column {
            name: name.into(),
            type_name: "text".into(),
            nullable: true,
            is_primary_key: false,
        };
        QueryResult {
            columns: vec![column("id"), column("name"), column("note")],
            rows: vec![
                vec!["1".into(), "Alice".into(), "a, b".into()],
                vec!["2".into(), "Bob".into(), NULL_CELL.into()],
                vec!["3".into(), "Carol".into(), "say \"hi\"".into()],
            ],
            ..Default::default()
        }
    }

    fn grid(r: &QueryResult) -> GridState {
        let mut g = GridState::default();
        g.ensure_view(&r.rows);
        g
    }

    #[test]
    fn a_single_focused_cell_is_no_selection() {
        let r = result();
        let mut g = grid(&r);
        g.selected = Some((RowRef::Base(0), 1));
        assert_eq!(g.selection_rect(3), None);
        assert_eq!(g.selection_csv(&r), None);
    }

    #[test]
    fn shift_click_selects_a_rectangle_copied_as_csv() {
        let r = result();
        let mut g = grid(&r);
        g.selected = Some((RowRef::Base(2), 2));
        g.extend_to(RowRef::Base(1), 1);
        assert_eq!(
            g.selection_rect(3),
            Some(SelRect {
                rows: 1..3,
                cols: 1..3
            })
        );
        assert_eq!(
            g.selection_csv(&r).unwrap(),
            "name,note\nBob,\nCarol,\"say \"\"hi\"\"\""
        );
    }

    #[test]
    fn shift_arrows_extend_from_the_focused_cell() {
        let r = result();
        let mut g = grid(&r);
        g.selected = Some((RowRef::Base(0), 0));
        g.extend_by(3, 1, 0);
        g.extend_by(3, 0, 1);
        assert_eq!(g.selected, Some((RowRef::Base(1), 1)));
        assert_eq!(g.selection_csv(&r).unwrap(), "id,name\n1,Alice\n2,Bob");
    }

    #[test]
    fn row_numbers_select_whole_rows() {
        let r = result();
        let mut g = grid(&r);
        g.select_rows(RowRef::Base(0), false);
        g.select_rows(RowRef::Base(1), true);
        assert_eq!(
            g.selection_csv(&r).unwrap(),
            "id,name,note\n1,Alice,\"a, b\"\n2,Bob,"
        );
        // Without Shift: that row alone.
        g.select_rows(RowRef::Base(2), false);
        assert_eq!(g.selection_rect(3).unwrap().rows, 2..3);
    }

    #[test]
    fn headers_select_whole_columns_in_display_order() {
        let r = result();
        let mut g = GridState {
            sort: Some((1, false)),
            ..Default::default()
        };
        g.ensure_view(&r.rows);
        g.select_column(0);
        assert_eq!(g.selection_csv(&r).unwrap(), "id\n3\n2\n1");
        g.select_column(1);
        assert_eq!(
            g.selection_rect(3),
            Some(SelRect {
                rows: 0..3,
                cols: 0..2
            })
        );
    }

    #[test]
    fn select_all_copies_every_cell_with_pending_edits() {
        let r = result();
        let mut g = grid(&r);
        g.edits.set(&r.rows, RowRef::Base(0), 1, "Zoe".into());
        g.edits.add_row(3);
        g.select_all();
        assert_eq!(g.selected, Some((RowRef::Base(0), 0)), "focus kept");
        let csv = g.selection_csv(&r).unwrap();
        assert!(csv.starts_with("id,name,note\n1,Zoe,"), "{csv}");
        assert_eq!(csv.lines().count(), 5, "header, 3 rows, the new row");
    }

    #[test]
    fn suppr_sets_the_selection_or_the_focused_cell_to_null() {
        let r = result();
        let mut g = grid(&r);
        g.edits.toggle_delete(RowRef::Base(1));
        g.selected = Some((RowRef::Base(0), 0));
        g.extend_to(RowRef::Base(2), 1);
        g.null_selected(&r);
        let v = |g: &GridState, row, c| g.edits.value(&r.rows, RowRef::Base(row), c).to_string();
        for row in [0, 2] {
            assert_eq!(
                (v(&g, row, 0), v(&g, row, 1)),
                (NULL_CELL.into(), NULL_CELL.into())
            );
            assert_ne!(v(&g, row, 2), NULL_CELL, "outside the selection");
        }
        assert_eq!(v(&g, 1, 0), "2", "deleted row left alone");
        // No selection: the focused cell.
        let mut g = grid(&r);
        g.selected = Some((RowRef::Base(1), 2));
        g.null_selected(&r);
        assert_eq!(g.edits.count(), 0, "already NULL");
        g.selected = Some((RowRef::Base(0), 2));
        g.null_selected(&r);
        assert_eq!(v(&g, 0, 2), NULL_CELL);
        assert_eq!(g.edits.count(), 1);
    }

    #[test]
    fn a_selection_on_a_vanished_row_is_dropped() {
        let r = result();
        let mut g = grid(&r);
        g.edits.add_row(3);
        g.selected = Some((RowRef::New(0), 0));
        g.extend_to(RowRef::Base(0), 0);
        assert!(g.selection_rect(3).is_some());
        g.revert();
        g.clamp(&r.rows, 3);
        assert_eq!(g.selection, None);
    }
}
