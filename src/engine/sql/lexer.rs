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

/// The SQL keywords (upper case), in suggestion order.
pub(crate) fn keywords() -> &'static [&'static str] {
    SQL_KEYWORDS
}

/// Whether `word` is an SQL keyword, ignoring case.
pub(crate) fn is_keyword(word: &str) -> bool {
    word_sets().0.contains(word.to_uppercase().as_str())
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
