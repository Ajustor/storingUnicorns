//! Pasting into a grid: CSV (our copies) or TSV (spreadsheets) spread over
//! the cells from the focused one, like a spreadsheet does.

use super::selection::Selection;
use super::view::{display_pos, row_ref};
use super::GridState;
use crate::engine::models::{QueryResult, NULL_CELL};

/// The clipboard as a block of values. Tab-separated when it has a tab,
/// else CSV when it has several lines, else a single value taken as is (a
/// copied cell may hold commas). An unquoted empty field is NULL, a quoted
/// one (`""`) the empty string.
pub fn parse_clipboard(text: &str) -> Vec<Vec<String>> {
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }
    let sep = if text.contains('\t') {
        '\t'
    } else if text.contains('\n') {
        ','
    } else {
        return vec![vec![text.to_string()]];
    };
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let (mut in_quotes, mut quoted) = (false, false);
    let finish = |field: &mut String, quoted: &mut bool| {
        let value = match (std::mem::take(field), *quoted) {
            (v, false) if v.is_empty() => NULL_CELL.to_string(),
            (v, _) => v,
        };
        *quoted = false;
        value
    };
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => in_quotes = false,
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() && !quoted => {
                in_quotes = true;
                quoted = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                record.push(finish(&mut field, &mut quoted));
                records.push(std::mem::take(&mut record));
            }
            c if c == sep => record.push(finish(&mut field, &mut quoted)),
            c => field.push(c),
        }
    }
    record.push(finish(&mut field, &mut quoted));
    records.push(record);
    records
}

