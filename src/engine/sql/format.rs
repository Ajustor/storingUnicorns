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
    sqlformat::format(text, &QueryParams::None, &options)
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
}
