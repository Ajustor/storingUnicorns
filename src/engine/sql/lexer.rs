//! SQL tokenizer and context-aware completions (UI-agnostic).

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::OnceLock;

/// SQL Keywords for syntax highlighting
const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "LIKE",
    "BETWEEN",
    "IS",
    "NULL",
    "TRUE",
    "FALSE",
    "AS",
    "ON",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "NATURAL",
    "USING",
    "ORDER",
    "BY",
    "ASC",
    "DESC",
    "LIMIT",
    "OFFSET",
    "GROUP",
    "HAVING",
    "DISTINCT",
    "ALL",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "INDEX",
    "VIEW",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "CONSTRAINT",
    "DEFAULT",
    "CHECK",
    "UNIQUE",
    "CASCADE",
    "TRUNCATE",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "GRANT",
    "REVOKE",
    "TOP",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "EXISTS",
    "ANY",
    "SOME",
    "COALESCE",
    "NULLIF",
    "CAST",
    "CONVERT",
];

/// SQL Functions for highlighting
const SQL_FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "ROUND",
    "FLOOR",
    "CEIL",
    "ABS",
    "UPPER",
    "LOWER",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "LENGTH",
    "LEN",
    "SUBSTRING",
    "SUBSTR",
    "REPLACE",
    "CONCAT",
    "COALESCE",
    "NULLIF",
    "NOW",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATE",
    "TIME",
    "DATETIME",
    "YEAR",
    "MONTH",
    "DAY",
    "HOUR",
    "MINUTE",
    "SECOND",
    "DATEADD",
    "DATEDIFF",
    "GETDATE",
    "GETUTCDATE",
    "ISNULL",
    "IFNULL",
    "NVL",
    "DECODE",
    "IIF",
];

/// SQL Operators
const SQL_OPERATORS: &[char] = &['=', '<', '>', '!', '+', '-', '*', '/', '%', '|', '&', '^'];

/// Token types for SQL
#[derive(Debug, Clone, PartialEq)]
pub enum SqlToken {
    Keyword(String),
    Function(String),
    String(String),
    Number(String),
    Operator(String),
    Comment(String),
    Identifier(String),
    Column(String), // Highlighted column from table
    Punctuation(String),
    Whitespace(String),
}

/// Kind of a token, without its text (see [`tokenize_spans`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Keyword,
    Function,
    String,
    Number,
    Operator,
    Comment,
    Identifier,
    Column,
    Punctuation,
    Whitespace,
}

impl TokenKind {
    fn with_text(self, text: String) -> SqlToken {
        match self {
            TokenKind::Keyword => SqlToken::Keyword(text),
            TokenKind::Function => SqlToken::Function(text),
            TokenKind::String => SqlToken::String(text),
            TokenKind::Number => SqlToken::Number(text),
            TokenKind::Operator => SqlToken::Operator(text),
            TokenKind::Comment => SqlToken::Comment(text),
            TokenKind::Identifier => SqlToken::Identifier(text),
            TokenKind::Column => SqlToken::Column(text),
            TokenKind::Punctuation => SqlToken::Punctuation(text),
            TokenKind::Whitespace => SqlToken::Whitespace(text),
        }
    }
}

impl SqlToken {
    pub fn text(&self) -> &str {
        match self {
            SqlToken::Keyword(s)
            | SqlToken::Function(s)
            | SqlToken::String(s)
            | SqlToken::Number(s)
            | SqlToken::Operator(s)
            | SqlToken::Comment(s)
            | SqlToken::Identifier(s)
            | SqlToken::Column(s)
            | SqlToken::Punctuation(s)
            | SqlToken::Whitespace(s) => s,
        }
    }
}

/// Tokenize SQL query for syntax highlighting
pub fn tokenize_sql(query: &str, known_columns: &[String]) -> Vec<SqlToken> {
    tokenize_spans(query, known_columns)
        .into_iter()
        .map(|(kind, range)| kind.with_text(query[range].to_string()))
        .collect()
}

fn is_operator(b: u8) -> bool {
    SQL_OPERATORS.contains(&(b as char))
}

/// Byte offset of the first character at or after `i` failing `keep`.
fn scan_while(text: &str, i: usize, keep: impl Fn(char) -> bool) -> usize {
    text[i..]
        .char_indices()
        .find(|&(_, c)| !keep(c))
        .map_or(text.len(), |(p, _)| i + p)
}

type WordSet = HashSet<&'static str>;

