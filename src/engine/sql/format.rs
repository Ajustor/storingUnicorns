//! SQL reformatting for the editor ("Reformat code").

use sqlformat::{FormatOptions, Indent, QueryParams};

/// Reformat SQL: uppercase keywords, 2-space indent, one blank line between
/// statements. Blank input gives an empty string.
pub fn format_sql(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let options = FormatOptions {
        indent: Indent::Spaces(2),
        uppercase: Some(true),
        lines_between_queries: 2,
        ..Default::default()
    };
    let (protected, literals) = protect_prefixed_literals(text);
    let mut out = sqlformat::format(&protected, &QueryParams::None, &options);
    for (i, literal) in literals.iter().enumerate() {
        out = out.replacen(&placeholder(i), literal, 1);
    }
    out
}

fn placeholder(i: usize) -> String {
    format!("__su_literal_{i}__")
}

/// Index just past the quoted text opened at `chars[start]` and closed by
/// `close` (a doubled `close` and, in `'…'`, a backslash escape stay inside,
/// as sqlformat reads them). Unterminated: the end of `chars`.
fn quoted_end(chars: &[char], start: usize, close: char) -> usize {
    let mut i = start + 1;
    while i < chars.len() {
        if close == '\'' && chars[i] == '\\' {
            i += 2;
        } else if chars[i] == close {
            if chars.get(i + 1) == Some(&close) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    chars.len()
}

/// Replace literals with a prefix (`E'…'`, `b'…'`, `x'…'`, `N'…'`, `U&'…'`)
/// by placeholder identifiers: sqlformat splits them into two tokens
/// (`b '101'`, which reads as a column with an alias). Returns the text and
/// the literals, in placeholder order.
fn protect_prefixed_literals(text: &str) -> (String, Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '@' | '#');
    let mut out = String::with_capacity(text.len());
    let mut literals = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        // Strings, quoted identifiers and comments are copied as they are.
        let end = match (c, next) {
            ('\'', _) | ('"', _) | ('`', _) => Some(quoted_end(&chars, i, c)),
            ('[', _) => Some(quoted_end(&chars, i, ']')),
            ('-', Some('-')) => Some(
                (i..chars.len())
                    .find(|&j| chars[j] == '\n')
                    .unwrap_or(chars.len()),
            ),
            ('/', Some('*')) => Some(
                (i + 2..chars.len().saturating_sub(1))
                    .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                    .map_or(chars.len(), |j| j + 2),
            ),
            _ => None,
        };
        if let Some(end) = end {
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        let prefix_len = if i > 0 && is_ident(chars[i - 1]) {
            0
        } else if matches!(c, 'e' | 'E' | 'b' | 'B' | 'x' | 'X' | 'n' | 'N') {
            1
        } else if matches!(c, 'u' | 'U') && next == Some('&') {
            2
        } else {
            0
        };
        if prefix_len > 0 && chars.get(i + prefix_len) == Some(&'\'') {
            let end = quoted_end(&chars, i + prefix_len, '\'');
            out.push_str(&placeholder(literals.len()));
            literals.push(chars[i..end].iter().collect());
            i = end;
            continue;
        }
        out.push(c);
        i += 1;
    }
    (out, literals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_uppercases() {
        let out = format_sql("select a, b from t where a = 1");
        assert!(out.starts_with("SELECT"));
        assert!(out.contains("\nFROM"));
        assert!(out.contains("\nWHERE"));
    }

    #[test]
    fn keeps_string_literals_untouched() {
        assert!(format_sql("select 'from x' as s").contains("'from x'"));
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(format_sql("   "), "");
    }

    #[test]
    fn keeps_prefixed_literals_in_one_piece() {
        let out = format_sql(r"select b'101', x'ff', U&'d\0061t', e'a\'b', n'y', size 'a' from t");
        for literal in [r"b'101'", "x'ff'", r"U&'d\0061t'", r"e'a\'b'", "n'y'"] {
            assert!(out.contains(literal), "{literal} in {out}");
        }
        // A word before a string (an alias) is not a prefix.
        assert!(out.contains("size 'a'"), "{out}");
        assert!(!out.contains("__su_literal"), "{out}");
    }

    #[test]
    fn prefix_letters_inside_strings_and_identifiers_are_left_alone() {
        let sql = "select 'max', \"x'\", [b'], `n'` -- e'\nfrom t";
        let out = format_sql(sql);
        for part in ["'max'", "\"x'\"", "[b']", "`n'`", "-- e'"] {
            assert!(out.contains(part), "{part} in {out}");
        }
    }
}
