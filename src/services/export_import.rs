use std::fs;
use std::path::{Path, PathBuf};

use crate::models::QueryResult;

/// State for filesystem path autocompletion
#[derive(Debug, Clone)]
pub struct PathCompletion {
    /// The list of matching suggestions
    pub suggestions: Vec<String>,
    /// Currently selected suggestion index
    pub selected_index: usize,
    /// Whether suggestions are currently visible
    pub active: bool,
    /// Whether we're completing directories only (true) or files too (false)
    pub dirs_only: bool,
}

impl PathCompletion {
    pub fn new(dirs_only: bool) -> Self {
        Self {
            suggestions: Vec::new(),
            selected_index: 0,
            active: false,
            dirs_only,
        }
    }

    /// Compute suggestions based on the current input text.
    /// Splits the input into a directory part and a prefix, then lists
    /// matching entries from the filesystem.
    pub fn update_suggestions(&mut self, input: &str) {
        self.suggestions.clear();
        self.selected_index = 0;

        if input.is_empty() {
            // List current directory
            self.list_dir(".", "", input);
            return;
        }

        let path = Path::new(input);

        // If input ends with a separator, list contents of that directory
        if input.ends_with('/') || input.ends_with('\\') {
            self.list_dir(input, "", input);
            return;
        }

        // Otherwise: parent = directory to list, file_name = prefix to filter
        if let Some(parent) = path.parent() {
            let dir = if parent.as_os_str().is_empty() {
                "."
            } else {
                parent.to_str().unwrap_or(".")
            };
            let prefix = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            self.list_dir(dir, prefix, input);
        } else {
            self.list_dir(".", input, input);
        }
    }

    /// List filesystem entries in `dir` that start with `prefix`.
    /// `base_input` is the original user input, used to build the full suggestion path.
    fn list_dir(&mut self, dir: &str, prefix: &str, base_input: &str) {
        let dir_path = Path::new(dir);
        let entries = match fs::read_dir(dir_path) {
            Ok(e) => e,
            Err(_) => return,
        };

        let prefix_lower = prefix.to_lowercase();

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = match file_name.to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Skip hidden files/dirs
            if name.starts_with('.') && !prefix.starts_with('.') {
                continue;
            }

            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

            // If dirs_only mode, skip files
            if self.dirs_only && !is_dir {
                continue;
            }

            if !prefix.is_empty() && !name.to_lowercase().starts_with(&prefix_lower) {
                continue;
            }

            // Build the full suggestion path
            let suggestion = if base_input.ends_with('/') || base_input.ends_with('\\') {
                if is_dir {
                    format!("{}{}/", base_input, name)
                } else {
                    format!("{}{}", base_input, name)
                }
            } else if let Some(last_sep) = base_input.rfind(|c: char| c == '/' || c == '\\') {
                let base = &base_input[..=last_sep];
                if is_dir {
                    format!("{}{}/", base, name)
                } else {
                    format!("{}{}", base, name)
                }
            } else if is_dir {
                format!("{}/", name)
            } else {
                name.clone()
            };

            self.suggestions.push(suggestion);
        }

        self.suggestions.sort();
    }

    /// Get the currently selected suggestion, if any.
    pub fn current_suggestion(&self) -> Option<&str> {
        if self.active && !self.suggestions.is_empty() {
            Some(&self.suggestions[self.selected_index])
        } else {
            None
        }
    }

    /// Move to the next suggestion (wrapping around).
    pub fn next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.suggestions.len();
        }
    }

    /// Move to the previous suggestion (wrapping around).
    #[allow(dead_code)]
    pub fn prev(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.suggestions.len() - 1
            } else {
                self.selected_index - 1
            };
        }
    }

    /// Apply the currently selected suggestion: returns the new path string.
    pub fn apply(&mut self) -> Option<String> {
        if let Some(s) = self.current_suggestion() {
            let result = s.to_string();
            self.active = false;
            self.suggestions.clear();
            Some(result)
        } else {
            None
        }
    }

    /// Dismiss suggestions.
    pub fn dismiss(&mut self) {
        self.active = false;
        self.suggestions.clear();
        self.selected_index = 0;
    }
}

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
    pub path_completion: PathCompletion,
}

