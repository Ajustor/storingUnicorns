//! Pending edits of a data grid. The grid shows `base` rows (from the
//! database) plus inserted rows; edits never mutate `base` until Submit
//! succeeds.

use std::collections::{BTreeMap, BTreeSet};

use crate::engine::models::NULL_CELL;
use crate::engine::ops::rows::RowChanges;

/// Identifies a displayed row: an existing row by its index in the base
/// result, or a new row by its index in `inserted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowRef {
    Base(usize),
    New(usize),
}

#[derive(Debug, Default, Clone)]
pub struct PendingEdits {
    /// (base row, column) → new value
    edited: BTreeMap<(usize, usize), String>,
    deleted: BTreeSet<usize>,
    inserted: Vec<Vec<String>>,
}

impl PendingEdits {
    pub fn is_empty(&self) -> bool {
        self.edited.is_empty() && self.deleted.is_empty() && self.inserted.is_empty()
    }

    /// Base rows with edits that are not also marked deleted.
    fn edited_rows(&self) -> BTreeSet<usize> {
        self.edited
            .keys()
            .map(|&(row, _)| row)
            .filter(|row| !self.deleted.contains(row))
            .collect()
    }

    /// Number of changed rows (edited rows + deleted + inserted).
    pub fn count(&self) -> usize {
        self.edited_rows().len() + self.deleted.len() + self.inserted.len()
    }

    /// Value to display for a cell, taking edits into account.
    pub fn value<'a>(&'a self, base: &'a [Vec<String>], row: RowRef, col: usize) -> &'a str {
        let cell = match row {
            RowRef::Base(i) => self
                .edited
                .get(&(i, col))
                .or_else(|| base.get(i).and_then(|r| r.get(col))),
            RowRef::New(i) => self.inserted.get(i).and_then(|r| r.get(col)),
        };
        cell.map_or("", String::as_str)
    }

    /// Whether a base cell was changed. New rows: false (the whole row is new).
    pub fn is_edited(&self, row: RowRef, col: usize) -> bool {
        matches!(row, RowRef::Base(i) if self.edited.contains_key(&(i, col)))
    }

    pub fn is_deleted(&self, row: RowRef) -> bool {
        matches!(row, RowRef::Base(i) if self.deleted.contains(&i))
    }

    /// Set a cell. Setting a base cell back to its original value removes the edit.
    pub fn set(&mut self, base: &[Vec<String>], row: RowRef, col: usize, value: String) {
        match row {
            RowRef::Base(i) => {
                let original = base.get(i).and_then(|r| r.get(col));
                if original == Some(&value) {
                    self.edited.remove(&(i, col));
                } else {
                    self.edited.insert((i, col), value);
                }
            }
            RowRef::New(i) => {
                if let Some(cell) = self.inserted.get_mut(i).and_then(|r| r.get_mut(col)) {
                    *cell = value;
                }
            }
        }
    }

    /// Append a new row of `ncols` NULL cells; returns its ref.
    pub fn add_row(&mut self, ncols: usize) -> RowRef {
        self.inserted.push(vec![NULL_CELL.to_string(); ncols]);
        RowRef::New(self.inserted.len() - 1)
    }

    /// Base row: toggle deletion mark (and drop its edits when marking).
    /// New row: remove it (later new rows shift down by one).
    pub fn toggle_delete(&mut self, row: RowRef) {
        match row {
            RowRef::Base(i) => {
                if !self.deleted.remove(&i) {
                    self.deleted.insert(i);
                    self.edited.retain(|&(r, _), _| r != i);
                }
            }
            RowRef::New(i) => {
                if i < self.inserted.len() {
                    self.inserted.remove(i);
                }
            }
        }
    }

    pub fn new_rows(&self) -> usize {
        self.inserted.len()
    }

