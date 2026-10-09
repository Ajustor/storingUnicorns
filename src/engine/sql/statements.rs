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
            // Postgres dollar quoting: `$$ … $$` or `$tag$ … $tag$` (a
            // function body full of `;`). `$1` is a parameter, not a quote.
            '$' if dollar_tag(&input[pos..]).is_some()
                && !(i > 0 && is_ident_char(chars[i - 1].1)) =>
            {
                let tag = dollar_tag(&input[pos..]).unwrap_or_default();
                let body = pos + tag.len();
                let end = input[body..]
                    .find(tag)
                    .map_or(input.len(), |at| body + at + tag.len());
                while i < chars.len() && chars[i].0 < end {
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

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The dollar-quote opening `text` (`$$` or `$tag$`, the tag an identifier
/// not starting with a digit), if any.
fn dollar_tag(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('$')?;
    let len = rest.find('$')?;
    let tag = &rest[..len];
    let valid = !tag.starts_with(|c: char| c.is_ascii_digit())
        && tag.chars().all(|c| c.is_alphanumeric() || c == '_');
    valid.then(|| &text[..len + 2])
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
/// `ROLLBACK [WORK|TRAN|TRANSACTION] TO [SAVEPOINT] x` keeps the transaction
/// open, so it is not one.
pub fn is_transaction_end(stmt: &str) -> bool {
    let words: Vec<String> = stmt
        .split_whitespace()
        .take(3)
        .map(|w| w.to_uppercase())
        .collect();
    match words.first().map(String::as_str) {
        Some("COMMIT" | "END") => true,
        Some("ROLLBACK") => !words[1..].iter().any(|w| w == "TO"),
        _ => false,
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

/// A token of a query, as needed to recognise its shape. Whitespace and
/// comments are dropped.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A bare word: keyword or unquoted identifier (`valid_from`, `@x`).
    Word(String),
    /// A quoted identifier, quotes included (`"t"`, `` `t` ``, `[t]`).
    Quoted(String),
    /// A string literal.
    Str,
    Number,
    /// Any other single character (`.`, `,`, `(`, `)`, `*`, `;`, `=`…).
    Punct(char),
}

impl Tok {
    /// Whether this is the keyword `kw` (upper case).
    fn is(&self, kw: &str) -> bool {
        matches!(self, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
    }

    fn is_name(&self) -> bool {
        matches!(self, Tok::Word(_) | Tok::Quoted(_))
    }

    fn text(&self) -> &str {
        match self {
            Tok::Word(w) | Tok::Quoted(w) => w,
            _ => "",
        }
    }
}

/// Split `sql` into tokens, skipping whitespace and comments. String
/// literals and quoted identifiers are single tokens, so words inside them
/// (or inside identifiers such as `valid_from`) are never keywords.
fn tokens(sql: &str) -> Vec<Tok> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let word_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '@' | '#');
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && next == Some('-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if matches!(c, '\'' | '"' | '`' | '[') {
            let close = if c == '[' { ']' } else { c };
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == close {
                    if chars.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(chars.len());
            out.push(if c == '\'' {
                Tok::Str
            } else {
                Tok::Quoted(chars[start..i].iter().collect())
            });
        } else if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            out.push(Tok::Number);
        } else if word_char(c) {
            let start = i;
            while i < chars.len() && word_char(chars[i]) {
                i += 1;
            }
            out.push(Tok::Word(chars[start..i].iter().collect()));
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

/// A possibly qualified name starting at `toks[i]` (`t`, `s.t`, `"s"."t"`):
/// its text without whitespace and the index after it.
fn qualified_name(toks: &[Tok], mut i: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    loop {
        let part = toks.get(i).filter(|t| t.is_name())?;
        name.push_str(part.text());
        i += 1;
        if toks.get(i) != Some(&Tok::Punct('.')) {
            return Some((name, i));
        }
        name.push('.');
        i += 1;
    }
}

/// Clause keywords that may follow a table name, so they are not its alias.
const AFTER_TABLE: &[&str] = &[
    "WHERE",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "NATURAL",
    "OUTER",
    "STRAIGHT_JOIN",
    "GROUP",
    "HAVING",
    "WINDOW",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "MINUS",
    "FOR",
    "WITH",
    "ON",
    "USING",
    "INTO",
    "QUALIFY",
];

/// Keywords that may not appear (outside parentheses) after the table of a
/// `single_table_source` query.
const NOT_AFTER_SOURCE: &[&str] = &[
    "JOIN",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "MINUS",
    "GROUP",
    "HAVING",
    "WINDOW",
    "INTO",
    "FOR",
    "FROM",
    "SELECT",
    "WITH",
];

/// Table name of a query (the first name after a top-level `FROM` keyword),
/// as written, quotes kept so it can be reused in SQL. Lenient: the query
/// may join other tables. Used for titles and default export names; use
/// `single_table_source` to decide whether rows can be edited.
pub fn extract_table_from_query(query: &str) -> Option<String> {
    let toks = tokens(query);
    let mut depth = 0i32;
    for (i, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            t if depth == 0 && t.is("FROM") => {
                return qualified_name(&toks, i + 1).map(|(name, _)| name);
            }
            _ => {}
        }
    }
    None
}

/// The table a query reads when each of its rows is one row of that table,
/// with its columns unchanged; `None` otherwise. Only this shape is accepted:
///
/// `SELECT [TOP n] <cols> FROM <table> [[AS] alias] [WHERE …] [ORDER BY …]
/// [LIMIT/OFFSET/FETCH …]`
///
/// where `<cols>` lists `*`, `alias.*` or plain (possibly qualified) column
/// names, without aliases or expressions (`SELECT name AS id` would make
/// edits of "id" write to another column). Joins, comma lists, subqueries in
/// FROM, set operations, GROUP BY/HAVING, DISTINCT, CTEs, `INTO`, `FOR XML`
/// and several statements are rejected. Keywords are recognised as tokens,
/// never inside identifiers (`valid_from`) or strings. The name is returned
/// as written (quotes kept).
pub fn single_table_source(sql: &str) -> Option<String> {
    let mut toks = tokens(sql);
    if toks.last() == Some(&Tok::Punct(';')) {
        toks.pop();
    }
    if !toks.first()?.is("SELECT") {
        return None;
    }
    let mut i = 1;
    if toks.get(i)?.is("TOP") {
        i += 1;
        match toks.get(i)? {
            Tok::Number => i += 1,
            Tok::Punct('(')
                if toks.get(i + 1) == Some(&Tok::Number)
                    && toks.get(i + 2) == Some(&Tok::Punct(')')) =>
            {
                i += 3
            }
            _ => return None,
        }
    }

    // Select list: `*`, `q.*` or `[q.]col`, comma separated, up to FROM.
    loop {
        let tok = toks.get(i)?;
        if *tok == Tok::Punct('*') {
            i += 1;
        } else if tok.is_name() && !tok.is("FROM") && !tok.is("DISTINCT") && !tok.is("ALL") {
            i += 1;
            while toks.get(i) == Some(&Tok::Punct('.')) {
                match toks.get(i + 1)? {
                    Tok::Punct('*') => {
                        i += 2;
                        break;
                    }
                    t if t.is_name() => i += 2,
                    _ => return None,
                }
            }
        } else {
            return None;
        }
        match toks.get(i)? {
            Tok::Punct(',') => i += 1,
            t if t.is("FROM") => break,
            _ => return None,
        }
    }
    i += 1; // FROM

    let (table, mut i) = qualified_name(&toks, i)?;
    let is_clause = |t: &Tok| AFTER_TABLE.iter().any(|kw| t.is(kw));
    // Optional alias.
    if toks.get(i).is_some_and(|t| t.is("AS")) {
        i += 1;
        if !toks.get(i).is_some_and(|t| t.is_name() && !is_clause(t)) {
            return None;
        }
        i += 1;
    } else if toks.get(i).is_some_and(|t| t.is_name() && !is_clause(t)) {
        i += 1;
    }

    // What follows: only filtering, ordering and paging clauses.
    match toks.get(i) {
        None => return Some(table),
        Some(t)
            if ["WHERE", "ORDER", "LIMIT", "OFFSET", "FETCH"]
                .iter()
                .any(|kw| t.is(kw)) => {}
        Some(_) => return None,
    }
    let mut depth = 0i32;
    for tok in &toks[i..] {
        match tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            // Another statement.
            Tok::Punct(';') => return None,
            t if depth == 0 && NOT_AFTER_SOURCE.iter().any(|kw| t.is(kw)) => return None,
            _ => {}
        }
    }
    (depth == 0).then_some(table)
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
    fn rollback_to_savepoint_does_not_end_the_block() {
        let sql = "BEGIN;
SAVEPOINT a;
DELETE FROM t;
ROLLBACK TO SAVEPOINT a;
                   rollback work to a;
COMMIT;";
        assert_eq!(
            get_execution_unit_at_cursor(sql, 0),
            tx(&[
                "BEGIN",
                "SAVEPOINT a",
                "DELETE FROM t",
                "ROLLBACK TO SAVEPOINT a",
                "rollback work to a",
                "COMMIT"
            ])
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
    fn split_keeps_dollar_quoted_bodies_whole() {
        let texts = |sql: &str| {
            split_statements(sql)
                .into_iter()
                .map(|s| s.2)
                .collect::<Vec<_>>()
        };
        let body = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql";
        assert_eq!(texts(&format!("{body}; SELECT 1")), [body, "SELECT 1"]);
        let tagged = "DO $fn$ BEGIN PERFORM 1; END $fn$";
        assert_eq!(texts(&format!("{tagged};SELECT 2")), [tagged, "SELECT 2"]);
        // Parameters and identifiers containing `$` are not quotes.
        assert_eq!(texts("SELECT $1; SELECT a$b$c FROM t; SELECT 3").len(), 3);
        // An unterminated quote runs to the end.
        assert_eq!(texts("SELECT $$ a; b").len(), 1);
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
    fn extract_table_ignores_from_inside_identifiers_and_strings() {
        assert_eq!(
            extract_table_from_query("SELECT valid_from FROM users"),
            Some("users".into())
        );
        assert_eq!(
            extract_table_from_query("SELECT 'FROM x' AS \"from\" FROM t1"),
            Some("t1".into())
        );
        assert_eq!(
            extract_table_from_query(
                "-- FROM nope
SELECT a FROM \"main\".\"t\" u"
            ),
            Some("\"main\".\"t\"".into())
        );
        assert_eq!(
            extract_table_from_query("SELECT a FROM `db`.`my t` JOIN b ON 1=1"),
            Some("`db`.`my t`".into())
        );
        assert_eq!(extract_table_from_query("SELECT fromage FROM"), None);
    }

    #[test]
    fn single_table_source_accepts_one_plain_table() {
        let some = |s: &str| Some(s.to_string());
        for (sql, table) in [
            ("SELECT * FROM users", "users"),
            ("select * from users;", "users"),
            ("SELECT valid_from FROM users", "users"),
            (
                "SELECT id, name FROM dbo.[Users] WHERE id > 1",
                "dbo.[Users]",
            ),
            ("SELECT u.id, u.* FROM users u ORDER BY u.id DESC", "users"),
            (
                "SELECT \"id\" FROM \"main\".\"t\" AS x LIMIT 5 OFFSET 10",
                "\"main\".\"t\"",
            ),
            (
                "SELECT * FROM `db`.`t` WHERE (a = 1 OR b IN (SELECT b FROM c))",
                "`db`.`t`",
            ),
            (
                "SELECT * FROM [t] ORDER BY (SELECT NULL) OFFSET 0 ROWS FETCH NEXT 500 ROWS ONLY",
                "[t]",
            ),
            ("SELECT TOP 100 * FROM t", "t"),
            ("SELECT TOP (10) id FROM t", "t"),
            (
                "/* c */ SELECT * -- x
 FROM t WHERE a = 'UNION'",
                "t",
            ),
        ] {
            assert_eq!(single_table_source(sql), some(table), "{sql}");
        }
    }

    #[test]
    fn single_table_source_rejects_other_shapes() {
        for sql in [
            "SELECT 1",
            "SELECT * FROM a JOIN b ON a.id = b.id",
            "SELECT * FROM a INNER JOIN b ON a.id = b.id",
            "SELECT * FROM a LEFT OUTER JOIN b USING (id)",
            "SELECT * FROM a NATURAL JOIN b",
            "SELECT * FROM a CROSS JOIN b",
            "SELECT * FROM a, b",
            "SELECT * FROM a x, b y WHERE x.id = y.id",
            "SELECT * FROM (SELECT * FROM t) s",
            "SELECT * FROM t UNION SELECT * FROM u",
            "SELECT * FROM t WHERE a = 1 UNION ALL SELECT * FROM t",
            "SELECT * FROM t INTERSECT SELECT * FROM u",
            "SELECT * FROM t EXCEPT SELECT * FROM u",
            "SELECT a, COUNT(*) FROM t GROUP BY a",
            "SELECT a FROM t GROUP BY a HAVING COUNT(*) > 1",
            "SELECT DISTINCT a FROM t",
            "WITH c AS (SELECT * FROM t) SELECT * FROM c",
            "SELECT name AS id FROM t",
            "SELECT name id FROM t",
            "SELECT upper(name) FROM t",
            "SELECT a + 1 FROM t",
            "SELECT * FROM t; DELETE FROM t",
            "SELECT * FROM t FOR XML AUTO",
            "SELECT * FROM t WITH (NOLOCK)",
            "SELECT * INTO u FROM t",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "INSERT INTO t SELECT * FROM u",
            "SELECT * FROM",
            "",
        ] {
            assert_eq!(single_table_source(sql), None, "{sql}");
        }
    }

    #[test]
    fn quote_chars_per_db() {
        assert_eq!(quote_chars(&DatabaseType::MySQL), ('`', '`'));
        assert_eq!(quote_chars(&DatabaseType::Azure), ('[', ']'));
        assert_eq!(quote_chars(&DatabaseType::Postgres), ('"', '"'));
    }
}
