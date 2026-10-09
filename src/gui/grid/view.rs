//! The rows a grid shows: client-side filter and sort, and the mapping
//! between display positions and rows.

use super::changes::{PendingEdits, RowRef};
use crate::engine::models::{display_cell, is_null};
use std::cmp::Ordering;

use super::GridState;

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

/// Row shown at display position `d`: the view, then the new rows.
pub(super) fn row_ref(view: &[usize], d: usize) -> RowRef {
    if d < view.len() {
        RowRef::Base(view[d])
    } else {
        RowRef::New(d - view.len())
    }
}

pub(super) fn display_pos(view: &[usize], r: RowRef) -> Option<usize> {
    match r {
        RowRef::Base(i) => view.iter().position(|&v| v == i),
        RowRef::New(k) => Some(view.len() + k),
    }
}

impl GridState {
    pub(super) fn ensure_view(&mut self, rows: &[Vec<String>]) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::NULL_CELL;

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
}