impl ExportState {
    pub fn new(table_name: Option<String>, _result: &QueryResult) -> Self {
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
            path_completion: PathCompletion::new(false),
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
    /// Import progress: (current_row, total_rows)
    pub import_progress: Option<(usize, usize)>,
    pub path_completion: PathCompletion,
}

impl ImportState {
    pub fn new(table_name: Option<String>) -> Self {
        let table = table_name.unwrap_or_default();
        Self {
            file_path: String::new(),
            cursor_position: 0,
            target_table: table,
            active_field: 0,
            import_progress: None,
            path_completion: PathCompletion::new(false),
        }
    }
}

/// State for batch export dialog
#[derive(Debug, Clone)]
pub struct BatchExportState {
    pub format: ExportFormat,
    pub directory: String,
    pub cursor_position: usize,
    /// List of (schema_name, table_name, selected)
    pub tables: Vec<(String, String, bool)>,
    /// Currently highlighted table in the list
    pub selected_index: usize,
    /// Scroll offset for the table list
    pub scroll_offset: usize,
    /// 0 = format, 1 = directory, 2 = table list
    pub active_field: usize,
    /// Progress: (current_table_index, total_tables, current_table_name)
    pub progress: Option<(usize, usize, String)>,
    pub path_completion: PathCompletion,
}

impl BatchExportState {
    pub fn new(schemas: &[crate::models::SchemaInfo]) -> Self {
        let mut tables = Vec::new();
        for schema in schemas {
            for table in &schema.tables {
                tables.push((schema.name.clone(), table.clone(), false));
            }
        }
        Self {
            format: ExportFormat::Csv,
            directory: String::from("."),
            cursor_position: 1,
            tables,
            selected_index: 0,
            scroll_offset: 0,
            active_field: 2, // Start on table list
            progress: None,
            path_completion: PathCompletion::new(true),
        }
    }

    /// Toggle selection of the currently highlighted table
    pub fn toggle_selected(&mut self) {
        if let Some(entry) = self.tables.get_mut(self.selected_index) {
            entry.2 = !entry.2;
        }
    }

    /// Select all tables
    pub fn select_all(&mut self) {
        for entry in &mut self.tables {
            entry.2 = true;
        }
    }

    /// Deselect all tables
    pub fn deselect_all(&mut self) {
        for entry in &mut self.tables {
            entry.2 = false;
        }
    }

    /// Get selected table names with schema prefix
    pub fn get_selected_tables(&self) -> Vec<(String, String)> {
        self.tables
            .iter()
            .filter(|(_, _, selected)| *selected)
            .map(|(schema, table, _)| (schema.clone(), table.clone()))
            .collect()
    }

    /// Clean table name for file path (remove quotes, special chars)
    pub fn clean_table_name(name: &str) -> String {
        name.replace('"', "")
            .replace('`', "")
            .replace('[', "")
            .replace(']', "")
    }
}

/// State for batch import dialog
#[derive(Debug, Clone)]
pub struct BatchImportState {
    pub directory: String,
    pub cursor_position: usize,
    /// List of (schema_name, table_name, selected)
    pub tables: Vec<(String, String, bool)>,
    /// Currently highlighted table in the list
    pub selected_index: usize,
    /// Scroll offset for the table list
    pub scroll_offset: usize,
    /// 0 = directory, 1 = table list
    pub active_field: usize,
    /// Progress: (current_table_index, total_tables, current_table_name)
    pub progress: Option<(usize, usize, String)>,
    pub path_completion: PathCompletion,
}

impl BatchImportState {
    pub fn new(schemas: &[crate::models::SchemaInfo]) -> Self {
        let mut tables = Vec::new();
        for schema in schemas {
            for table in &schema.tables {
                tables.push((schema.name.clone(), table.clone(), false));
            }
        }
        Self {
            directory: String::from("."),
            cursor_position: 1,
            tables,
            selected_index: 0,
            scroll_offset: 0,
            active_field: 1, // Start on table list
            progress: None,
            path_completion: PathCompletion::new(true),
        }
    }

    /// Toggle selection of the currently highlighted table
    pub fn toggle_selected(&mut self) {
        if let Some(entry) = self.tables.get_mut(self.selected_index) {
            entry.2 = !entry.2;
        }
    }

    /// Select all tables
    pub fn select_all(&mut self) {
        for entry in &mut self.tables {
            entry.2 = true;
        }
    }

    /// Deselect all tables
    pub fn deselect_all(&mut self) {
        for entry in &mut self.tables {
            entry.2 = false;
        }
    }

    /// Get selected table names with schema prefix
    pub fn get_selected_tables(&self) -> Vec<(String, String)> {
        self.tables
            .iter()
            .filter(|(_, _, selected)| *selected)
            .map(|(schema, table, _)| (schema.clone(), table.clone()))
            .collect()
    }