    /// Convert to the engine format. Base rows both edited and deleted only
    /// appear in `deletes`.
    pub fn to_row_changes(&self, base: &[Vec<String>]) -> RowChanges {
        let updates = self
            .edited_rows()
            .into_iter()
            .filter_map(|i| {
                let original = base.get(i)?;
                let edited = (0..original.len())
                    .map(|col| self.value(base, RowRef::Base(i), col).to_string())
                    .collect();
                Some((original.clone(), edited))
            })
            .collect();
        let deletes = self
            .deleted
            .iter()
            .filter_map(|&i| base.get(i).cloned())
            .collect();
        RowChanges {
            updates,
            inserts: self.inserted.clone(),
            deletes,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<Vec<String>> {
        vec![vec!["1".into(), "a".into()], vec!["2".into(), "b".into()]]
    }

    #[test]
    fn edit_display_and_revert_to_original() {
        let b = base();
        let mut p = PendingEdits::default();
        p.set(&b, RowRef::Base(0), 1, "z".into());
        assert_eq!(p.value(&b, RowRef::Base(0), 1), "z");
        assert!(p.is_edited(RowRef::Base(0), 1));
        assert_eq!(p.count(), 1);
        p.set(&b, RowRef::Base(0), 1, "a".into());
        assert!(p.is_empty());
    }

    #[test]
    fn new_rows_and_deletes() {
        let b = base();
        let mut p = PendingEdits::default();
        let r = p.add_row(2);
        p.set(&b, r, 1, "c".into());
        assert_eq!(p.value(&b, r, 1), "c");
        p.toggle_delete(RowRef::Base(1));
        assert!(p.is_deleted(RowRef::Base(1)));
        assert_eq!(p.count(), 2);
        p.toggle_delete(r); // removing a new row drops it
        assert_eq!(p.new_rows(), 0);
        p.toggle_delete(RowRef::Base(1));
        assert!(p.is_empty());
    }

    #[test]
    fn to_row_changes() {
        let b = base();
        let mut p = PendingEdits::default();
        p.set(&b, RowRef::Base(0), 1, "z".into());
        p.set(&b, RowRef::Base(1), 1, "y".into());
        p.toggle_delete(RowRef::Base(1)); // deleted wins over edited
        let r = p.add_row(2);
        p.set(&b, r, 1, "c".into());
        let c = p.to_row_changes(&b);
        assert_eq!(
            c.updates,
            vec![(b[0].clone(), vec!["1".to_string(), "z".to_string()])]
        );
        assert_eq!(c.deletes, vec![b[1].clone()]);
        assert_eq!(
            c.inserts,
            vec![vec![NULL_CELL.to_string(), "c".to_string()]]
        );
    }

    #[test]
    fn several_edits_in_one_row_count_once_and_merge() {
        let b = base();
        let mut p = PendingEdits::default();
        p.set(&b, RowRef::Base(1), 0, "9".into());
        p.set(&b, RowRef::Base(1), 1, "q".into());
        assert_eq!(p.count(), 1);
        assert!(!p.is_edited(RowRef::Base(0), 0));
        assert_eq!(p.value(&b, RowRef::Base(1), 0), "9");
        assert_eq!(p.value(&b, RowRef::Base(0), 1), "a");
        let c = p.to_row_changes(&b);
        assert_eq!(
            c.updates,
            vec![(b[1].clone(), vec!["9".to_string(), "q".to_string()])]
        );
    }

    #[test]
    fn new_rows_are_not_edited_or_deleted_and_clear_resets() {
        let b = base();
        let mut p = PendingEdits::default();
        let r = p.add_row(2);
        assert_eq!(r, RowRef::New(0));
        assert_eq!(p.add_row(2), RowRef::New(1));
        p.set(&b, r, 0, "x".into());
        assert!(!p.is_edited(r, 0));
        assert!(!p.is_deleted(r));
        assert_eq!(p.new_rows(), 2);
        // Marking a row deleted drops its edits; unmarking does not restore them.
        p.set(&b, RowRef::Base(0), 1, "z".into());
        p.toggle_delete(RowRef::Base(0));
        p.toggle_delete(RowRef::Base(0));
        assert_eq!(p.value(&b, RowRef::Base(0), 1), "a");
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.count(), 0);
    }
}
