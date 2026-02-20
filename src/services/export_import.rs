use std::fs;
use std::path::PathBuf;

use crate::models::{Column, QueryResult};

/// Export format options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    SqlInsert,
}

impl ExportFormat {
    pub fn extension(&self) -> &str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::SqlInsert => "sql",
        }
    }

    pub fn label(&self) -> &str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::SqlInsert => "SQL INSERT",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ExportFormat::Csv => ExportFormat::SqlInsert,
            ExportFormat::SqlInsert => ExportFormat::Csv,
        }
    }
}

/// State for the export dialog
#[derive(Debug, Clone)]
pub struct ExportState {
    pub format: ExportFormat,
    pub file_path: String,
    pub cursor_position: usize,
    pub table_name: Option<String>,
    /// 0 = format, 1 = file path
    pub active_field: usize,
}

impl ExportState {
    pub fn new(table_name: Option<String>, result: &QueryResult) -> Self {
        let default_name = table_name
            .as_deref()
            .unwrap_or("export")
            // Remove quotes and schema prefix for file name
            .replace('"', "")
            .replace('`', "")
            .replace('[', "")
            .replace(']', "");
        // Use last part after dot (table name without schema)
        let clean_name = default_name
            .split('.')
            .last()
            .unwrap_or(&default_name)
            .to_string();

        let path = format!("{}.csv", clean_name);
        let cursor = path.len();

        Self {
            format: ExportFormat::Csv,
            file_path: path,
            cursor_position: cursor,
            table_name,
            active_field: 1, // Start on file path
        }
    }

    /// Update file extension when format changes
    pub fn update_extension(&mut self) {
        if let Some(dot_pos) = self.file_path.rfind('.') {
            self.file_path = format!("{}.{}", &self.file_path[..dot_pos], self.format.extension());
        } else {
            self.file_path = format!("{}.{}", self.file_path, self.format.extension());
        }
        self.cursor_position = self.file_path.len();
    }
}

/// State for the import dialog
#[derive(Debug, Clone)]
pub struct ImportState {
    pub file_path: String,
    pub cursor_position: usize,
    pub target_table: String,
    /// 0 = file path, 1 = target table
    pub active_field: usize,
}

impl ImportState {
    pub fn new(table_name: Option<String>) -> Self {
        let table = table_name.unwrap_or_default();
        Self {
            file_path: String::new(),
            cursor_position: 0,
            target_table: table,
            active_field: 0,
        }
    }
}

/// Escape a CSV field value (RFC 4180)
fn escape_csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Check if a SQL type is numeric (should not be quoted in SQL INSERT)
fn is_numeric_type(type_name: &str) -> bool {
    let type_lower = type_name.to_lowercase();
    type_lower.contains("int")
        || type_lower.contains("serial")
        || type_lower.contains("float")
        || type_lower.contains("double")
        || type_lower.contains("decimal")
        || type_lower.contains("numeric")
        || type_lower.contains("real")
        || type_lower.contains("money")
        || type_lower.contains("number")
        || type_lower == "bit"
}

/// Export query results to CSV format
pub fn export_to_csv(result: &QueryResult) -> String {
    let mut output = String::new();

    // Header row
    let headers: Vec<String> = result
        .columns
        .iter()
        .map(|c| escape_csv_field(&c.name))
        .collect();
    output.push_str(&headers.join(","));
    output.push('\n');

    // Data rows
    for row in &result.rows {
        let fields: Vec<String> = row.iter().map(|v| escape_csv_field(v)).collect();
        output.push_str(&fields.join(","));
        output.push('\n');
    }

    output
}

/// Export query results to SQL INSERT statements
pub fn export_to_sql_insert(
    result: &QueryResult,
    table_name: &str,
    quote_start: char,
    quote_end: char,
) -> String {
    let mut output = String::new();

    if result.rows.is_empty() {
        output.push_str("-- No data to export\n");
        return output;
    }

    // Column names
    let col_names: Vec<String> = result
        .columns
        .iter()
        .map(|c| format!("{}{}{}", quote_start, c.name, quote_end))
        .collect();
    let columns_str = col_names.join(", ");

    // Generate INSERT for each row
    for row in &result.rows {
        let values: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(idx, val)| {
                if val == "NULL" || val.is_empty() {
                    "NULL".to_string()
                } else {
                    let escaped = val.replace('\'', "''");
                    if idx < result.columns.len() && is_numeric_type(&result.columns[idx].type_name)
                    {
                        escaped
                    } else {
                        format!("'{}'", escaped)
                    }
                }
            })
            .collect();

        output.push_str(&format!(
            "INSERT INTO {} ({}) VALUES ({});\n",
            table_name,
            columns_str,
            values.join(", ")
        ));
    }

    output
}

/// Export query results to file
pub fn export_to_file(
    result: &QueryResult,
    format: ExportFormat,
    file_path: &str,
    table_name: &str,
    quote_start: char,
    quote_end: char,
) -> Result<usize, String> {
    let content = match format {
        ExportFormat::Csv => export_to_csv(result),
        ExportFormat::SqlInsert => export_to_sql_insert(result, table_name, quote_start, quote_end),
    };

    let row_count = result.rows.len();

    // Resolve path (support relative paths)
    let path = PathBuf::from(file_path);

    fs::write(&path, &content).map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(row_count)
}

/// Parse a CSV file and return column names and rows
pub fn parse_csv(content: &str) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let mut lines = content.lines();

    // Parse header
    let header_line = lines.next().ok_or("CSV file is empty")?;
    let columns = parse_csv_line(header_line);

    if columns.is_empty() {
        return Err("No columns found in CSV header".to_string());
    }

    // Parse data rows
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (line_num, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields = parse_csv_line(line);
        if fields.len() != columns.len() {
            return Err(format!(
                "Row {} has {} fields, expected {} (columns)",
                line_num + 2,
                fields.len(),
                columns.len()
            ));
        }
        rows.push(fields);
    }

    Ok((columns, rows))
}

/// Parse a single CSV line respecting quoted fields
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    // Escaped quote
                    current.push('"');
                    chars.next();
                } else {
                    // End of quoted field
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else {
            match c {
                '"' => {
                    in_quotes = true;
                }
                ',' => {
                    fields.push(current.clone());
                    current.clear();
                }
                _ => {
                    current.push(c);
                }
            }
        }
    }

    fields.push(current);
    fields
}

/// Build INSERT queries from CSV data
pub fn build_import_queries(
    table_name: &str,
    columns: &[String],
    rows: &[Vec<String>],
    quote_start: char,
    quote_end: char,
) -> Vec<String> {
    let col_names: Vec<String> = columns
        .iter()
        .map(|c| format!("{}{}{}", quote_start, c, quote_end))
        .collect();
    let columns_str = col_names.join(", ");

    rows.iter()
        .map(|row| {
            let values: Vec<String> = row
                .iter()
                .map(|val| {
                    if val == "NULL" || val.is_empty() {
                        "NULL".to_string()
                    } else {
                        let escaped = val.replace('\'', "''");
                        format!("'{}'", escaped)
                    }
                })
                .collect();

            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                table_name,
                columns_str,
                values.join(", ")
            )
        })
        .collect()
}