/// Lookup sets of the keywords and functions (upper case).
fn word_sets() -> &'static (WordSet, WordSet) {
    static SETS: OnceLock<(WordSet, WordSet)> = OnceLock::new();
    SETS.get_or_init(|| {
        (
            SQL_KEYWORDS.iter().copied().collect(),
            SQL_FUNCTIONS.iter().copied().collect(),
        )
    })
}

/// Known columns, compared ignoring ASCII case.
enum Columns<'a> {
    /// A few: a linear scan allocates nothing.
    Few(&'a [String]),
    /// Many, in ASCII lower case.
    Many(HashSet<String>),
}

impl<'a> Columns<'a> {
    fn new(columns: &'a [String]) -> Self {
        if columns.len() <= 16 {
            Columns::Few(columns)
        } else {
            Columns::Many(columns.iter().map(|c| c.to_ascii_lowercase()).collect())
        }
    }

    fn contains(&self, word: &str) -> bool {
        match self {
            Columns::Few(cols) => cols.iter().any(|c| c.eq_ignore_ascii_case(word)),
            Columns::Many(set) => set.contains(word.to_ascii_lowercase().as_str()),
        }
    }
}

/// Kind of `word`: keyword, function, known column or plain identifier.
fn classify_word(word: &str, columns: &Columns) -> TokenKind {
    let (keywords, functions) = word_sets();
    // Keywords are short ASCII words: upper-case into a stack buffer.
    let mut buf = [0u8; 32];
    let upper: Cow<str> = if word.is_ascii() && word.len() <= buf.len() {
        let buf = &mut buf[..word.len()];
        buf.copy_from_slice(word.as_bytes());
        buf.make_ascii_uppercase();
        Cow::Borrowed(std::str::from_utf8(buf).unwrap_or_default())
    } else {
        Cow::Owned(word.to_uppercase())
    };
    if keywords.contains(upper.as_ref()) {
        TokenKind::Keyword
    } else if functions.contains(upper.as_ref()) {
        TokenKind::Function
    } else if columns.contains(word) {
        TokenKind::Column
    } else {
        TokenKind::Identifier
    }
}

