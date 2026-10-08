//! Splitting SQL text into statements and execution units, plus small
//! identifier helpers.

use crate::engine::models::DatabaseType;

/// The unit of SQL to execute when the user triggers execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionUnit {
    /// A single statement (the historical behavior).
    Single(String),
    /// A full transaction block: ordered statements including the opening
    /// `BEGIN`/`START TRANSACTION` and the closing `COMMIT`/`ROLLBACK`/`END`.
    Transaction(Vec<String>),
    /// The cursor is inside a `BEGIN` that has no matching terminator.
    UnterminatedTransaction,
}

/// Split SQL into top-level statements, ignoring `;` inside string literals
/// (`'…'`, `"…"`) and comments (`-- …`, `/* … */`).
/// Returns `(start_byte, end_byte, trimmed_text)` for each non-empty statement.
pub fn split_statements(input: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut statements = Vec::new();
    let mut stmt_start = 0usize;
    let mut i = 0usize;

    let push_stmt = |statements: &mut Vec<(usize, usize, String)>, start: usize, end: usize| {
        let text = input[start..end].trim();
        if !text.is_empty() {
            statements.push((start, end, text.to_string()));
        }
    };

    while i < chars.len() {
        let (pos, c) = chars[i];
        match c {
            '\'' | '"' => {
                let quote = c;
                i += 1;
                while i < chars.len() {
                    let cc = chars[i].1;
                    if cc == quote {
                        // Doubled quote = escaped quote inside the literal
                        if i + 1 < chars.len() && chars[i + 1].1 == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '-' if i + 1 < chars.len() && chars[i + 1].1 == '-' => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < chars.len() && chars[i + 1].1 == '*' => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i].1 == '*' && chars[i + 1].1 == '/') {
                    i += 1;
                }
                if i + 1 < chars.len() {
                    i += 2; // skip */
                }
            }
            ';' => {
                push_stmt(&mut statements, stmt_start, pos);
                stmt_start = pos + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }

    // Trailing statement without a terminating ';'
    push_stmt(&mut statements, stmt_start, input.len());
    statements
}

/// Classify a statement as a transaction start, returning true for
/// `BEGIN`, `BEGIN TRANSACTION`, `BEGIN TRAN`, `START TRANSACTION`.
pub fn is_transaction_start(stmt: &str) -> bool {
    let mut words = stmt.split_whitespace();
    match words.next().map(|w| w.to_uppercase()) {
        Some(w) if w == "BEGIN" => true,
        Some(w) if w == "START" => words
            .next()
            .map(|w2| w2.eq_ignore_ascii_case("TRANSACTION"))
            .unwrap_or(false),
        _ => false,
    }
}

/// Classify a statement as a transaction terminator:
/// `COMMIT`, `ROLLBACK`, `END` (and their `… TRANSACTION/TRAN` variants).
pub fn is_transaction_end(stmt: &str) -> bool {
    match stmt.split_whitespace().next().map(|w| w.to_uppercase()) {
        Some(w) => w == "COMMIT" || w == "ROLLBACK" || w == "END",
        None => false,
    }
}

/// Determine what to execute given the cursor position.
/// If the cursor sits inside a `BEGIN … COMMIT/ROLLBACK/END` block, the whole
/// block is returned; otherwise the single statement at the cursor.
pub fn get_execution_unit_at_cursor(input: &str, cursor_pos: usize) -> ExecutionUnit {
    let statements = split_statements(input);
    if statements.is_empty() {
        return ExecutionUnit::Single(String::new());
    }

    // Index of the statement containing (or following) the cursor.
    let cursor_idx = statements
        .iter()
        .position(|(_, end, _)| cursor_pos <= *end)
        .unwrap_or(statements.len() - 1);

    // Walk backward to find an enclosing transaction start.
    let mut start_idx = None;
    for j in (0..=cursor_idx).rev() {
        let text = &statements[j].2;
        if is_transaction_start(text) {
            start_idx = Some(j);
            break;
        }
        if j < cursor_idx && is_transaction_end(text) {
            // A previous transaction already closed before the cursor.
            break;
        }
    }

    let Some(si) = start_idx else {
        return ExecutionUnit::Single(statements[cursor_idx].2.clone());
    };

    // Find the matching terminator at or after the start.
    let end_idx = (si..statements.len()).find(|&j| is_transaction_end(&statements[j].2));

    match end_idx {
        Some(ei) => {
            ExecutionUnit::Transaction(statements[si..=ei].iter().map(|s| s.2.clone()).collect())
        }
        None => ExecutionUnit::UnterminatedTransaction,
    }
}

/// Extract table name from a query (simple heuristic for SELECT ... FROM table)
/// Supports schema.table format and quoted identifiers
pub fn extract_table_from_query(query: &str) -> Option<String> {
    let query_upper = query.to_uppercase();
    if let Some(from_pos) = query_upper.find("FROM") {
        let after_from = query[from_pos + 4..].trim_start();
        // Take the first word after FROM (including schema.table and quoted identifiers)
        let table_name: String = after_from
            .chars()
            .take_while(|c| {
                c.is_alphanumeric()
                    || *c == '_'
                    || *c == '.'
                    || *c == '['
                    || *c == ']'
                    || *c == '"'
                    || *c == '`'
            })
            .collect();
        if !table_name.is_empty() {
            return Some(table_name);
        }
    }
    None
}

/// Identifier quote characters `(open, close)` for a database type.
pub fn quote_chars(db_type: &DatabaseType) -> (char, char) {
    match db_type {
        DatabaseType::Postgres | DatabaseType::SQLite => ('"', '"'),
        DatabaseType::MySQL => ('`', '`'),
        DatabaseType::SQLServer | DatabaseType::Azure => ('[', ']'),
    }
}

#[cfg(test)]
mod execution_unit_tests {
    use super::{get_execution_unit_at_cursor, ExecutionUnit};

    fn tx(stmts: &[&str]) -> ExecutionUnit {
        ExecutionUnit::Transaction(stmts.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn single_statement_outside_transaction() {
        assert_eq!(
            get_execution_unit_at_cursor("SELECT 1", 0),
            ExecutionUnit::Single("SELECT 1".into())
        );
    }

    #[test]
    fn picks_statement_at_cursor_among_many() {
        let sql = "SELECT 1; SELECT 2; SELECT 3";
        // cursor inside "SELECT 2"
        assert_eq!(
            get_execution_unit_at_cursor(sql, 12),
            ExecutionUnit::Single("SELECT 2".into())
        );
    }

    #[test]
    fn detects_begin_commit_block_from_begin() {
        let sql = "BEGIN;\nINSERT INTO t VALUES (1);\nCOMMIT;";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            tx(&["BEGIN", "INSERT INTO t VALUES (1)", "COMMIT"])
        );
    }

    #[test]
    fn detects_block_when_cursor_inside_block() {
        let sql = "BEGIN;\nINSERT INTO t VALUES (1);\nCOMMIT;";
        // cursor on the INSERT line
        let cursor = sql.find("INSERT").unwrap();
        assert_eq!(
            get_execution_unit_at_cursor(sql, cursor),
            tx(&["BEGIN", "INSERT INTO t VALUES (1)", "COMMIT"])
        );
    }

    #[test]
    fn supports_start_transaction_keyword() {
        let sql = "START TRANSACTION;\nUPDATE t SET a = 1;\nCOMMIT;";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            tx(&["START TRANSACTION", "UPDATE t SET a = 1", "COMMIT"])
        );
    }

    #[test]
    fn supports_begin_tran_and_rollback() {
        let sql = "BEGIN TRAN;\nDELETE FROM t;\nROLLBACK;";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            tx(&["BEGIN TRAN", "DELETE FROM t", "ROLLBACK"])
        );
    }

    #[test]
    fn supports_end_as_terminator() {
        let sql = "BEGIN;\nINSERT INTO t VALUES (1);\nEND;";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            tx(&["BEGIN", "INSERT INTO t VALUES (1)", "END"])
        );
    }

    #[test]
    fn semicolon_inside_string_does_not_split() {
        let sql = "BEGIN;\nINSERT INTO t VALUES (';');\nCOMMIT;";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            tx(&["BEGIN", "INSERT INTO t VALUES (';')", "COMMIT"])
        );
    }

    #[test]
    fn begin_without_terminator_is_unterminated() {
        let sql = "BEGIN;\nINSERT INTO t VALUES (1);";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            ExecutionUnit::UnterminatedTransaction
        );
    }

    #[test]
    fn statement_after_completed_transaction_is_single() {
        let sql = "BEGIN;\nINSERT INTO t VALUES (1);\nCOMMIT;\nSELECT 2;";
        let cursor = sql.find("SELECT 2").unwrap();
        assert_eq!(
            get_execution_unit_at_cursor(sql, cursor),
            ExecutionUnit::Single("SELECT 2".into())
        );
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn split_ignores_semicolons_in_comments() {
        let stmts = split_statements("SELECT 1; -- a;b\nSELECT 2; /* x; */ SELECT 3");
        let texts: Vec<_> = stmts.iter().map(|s| s.2.as_str()).collect();
        assert_eq!(texts, ["SELECT 1", "-- a;b\nSELECT 2", "/* x; */ SELECT 3"]);
    }

    #[test]
    fn extract_table_handles_schema_and_quotes() {
        assert_eq!(
            extract_table_from_query("select * from dbo.[Users] where 1=1"),
            Some("dbo.[Users]".into())
        );
        assert_eq!(extract_table_from_query("SELECT 1"), None);
    }

    #[test]
    fn quote_chars_per_db() {
        assert_eq!(quote_chars(&DatabaseType::MySQL), ('`', '`'));
        assert_eq!(quote_chars(&DatabaseType::Azure), ('[', ']'));
        assert_eq!(quote_chars(&DatabaseType::Postgres), ('"', '"'));
    }
}