    /// Auto-select tables that have a matching CSV file in the directory.
    pub fn auto_select_matching_files(&mut self) {
        let dir = Path::new(&self.directory);
        for entry in &mut self.tables {
            let clean_name = BatchExportState::clean_table_name(&entry.1);
            let csv_path = dir.join(format!("{}.csv", clean_name));
            entry.2 = csv_path.exists();
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

/// Detect the index of an "id" column in the CSV headers.
/// Looks for a column named exactly "id" (case-insensitive).
pub fn find_id_column(columns: &[String]) -> Option<usize> {
    columns
        .iter()
        .position(|c| c.trim().eq_ignore_ascii_case("id"))
}

/// Escape a value for SQL (single quotes around strings, NULL for empty).
fn escape_sql_value(val: &str) -> String {
    if val == "NULL" || val.is_empty() {
        "NULL".to_string()
    } else {
        let escaped = val.replace('\'', "''");
        format!("'{}'", escaped)
    }
}

/// Represents an import action for a single row when upsert mode is active.
pub enum ImportAction {
    /// Row has an ID → try UPDATE first, then INSERT without ID if no match
    Upsert {
        update_query: String,
        insert_query: String,
    },
    /// Row has no ID (empty/NULL) → just INSERT without the ID column
    InsertOnly { query: String },
}

/// Build upsert import actions from CSV data.
/// When an "id" column is detected:
/// - Rows with an ID value → UPDATE ... WHERE id = X, fallback INSERT without id
/// - Rows without an ID value → INSERT without the id column
/// When no "id" column is found, falls back to plain INSERT for all rows.
pub fn build_upsert_import_actions(
    table_name: &str,
    columns: &[String],
    rows: &[Vec<String>],
    quote_start: char,
    quote_end: char,
) -> Vec<ImportAction> {
    let id_col_idx = match find_id_column(columns) {
        Some(idx) => idx,
        None => {
            // No id column, fall back to plain inserts
            return rows
                .iter()
                .map(|row| {
                    let col_names: Vec<String> = columns
                        .iter()
                        .map(|c| format!("{}{}{}", quote_start, c, quote_end))
                        .collect();
                    let values: Vec<String> = row.iter().map(|v| escape_sql_value(v)).collect();
                    ImportAction::InsertOnly {
                        query: format!(
                            "INSERT INTO {} ({}) VALUES ({})",
                            table_name,
                            col_names.join(", "),
                            values.join(", ")
                        ),
                    }
                })
                .collect();
        }
    };

    let id_col_name = format!("{}{}{}", quote_start, columns[id_col_idx], quote_end);

    // Columns without the id column (for INSERT without id)
    let non_id_columns: Vec<String> = columns
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != id_col_idx)
        .map(|(_, c)| format!("{}{}{}", quote_start, c, quote_end))
        .collect();
    let non_id_columns_str = non_id_columns.join(", ");

    // All columns (for UPDATE SET)
    let all_col_names: Vec<String> = columns
        .iter()
        .map(|c| format!("{}{}{}", quote_start, c, quote_end))
        .collect();

    rows.iter()
        .map(|row| {
            let id_value = &row[id_col_idx];
            let has_id = !id_value.is_empty() && id_value != "NULL";

            if has_id {
                // Build UPDATE query
                let set_clauses: Vec<String> = columns
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != id_col_idx)
                    .map(|(i, _)| format!("{} = {}", all_col_names[i], escape_sql_value(&row[i])))
                    .collect();

                let update_query = format!(
                    "UPDATE {} SET {} WHERE {} = {}",
                    table_name,
                    set_clauses.join(", "),
                    id_col_name,
                    escape_sql_value(id_value)
                );

                // Build INSERT without id (fallback)
                let non_id_values: Vec<String> = row
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != id_col_idx)
                    .map(|(_, v)| escape_sql_value(v))
                    .collect();

                let insert_query = format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    table_name,
                    non_id_columns_str,
                    non_id_values.join(", ")
                );

                ImportAction::Upsert {
                    update_query,
                    insert_query,
                }
            } else {
                // No id value → INSERT without id column
                let non_id_values: Vec<String> = row
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != id_col_idx)
                    .map(|(_, v)| escape_sql_value(v))
                    .collect();

                ImportAction::InsertOnly {
                    query: format!(
                        "INSERT INTO {} ({}) VALUES ({})",
                        table_name,
                        non_id_columns_str,
                        non_id_values.join(", ")
                    ),
                }
            }
        })
        .collect()
}

/// State for batch truncate dialog
#[derive(Debug, Clone)]
pub struct BatchTruncateState {
    /// List of (schema_name, table_name, selected)
    pub tables: Vec<(String, String, bool)>,
    /// Currently highlighted table in the list
    pub selected_index: usize,
    /// Scroll offset for the table list
    pub scroll_offset: usize,
    /// Progress: (current_table_index, total_tables, current_table_name)
    pub progress: Option<(usize, usize, String)>,
}

impl BatchTruncateState {
    pub fn new(schemas: &[crate::models::SchemaInfo]) -> Self {
        let mut tables = Vec::new();
        for schema in schemas {
            for table in &schema.tables {
                tables.push((schema.name.clone(), table.clone(), false));
            }
        }
        Self {
            tables,
            selected_index: 0,
            scroll_offset: 0,
            progress: None,
        }
    }

    /// Toggle selection of the currently highlighted table
    pub fn toggle_selected(&mut self) {
        if let Some(entry) = self.tables.get_mut(self.selected_index) {
            entry.2 = !entry.2;
        }
    }

    /// Select all tables
    pub fn select_all(&mut self) {
        for entry in &mut self.tables {
            entry.2 = true;
        }
    }

    /// Deselect all tables
    pub fn deselect_all(&mut self) {
        for entry in &mut self.tables {
            entry.2 = false;
        }
    }

    /// Get selected table names with schema prefix (quoted)
    pub fn get_selected_tables(&self, quote_start: char, quote_end: char) -> Vec<String> {
        self.tables
            .iter()
            .filter(|(_, _, selected)| *selected)
            .map(|(schema, table, _)| {
                format!("{1}{0}{2}.{1}{3}{2}", schema, quote_start, quote_end, table)
            })
            .collect()
    }
}
