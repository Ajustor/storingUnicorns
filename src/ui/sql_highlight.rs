use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

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

/// Tokenize SQL query for syntax highlighting
pub fn tokenize_sql(query: &str, known_columns: &[String]) -> Vec<SqlToken> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Whitespace
        if c.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            tokens.push(SqlToken::Whitespace(chars[start..i].iter().collect()));
            continue;
        }

        // Single-line comment (-- ...)
        if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            tokens.push(SqlToken::Comment(chars[start..i].iter().collect()));
            continue;
        }

        // Block comment (/* ... */)
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            if i + 1 < chars.len() {
                i += 2; // Skip */
            }
            tokens.push(SqlToken::Comment(chars[start..i].iter().collect()));
            continue;
        }

        // String literals ('...' or "...")
        if c == '\'' || c == '"' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == quote {
                    // Check for escaped quote
                    if i + 1 < chars.len() && chars[i + 1] == quote {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(SqlToken::String(chars[start..i].iter().collect()));
            continue;
        }

        // Numbers
        if c.is_ascii_digit() || (c == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            tokens.push(SqlToken::Number(chars[start..i].iter().collect()));
            continue;
        }

        // Operators
        if SQL_OPERATORS.contains(&c) {
            let start = i;
            while i < chars.len() && SQL_OPERATORS.contains(&chars[i]) {
                i += 1;
            }
            tokens.push(SqlToken::Operator(chars[start..i].iter().collect()));
            continue;
        }

        // Punctuation
        if c == '(' || c == ')' || c == ',' || c == ';' || c == '.' || c == '[' || c == ']' {
            tokens.push(SqlToken::Punctuation(c.to_string()));
            i += 1;
            continue;
        }

        // Identifiers and keywords
        if c.is_alphabetic() || c == '_' || c == '@' || c == '#' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let upper = word.to_uppercase();

            if SQL_KEYWORDS.contains(&upper.as_str()) {
                tokens.push(SqlToken::Keyword(word));
            } else if SQL_FUNCTIONS.contains(&upper.as_str()) {
                tokens.push(SqlToken::Function(word));
            } else if known_columns
                .iter()
                .any(|col| col.eq_ignore_ascii_case(&word))
            {
                tokens.push(SqlToken::Column(word));
            } else {
                tokens.push(SqlToken::Identifier(word));
            }
            continue;
        }

        // Unknown character
        tokens.push(SqlToken::Punctuation(c.to_string()));
        i += 1;
    }

    tokens
}

/// Convert tokens to styled spans for ratatui
pub fn tokens_to_spans(tokens: &[SqlToken]) -> Vec<Span<'static>> {
    tokens
        .iter()
        .map(|token| match token {
            SqlToken::Keyword(s) => Span::styled(
                s.clone(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            SqlToken::Function(s) => Span::styled(s.clone(), Style::default().fg(Color::Yellow)),
            SqlToken::String(s) => Span::styled(s.clone(), Style::default().fg(Color::Green)),
            SqlToken::Number(s) => Span::styled(s.clone(), Style::default().fg(Color::Cyan)),
            SqlToken::Operator(s) => Span::styled(s.clone(), Style::default().fg(Color::Red)),
            SqlToken::Comment(s) => Span::styled(
                s.clone(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            SqlToken::Column(s) => Span::styled(
                s.clone(),
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            SqlToken::Identifier(s) => Span::styled(s.clone(), Style::default().fg(Color::White)),
            SqlToken::Punctuation(s) => Span::styled(s.clone(), Style::default().fg(Color::Gray)),
            SqlToken::Whitespace(s) => Span::raw(s.clone()),
        })
        .collect()
}

/// Highlight SQL query and return styled lines
pub fn highlight_sql(query: &str, known_columns: &[String]) -> Vec<Line<'static>> {
    if query.is_empty() {
        return vec![Line::from("")];
    }

    let tokens = tokenize_sql(query, known_columns);
    let spans = tokens_to_spans(&tokens);

    // Split spans by newlines to create multiple lines
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_line_spans: Vec<Span<'static>> = Vec::new();

    for span in spans {
        let text = span.content.to_string();
        if text.contains('\n') {
            // Split this span at newlines
            let parts: Vec<&str> = text.split('\n').collect();
            for (idx, part) in parts.iter().enumerate() {
                if idx > 0 {
                    // Push the current line and start a new one
                    lines.push(Line::from(std::mem::take(&mut current_line_spans)));
                }
                if !part.is_empty() {
                    current_line_spans.push(Span::styled(part.to_string(), span.style));
                }
            }
        } else {
            current_line_spans.push(span);
        }
    }

    // Don't forget the last line
    if !current_line_spans.is_empty() {
        lines.push(Line::from(current_line_spans));
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
    }

    lines
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
        let next = text[start..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        start += next;
    }
    result
}

/// Extract the current SQL token from text ending at the cursor.
/// Handles quoted identifiers (`"…"`, `` `…` ``, `[…]`).
/// Returns `(token_start_byte, inner_text, opening_quote_char)`.
/// `token_start_byte` points to the opening quote if in a quote, else the first word char.
/// `inner_text` is the text without the opening quote.
pub(crate) fn extract_token(text: &str) -> (usize, String, Option<char>) {
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
        None if name
            .chars()
            .any(|c| !c.is_alphanumeric() && c != '_') =>
        {
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
                c.is_alphanumeric() || c == '_' || c == '.' || c == '"' || c == '`' || c == '[' || c == ']'
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
    let mut seen = std::collections::HashSet::new();
    suggestions.retain(|s| seen.insert(s.clone()));
    suggestions.truncate(10);
    suggestions
}

/// Extract table name from a SQL query for context-aware completion
pub fn extract_table_from_query(query: &str) -> Option<String> {
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
