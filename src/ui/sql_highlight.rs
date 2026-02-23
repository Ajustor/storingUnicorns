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

/// Get completion suggestions based on current word and context
pub fn get_completions(
    query: &str,
    cursor_pos: usize,
    known_columns: &[String],
    known_tables: &[String],
) -> Vec<String> {
    // Find the current word being typed
    let before_cursor = &query[..cursor_pos.min(query.len())];

    // Find the start of the current word
    let word_start = before_cursor
        .rfind(|c: char| !c.is_alphanumeric() && c != '_')
        .map(|i| i + 1)
        .unwrap_or(0);

    let current_word = &before_cursor[word_start..];

    if current_word.is_empty() {
        return Vec::new();
    }

    let word_upper = current_word.to_uppercase();
    let mut suggestions = Vec::new();

    // Check context - are we after FROM, JOIN, INTO, UPDATE?
    let context_before = before_cursor[..word_start].to_uppercase();
    let table_context = context_before.ends_with("FROM ")
        || context_before.ends_with("JOIN ")
        || context_before.ends_with("INTO ")
        || context_before.ends_with("UPDATE ")
        || context_before.ends_with("TABLE ");

    // Check if we're in SELECT clause (before FROM)
    let in_select_clause = context_before.contains("SELECT") && !context_before.contains("FROM");

    // Check if we're after WHERE, AND, OR, SET
    let in_condition_clause = context_before.ends_with("WHERE ")
        || context_before.ends_with("AND ")
        || context_before.ends_with("OR ")
        || context_before.ends_with("SET ")
        || context_before.ends_with("ON ")
        || context_before.ends_with("BY ");

    if table_context {
        // Suggest tables
        for table in known_tables {
            if table.to_uppercase().starts_with(&word_upper) {
                suggestions.push(table.clone());
            }
        }
    } else if in_select_clause || in_condition_clause {
        // Prioritize columns in SELECT and WHERE clauses
        for col in known_columns {
            if col.to_uppercase().starts_with(&word_upper) {
                suggestions.push(col.clone());
            }
        }

        // Then add functions (especially aggregate functions in SELECT)
        if in_select_clause {
            for func in SQL_FUNCTIONS {
                if func.starts_with(&word_upper) {
                    suggestions.push(format!("{}()", func));
                }
            }
        }

        // Then keywords
        for kw in SQL_KEYWORDS {
            if kw.starts_with(&word_upper) {
                suggestions.push(kw.to_string());
            }
        }
    } else {
        // Default behavior: suggest columns first
        for col in known_columns {
            if col.to_uppercase().starts_with(&word_upper) {
                suggestions.push(col.clone());
            }
        }

        // Then keywords
        for kw in SQL_KEYWORDS {
            if kw.starts_with(&word_upper) {
                suggestions.push(kw.to_string());
            }
        }

        // Then functions
        for func in SQL_FUNCTIONS {
            if func.starts_with(&word_upper) {
                suggestions.push(format!("{}()", func));
            }
        }
    }

    // Remove duplicates while preserving order (DB fields first)
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