impl GridState {
    /// Paste `text` into pending edits: from the top-left cell of the
    /// selection (or the focused cell), clipped to the grid. Like a
    /// spreadsheet, a selection whose size is a multiple of the block's
    /// (a single value: any selection) is filled by repeating it. A first
    /// line made of column names (our copies' header) is skipped; deleted
    /// rows are left alone. The pasted cells end up selected. Whether
    /// anything was written.
    pub(super) fn paste(&mut self, result: &QueryResult, text: &str) -> bool {
        let mut block = parse_clipboard(text);
        let names = |row: &Vec<String>| {
            row.iter()
                .all(|f| result.columns.iter().any(|c| c.name == *f))
        };
        if block.len() > 1 && names(&block[0]) {
            block.remove(0);
        }
        let (h, w) = (block.len(), block.iter().map(Vec::len).max().unwrap_or(0));
        let (total, ncols) = (self.total_rows(), result.columns.len());
        if h == 0 || w == 0 || total == 0 || ncols == 0 {
            return false;
        }
        let (top, left, rows, cols) = match self.selection_rect(ncols) {
            Some(s) if s.rows.len() % h == 0 && s.cols.len() % w == 0 => {
                (s.rows.start, s.cols.start, s.rows.len(), s.cols.len())
            }
            Some(s) => (s.rows.start, s.cols.start, h, w),
            None => {
                let Some((pos, c)) = self
                    .selected
                    .and_then(|(r, c)| Some((display_pos(&self.view, r)?, c)))
                else {
                    return false;
                };
                (pos, c, h, w)
            }
        };
        let (rows, cols) = (rows.min(total - top), cols.min(ncols - left));
        for i in 0..rows {
            let r = row_ref(&self.view, top + i);
            if self.edits.is_deleted(r) {
                continue;
            }
            for j in 0..cols {
                if let Some(v) = block[i % h].get(j % w) {
                    self.edits.set(&result.rows, r, left + j, v.clone());
                }
            }
        }
        let corner = |i: usize, j: usize| (row_ref(&self.view, top + i), left + j);
        self.selected = Some(corner(0, 0));
        self.selection = (rows * cols > 1).then(|| Selection::cells(corner(rows - 1, cols - 1)));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::changes::RowRef;
    use super::*;
    use crate::engine::models::Column;

    fn result(nrows: usize) -> QueryResult {
        let column = |name: &str| Column {
            name: name.into(),
            type_name: "text".into(),
            nullable: true,
            is_primary_key: false,
        };
        QueryResult {
            columns: vec![column("id"), column("name"), column("note")],
            rows: (0..nrows)
                .map(|r| (0..3).map(|c| format!("r{r}c{c}")).collect())
                .collect(),
            ..Default::default()
        }
    }

    fn grid(r: &QueryResult, focus: (usize, usize)) -> GridState {
        let mut g = GridState::default();
        g.ensure_view(&r.rows);
        g.selected = Some((RowRef::Base(focus.0), focus.1));
        g
    }

    fn cell(g: &GridState, r: &QueryResult, row: usize, col: usize) -> String {
        g.edits.value(&r.rows, RowRef::Base(row), col).to_string()
    }

    #[test]
    fn clipboard_is_read_as_tsv_csv_or_a_single_value() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_clipboard("a, b"), vec![s(&["a, b"])], "one value");
        assert_eq!(parse_clipboard("a, b\n"), vec![s(&["a, b"])], "one line");
        assert_eq!(
            parse_clipboard("1\ta, b\r\n2\t\r\n"),
            vec![s(&["1", "a, b"]), s(&["2", NULL_CELL])],
            "Excel's tabs, CRLF and trailing newline"
        );
        assert_eq!(
            parse_clipboard("x,y\n\"multi\nline\",\"say \"\"hi\"\"\"\n,\"\""),
            vec![
                s(&["x", "y"]),
                s(&["multi\nline", "say \"hi\""]),
                s(&[NULL_CELL, ""])
            ],
            "quoted newlines and quotes; NULL vs empty string"
        );
        assert!(parse_clipboard("\n").is_empty());
    }

    #[test]
    fn our_copy_pastes_back_without_its_header() {
        let r = result(3);
        let mut g = grid(&r, (0, 1));
        g.paste(&r, "name,note\nBob,\n\"\",\"a, b\"");
        assert_eq!(cell(&g, &r, 0, 1), "Bob");
        assert_eq!(cell(&g, &r, 0, 2), NULL_CELL);
        assert_eq!(cell(&g, &r, 1, 1), "");
        assert_eq!(cell(&g, &r, 1, 2), "a, b");
        assert_eq!(cell(&g, &r, 2, 1), "r2c1", "untouched");
        assert_eq!(g.selected, Some((RowRef::Base(0), 1)));
        let s = g.selection_rect(3).unwrap();
        assert_eq!((s.rows, s.cols), (0..2, 1..3), "pasted cells selected");
    }

    #[test]
    fn a_block_past_the_grid_is_clipped() {
        let r = result(2);
        let mut g = grid(&r, (1, 2));
        assert!(g.paste(&r, "a\tb\nc\td\n"));
        assert_eq!(cell(&g, &r, 1, 2), "a");
        assert_eq!(g.edits.count(), 1, "other cells fall outside");
    }

    #[test]
    fn a_single_value_fills_the_selection() {
        let r = result(3);
        let mut g = grid(&r, (0, 0));
        g.extend_to(RowRef::Base(2), 1);
        g.paste(&r, "x");
        for row in 0..3 {
            assert_eq!(cell(&g, &r, row, 0), "x");
            assert_eq!(cell(&g, &r, row, 1), "x");
            assert_eq!(cell(&g, &r, row, 2), format!("r{row}c2"));
        }
    }

    #[test]
    fn a_block_is_repeated_over_a_multiple_selection_only() {
        let r = result(4);
        let mut g = grid(&r, (0, 0));
        g.extend_to(RowRef::Base(3), 0);
        g.paste(&r, "a\nb\n");
        let col0: Vec<_> = (0..4).map(|row| cell(&g, &r, row, 0)).collect();
        assert_eq!(col0, ["a", "b", "a", "b"], "2 lines over 4 rows");
        // 2 columns into 1 selected: not a multiple, pasted once.
        let mut g = grid(&r, (0, 0));
        g.extend_to(RowRef::Base(3), 0);
        g.paste(&r, "a\tz\nb\tz\n");
        assert_eq!(cell(&g, &r, 0, 1), "z");
        assert_eq!(cell(&g, &r, 2, 0), "r2c0");
        // 3 rows selected, 2 lines: pasted once from the top.
        let mut g = grid(&r, (0, 2));
        g.extend_to(RowRef::Base(2), 2);
        g.paste(&r, "a\nb\n");
        assert_eq!(cell(&g, &r, 1, 2), "b");
        assert_eq!(cell(&g, &r, 2, 2), "r2c2");
    }

    #[test]
    fn deleted_rows_are_skipped_and_new_rows_filled() {
        let r = result(2);
        let mut g = grid(&r, (0, 0));
        g.edits.toggle_delete(RowRef::Base(1));
        g.edits.add_row(3);
        g.paste(&r, "a\nb\nc\n");
        assert_eq!(cell(&g, &r, 0, 0), "a");
        assert_eq!(cell(&g, &r, 1, 0), "r1c0", "deleted row untouched");
        assert_eq!(g.edits.value(&r.rows, RowRef::New(0), 0), "c");
    }
}
