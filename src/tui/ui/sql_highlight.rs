use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub use crate::engine::sql::lexer::*;

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