/// Tokens of `query` as (kind, byte range), contiguous and covering the
/// whole text, without allocating a string per token.
pub fn tokenize_spans(query: &str, known_columns: &[String]) -> Vec<(TokenKind, Range<usize>)> {
    let columns = Columns::new(known_columns);
    let b = query.as_bytes();
    let n = b.len();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < n {
        let start = i;
        let c = query[i..].chars().next().unwrap_or_default();
        let next = b.get(i + 1).copied();
        let kind = if c.is_whitespace() {
            i = scan_while(query, i, char::is_whitespace);
            TokenKind::Whitespace
        } else if c == '-' && next == Some(b'-') {
            // Single-line comment, up to (not including) the newline.
            i = query[i..].find('\n').map_or(n, |p| i + p);
            TokenKind::Comment
        } else if c == '/' && next == Some(b'*') {
            // Block comment; an unterminated one runs to the end.
            i = query[i + 2..].find("*/").map_or(n, |p| i + 2 + p + 2);
            TokenKind::Comment
        } else if c == '\'' || c == '"' {
            // String literal; a doubled quote is an escaped one.
            let quote = b[i];
            i += 1;
            loop {
                match b[i..].iter().position(|&x| x == quote) {
                    None => {
                        i = n;
                        break;
                    }
                    Some(p) => {
                        i += p + 1;
                        if b.get(i) == Some(&quote) {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                }
            }
            TokenKind::String
        } else if c.is_ascii_digit() || (c == '.' && next.is_some_and(|x| x.is_ascii_digit())) {
            while i < n && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            TokenKind::Number
        } else if c.is_ascii() && is_operator(b[i]) {
            while i < n && is_operator(b[i]) {
                i += 1;
            }
            TokenKind::Operator
        } else if matches!(c, '(' | ')' | ',' | ';' | '.' | '[' | ']') {
            i += 1;
            TokenKind::Punctuation
        } else if c.is_alphabetic() || c == '_' || c == '@' || c == '#' {
            // Always consume the first char (handles '@', '#' prefixes like
            // SQL Server variables/temp tables, which aren't alphanumeric).
            i = scan_while(query, i + c.len_utf8(), |c| c.is_alphanumeric() || c == '_');
            classify_word(&query[start..i], &columns)
        } else {
            // Unknown character
            i += c.len_utf8();
            TokenKind::Punctuation
        };
        tokens.push((kind, start..i));
    }

    tokens
}

/// Find the last whole-word occurrence of `kw` (uppercase) in `text` (uppercase).
/// Returns the byte position just after the keyword, or None.
fn last_keyword_pos(text: &str, kw: &str) -> Option<usize> {
    if kw.is_empty() || text.len() < kw.len() {
        return None;
    }
    let kw_len = kw.len();
    let mut result = None;
    let mut start = 0;
    while start + kw_len <= text.len() {
        if text[start..].starts_with(kw) {
            let end = start + kw_len;
            let before_ok = start == 0
                || text[..start]
                    .chars()
                    .last()
                    .map(|c| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(true);
            let after_ok = end >= text.len()
                || text[end..]
                    .chars()
                    .next()
                    .map(|c| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(true);
            if before_ok && after_ok {
                result = Some(end);
            }
        }
        let next = text[start..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        start += next;
    }
    result
}

/// Extract the current SQL token from text ending at the cursor.
/// Handles quoted identifiers (`"…"`, `` `…` ``, `[…]`).
/// Returns `(token_start_byte, inner_text, opening_quote_char)`.
/// `token_start_byte` points to the opening quote if in a quote, else the first word char.
/// `inner_text` is the text without the opening quote.
pub fn extract_token(text: &str) -> (usize, String, Option<char>) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut in_quote: Option<char> = None;
    let mut quote_start = 0usize;
    let mut i = 0;

    while i < chars.len() {
        let (pos, c) = chars[i];
        match (c, in_quote) {
            ('"', None) | ('`', None) => {
                in_quote = Some(c);
                quote_start = pos;
                i += 1;
            }
            ('[', None) => {
                in_quote = Some('[');
                quote_start = pos;
                i += 1;
            }
            (c2, Some(open)) if c2 == open && open != '[' => {
                // Check for doubled/escaped quote (e.g. "" inside a string)
                if i + 1 < chars.len() && chars[i + 1].1 == open {
                    i += 2;
                } else {
                    in_quote = None;
                    i += 1;
                }
            }
            (']', Some('[')) => {
                in_quote = None;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    if let Some(open_q) = in_quote {
        let inner_start = quote_start + open_q.len_utf8();
        return (quote_start, text[inner_start..].to_string(), Some(open_q));
    }

    // No unclosed quote — find the word boundary (alphanumeric + underscore only)
    let word_start = text
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);

    (word_start, text[word_start..].to_string(), None)
}

/// If `text` ends with a schema qualifier like `schema.` or `"schema".` or `[schema].`,
/// return the schema name (unquoted).
fn extract_schema_qualifier(text: &str) -> Option<String> {
    if !text.ends_with('.') {
        return None;
    }
    let before_dot = &text[..text.len() - 1];
    if before_dot.is_empty() {
        return None;
    }
    let last = before_dot.chars().last().unwrap();
    match last {
        '"' | '`' => {
            let inner_end = before_dot.len() - last.len_utf8();
            before_dot[..inner_end]
                .rfind(last)
                .map(|p| before_dot[p + last.len_utf8()..inner_end].to_string())
        }
        ']' => before_dot
            .rfind('[')
            .map(|p| before_dot[p + 1..before_dot.len() - 1].to_string()),
        c if c.is_alphanumeric() || c == '_' => {
            let start = before_dot
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            let name = &before_dot[start..];
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        }
        _ => None,
    }
}

/// Format an identifier, wrapping in quotes if it contains special characters
/// or if the user started typing with a specific quote style.
fn format_identifier(name: &str, quote_char: Option<char>) -> String {
    match quote_char {
        Some('[') => format!("[{}]", name),
        Some(q) => format!("{}{}{}", q, name, q),
        None if name.chars().any(|c| !c.is_alphanumeric() && c != '_') => {
            format!("\"{}\"", name)
        }
        None => name.to_string(),
    }
}

/// Get completion suggestions based on current word and context
pub fn get_completions(
    query: &str,
    cursor_pos: usize,
    known_columns: &[String],
    known_tables: &[String],
) -> Vec<String> {
    let before_cursor = &query[..cursor_pos.min(query.len())];

    // Find the current token, handling quoted identifiers
    let (word_start, current_word, quote_char) = extract_token(before_cursor);
    let before_token = &before_cursor[..word_start];

    // Check for a schema qualifier immediately before the current token (e.g. "schema".)
    let schema_qualifier = extract_schema_qualifier(before_token);

    // Only bail out if there's nothing to complete and no schema context
    if current_word.is_empty() && quote_char.is_none() && schema_qualifier.is_none() {
        return Vec::new();
    }

    // Use the uppercase text before the token for context detection.
    // Strip the schema qualifier part so it doesn't confuse keyword detection.
    let context_text = if schema_qualifier.is_some() {
        // Drop trailing "schema." (and any quote chars around schema name)
        before_token
            .trim_end_matches(|c: char| {
                c.is_alphanumeric()
                    || c == '_'
                    || c == '.'
                    || c == '"'
                    || c == '`'
                    || c == '['
                    || c == ']'
            })
            .to_uppercase()
    } else {
        before_token.to_uppercase()
    };

    // Determine context via last-keyword wins
    const TABLE_KWS: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];
    const COLUMN_KWS: &[&str] = &[
        "SELECT", "WHERE", "AND", "OR", "SET", "ON", "BY", "HAVING", "DISTINCT",
    ];

    let last_table = TABLE_KWS
        .iter()
        .filter_map(|kw| last_keyword_pos(&context_text, kw))
        .max();
    let last_column = COLUMN_KWS
        .iter()
        .filter_map(|kw| last_keyword_pos(&context_text, kw))
        .max();

    // A schema qualifier always means table context
    let table_context = schema_qualifier.is_some()
        || matches!((last_table, last_column), (Some(t), Some(c)) if t > c)
        || matches!((last_table, last_column), (Some(_), None));

    // SELECT clause: last column-context keyword was SELECT itself
    let last_select = last_keyword_pos(&context_text, "SELECT");
    let in_select_clause = !table_context && last_select.is_some() && last_select == last_column;

    let word_upper = current_word.to_uppercase();
    let mut suggestions = Vec::new();

    if table_context {
        for table in known_tables {
            // Tables are stored as "schema.table" or just "table"
            let (t_schema, t_name) = table
                .find('.')
                .map(|d| (Some(&table[..d]), &table[d + 1..]))
                .unwrap_or((None, table.as_str()));

            let matches = if let Some(ref schema) = schema_qualifier {
                // Must match schema name and table prefix
                t_schema
                    .map(|s| s.eq_ignore_ascii_case(schema))
                    .unwrap_or(false)
                    && t_name.to_uppercase().starts_with(&word_upper)
            } else {
                // Match against the full "schema.table" or just "table"
                table.to_uppercase().starts_with(&word_upper)
                    || t_name.to_uppercase().starts_with(&word_upper)
            };

            if matches {
                let suggestion = if schema_qualifier.is_some() {
                    // Schema already typed — suggest only the table part
                    format_identifier(t_name, quote_char)
                } else if quote_char.is_some() {
                    // Inside an open quote — suggest full name, will be closed on apply
                    table.clone()
                } else {
                    // Unquoted — format the full reference, quoting if needed
                    match (t_schema, t_name) {
                        (Some(s), n)
                            if s.chars().any(|c| !c.is_alphanumeric() && c != '_')
                                || n.chars().any(|c| !c.is_alphanumeric() && c != '_') =>
                        {
                            format!("\"{}\".\"{}\"", s, n)
                        }
                        _ => table.clone(),
                    }
                };
                suggestions.push(suggestion);
            }
        }
    } else if in_select_clause {
        for col in known_columns {
            if col.to_uppercase().starts_with(&word_upper) {
                suggestions.push(col.clone());
            }
        }
        for func in SQL_FUNCTIONS {
            if func.starts_with(&word_upper) {
                suggestions.push(format!("{}()", func));
            }
        }
        for kw in SQL_KEYWORDS {
            if kw.starts_with(&word_upper) {
                suggestions.push(kw.to_string());
            }
        }
    } else if last_column.is_some() {
        // Condition / general column context
        for col in known_columns {
            if col.to_uppercase().starts_with(&word_upper) {
                suggestions.push(col.clone());
            }
        }
        for kw in SQL_KEYWORDS {
            if kw.starts_with(&word_upper) {
                suggestions.push(kw.to_string());
            }
        }
    } else {
        // Default: columns → keywords → functions
        for col in known_columns {
            if col.to_uppercase().starts_with(&word_upper) {
                suggestions.push(col.clone());
            }
        }
        for kw in SQL_KEYWORDS {
            if kw.starts_with(&word_upper) {
                suggestions.push(kw.to_string());
            }
        }
        for func in SQL_FUNCTIONS {
            if func.starts_with(&word_upper) {
                suggestions.push(format!("{}()", func));
            }
        }
    }

    // Remove duplicates while preserving order
    let mut seen = HashSet::new();
    suggestions.retain(|s| seen.insert(s.clone()));
    suggestions.truncate(10);
    suggestions
}

/// Table the query works on (after FROM, UPDATE or INTO), with its outer
/// quotes stripped, for context-aware completion. Not to be confused with
/// `statements::extract_table_from_query`, which keeps the quotes so the name
/// can be reused in SQL.
pub fn completion_context_table(query: &str) -> Option<String> {
    let query_upper = query.to_uppercase();

    // Try to find table name after FROM
    if let Some(from_pos) = query_upper.find("FROM") {
        let after_from = query[from_pos + 4..].trim_start();
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
            return Some(
                table_name
                    .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
                    .to_string(),
            );
        }
    }

    // Try UPDATE table_name
    if let Some(update_pos) = query_upper.find("UPDATE") {
        let after_update = query[update_pos + 6..].trim_start();
        let table_name: String = after_update
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
            return Some(
                table_name
                    .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
                    .to_string(),
            );
        }
    }

    // Try INSERT INTO table_name
    if let Some(into_pos) = query_upper.find("INTO") {
        let after_into = query[into_pos + 4..].trim_start();
        let table_name: String = after_into
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
            return Some(
                table_name
                    .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
                    .to_string(),
            );
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: typing '@' (or '#') used to send tokenize_sql into an infinite
    // loop because the token-start branch accepted '@'/'#' but the consumption
    // loop never advanced past them, freezing/crashing the app on every render.
    #[test]
    fn tokenize_at_and_hash_do_not_loop() {
        assert_eq!(tokenize_sql("@", &[]), vec![SqlToken::Identifier("@".into())]);
        assert_eq!(tokenize_sql("#", &[]), vec![SqlToken::Identifier("#".into())]);
    }

    #[test]
    fn tokenize_sql_server_variable() {
        assert_eq!(
            tokenize_sql("@var", &[]),
            vec![SqlToken::Identifier("@var".into())]
        );
        assert_eq!(
            tokenize_sql("#temp", &[]),
            vec![SqlToken::Identifier("#temp".into())]
        );
    }

    #[test]
    fn tokenize_at_in_context() {
        // '@' followed by a space, embedded in a real query.
        let tokens = tokenize_sql("SELECT @ FROM t", &[]);
        assert!(tokens.contains(&SqlToken::Identifier("@".into())));
        assert!(tokens.contains(&SqlToken::Keyword("SELECT".into())));
    }

    #[test]
    fn spans_cover_text_and_match_tokens() {
        let text =
            "SELECT é_col, \"Nom\" FROM t -- c\n/* b */ WHERE x >= 1.5 AND y = 'it''s' ;@v #t ¤";
        let cols = vec!["X".to_string(), "é_col".to_string()];
        // Many columns take the hashed lookup: same tokens.
        let many: Vec<String> = (0..40)
            .map(|i| format!("c{i}"))
            .chain(cols.clone())
            .collect();
        assert_eq!(tokenize_sql(text, &many), tokenize_sql(text, &cols));
        let spans = tokenize_spans(text, &cols);
        let mut end = 0;
        for (_, r) in &spans {
            assert_eq!(r.start, end, "contiguous");
            end = r.end;
        }
        assert_eq!(end, text.len());
        let tokens = tokenize_sql(text, &cols);
        assert_eq!(tokens.len(), spans.len());
        for (t, (k, r)) in tokens.iter().zip(&spans) {
            assert_eq!(*t, k.with_text(text[r.clone()].to_string()));
        }
        assert!(tokens.contains(&SqlToken::Column("x".into())));
        assert!(tokens.contains(&SqlToken::Column("é_col".into())));
        assert!(tokens.contains(&SqlToken::String("'it''s'".into())));
        assert!(tokens.contains(&SqlToken::Comment("/* b */".into())));
        assert!(tokens.contains(&SqlToken::Comment("-- c".into())));
        assert!(tokens.contains(&SqlToken::Number("1.5".into())));
        assert!(tokens.contains(&SqlToken::Operator(">=".into())));
        assert!(tokens.contains(&SqlToken::Punctuation("¤".into())));
    }

    #[test]
    fn keywords_and_functions_ignore_case() {
        let tokens = tokenize_sql("select Count(x) from t", &[]);
        assert_eq!(tokens[0], SqlToken::Keyword("select".into()));
        assert_eq!(tokens[2], SqlToken::Function("Count".into()));
    }

    #[test]
    fn unterminated_comment_and_string_run_to_the_end() {
        assert_eq!(
            tokenize_sql("a /* open", &[]).last(),
            Some(&SqlToken::Comment("/* open".into()))
        );
        assert_eq!(
            tokenize_sql("a 'open", &[]).last(),
            Some(&SqlToken::String("'open".into()))
        );
    }
}
