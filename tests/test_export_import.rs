use storing_unicorns::models::{Column, QueryResult, SchemaInfo};
use storing_unicorns::services::export_import::*;

// ========== ExportFormat Tests ==========

#[test]
fn export_format_extension() {
    assert_eq!(ExportFormat::Csv.extension(), "csv");
    assert_eq!(ExportFormat::SqlInsert.extension(), "sql");
}

#[test]
fn export_format_label() {
    assert_eq!(ExportFormat::Csv.label(), "CSV");
    assert_eq!(ExportFormat::SqlInsert.label(), "SQL INSERT");
}

#[test]
fn export_format_next() {
    assert_eq!(ExportFormat::Csv.next(), ExportFormat::SqlInsert);
    assert_eq!(ExportFormat::SqlInsert.next(), ExportFormat::Csv);
}

// ========== ExportState Tests ==========

fn make_query_result() -> QueryResult {
    QueryResult {
        columns: vec![
            Column {
                name: "id".into(),
                type_name: "integer".into(),
                nullable: false,
                is_primary_key: true,
            },
            Column {
                name: "name".into(),
                type_name: "varchar".into(),
                nullable: true,
                is_primary_key: false,
            },
        ],
        rows: vec![
            vec!["1".into(), "Alice".into()],
            vec!["2".into(), "Bob".into()],
        ],
        rows_affected: 2,
        execution_time_ms: 5,
    }
}

#[test]
fn export_state_new_default_filename() {
    let result = make_query_result();
    let state = ExportState::new(Some("\"public\".\"users\"".into()), &result);
    assert_eq!(state.file_path, "users.csv");
    assert_eq!(state.format, ExportFormat::Csv);
    assert_eq!(state.active_field, 1);
}

#[test]
fn export_state_new_no_table_name() {
    let result = make_query_result();
    let state = ExportState::new(None, &result);
    assert_eq!(state.file_path, "export.csv");
}

#[test]
fn export_state_new_cleans_brackets() {
    let result = make_query_result();
    let state = ExportState::new(Some("[dbo].[users]".into()), &result);
    assert_eq!(state.file_path, "users.csv");
}

#[test]
fn export_state_update_extension() {
    let result = make_query_result();
    let mut state = ExportState::new(Some("users".into()), &result);
    assert_eq!(state.file_path, "users.csv");

    state.format = ExportFormat::SqlInsert;
    state.update_extension();
    assert_eq!(state.file_path, "users.sql");

    state.format = ExportFormat::Csv;
    state.update_extension();
    assert_eq!(state.file_path, "users.csv");
}

// ========== ImportState Tests ==========

#[test]
fn import_state_new_with_table() {
    let state = ImportState::new(Some("my_table".into()));
    assert_eq!(state.target_table, "my_table");
    assert!(state.file_path.is_empty());
    assert_eq!(state.active_field, 0);
    assert!(state.import_progress.is_none());
}

#[test]
fn import_state_new_without_table() {
    let state = ImportState::new(None);
    assert!(state.target_table.is_empty());
}

// ========== PathCompletion Tests ==========

#[test]
fn path_completion_new() {
    let pc = PathCompletion::new(false);
    assert!(pc.suggestions.is_empty());
    assert_eq!(pc.selected_index, 0);
    assert!(!pc.active);
    assert!(!pc.dirs_only);
}

#[test]
fn path_completion_new_dirs_only() {
    let pc = PathCompletion::new(true);
    assert!(pc.dirs_only);
}

#[test]
fn path_completion_next_wraps() {
    let mut pc = PathCompletion::new(false);
    pc.suggestions = vec!["a".into(), "b".into(), "c".into()];
    assert_eq!(pc.selected_index, 0);

    pc.next();
    assert_eq!(pc.selected_index, 1);
    pc.next();
    assert_eq!(pc.selected_index, 2);
    pc.next();
    assert_eq!(pc.selected_index, 0); // wrap
}

#[test]
fn path_completion_prev_wraps() {
    let mut pc = PathCompletion::new(false);
    pc.suggestions = vec!["a".into(), "b".into(), "c".into()];
    assert_eq!(pc.selected_index, 0);

    pc.prev();
    assert_eq!(pc.selected_index, 2); // wrap backward
    pc.prev();
    assert_eq!(pc.selected_index, 1);
}

#[test]
fn path_completion_current_suggestion() {
    let mut pc = PathCompletion::new(false);
    pc.suggestions = vec!["first".into(), "second".into()];

    // Not active => None
    assert!(pc.current_suggestion().is_none());

    pc.active = true;
    assert_eq!(pc.current_suggestion(), Some("first"));

    pc.next();
    assert_eq!(pc.current_suggestion(), Some("second"));
}

#[test]
fn path_completion_apply() {
    let mut pc = PathCompletion::new(false);
    pc.suggestions = vec!["test.csv".into()];
    pc.active = true;

    let result = pc.apply();
    assert_eq!(result, Some("test.csv".into()));
    assert!(!pc.active);
    assert!(pc.suggestions.is_empty());
}

#[test]
fn path_completion_apply_empty() {
    let mut pc = PathCompletion::new(false);
    pc.active = false;
    assert!(pc.apply().is_none());
}

#[test]
fn path_completion_dismiss() {
    let mut pc = PathCompletion::new(false);
    pc.suggestions = vec!["a".into()];
    pc.active = true;
    pc.selected_index = 1;

    pc.dismiss();
    assert!(!pc.active);
    assert!(pc.suggestions.is_empty());
    assert_eq!(pc.selected_index, 0);
}

// ========== BatchExportState Tests ==========

fn make_schemas() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            name: "public".into(),
            tables: vec!["users".into(), "orders".into()],
            expanded: true,
        },
        SchemaInfo {
            name: "admin".into(),
            tables: vec!["logs".into()],
            expanded: false,
        },
    ]
}

#[test]
fn batch_export_state_new() {
    let schemas = make_schemas();
    let state = BatchExportState::new(&schemas);
    assert_eq!(state.tables.len(), 3);
    assert_eq!(state.tables[0], ("public".into(), "users".into(), false));
    assert_eq!(state.tables[1], ("public".into(), "orders".into(), false));
    assert_eq!(state.tables[2], ("admin".into(), "logs".into(), false));
    assert_eq!(state.active_field, 2);
    assert_eq!(state.format, ExportFormat::Csv);
}

#[test]
fn batch_export_state_toggle_selected() {
    let schemas = make_schemas();
    let mut state = BatchExportState::new(&schemas);
    assert!(!state.tables[0].2);

    state.toggle_selected(); // Toggle index 0
    assert!(state.tables[0].2);

    state.toggle_selected();
    assert!(!state.tables[0].2);
}

#[test]
fn batch_export_state_select_all() {
    let schemas = make_schemas();
    let mut state = BatchExportState::new(&schemas);

    state.select_all();
    assert!(state.tables.iter().all(|(_, _, s)| *s));
}

#[test]
fn batch_export_state_deselect_all() {
    let schemas = make_schemas();
    let mut state = BatchExportState::new(&schemas);
    state.select_all();
    state.deselect_all();
    assert!(state.tables.iter().all(|(_, _, s)| !*s));
}

#[test]
fn batch_export_state_get_selected_tables() {
    let schemas = make_schemas();
    let mut state = BatchExportState::new(&schemas);
    state.tables[0].2 = true; // Select "users"
    state.tables[2].2 = true; // Select "logs"

    let selected = state.get_selected_tables();
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0], ("public".into(), "users".into()));
    assert_eq!(selected[1], ("admin".into(), "logs".into()));
}

#[test]
fn batch_export_state_clean_table_name() {
    assert_eq!(BatchExportState::clean_table_name("\"users\""), "users");
    assert_eq!(BatchExportState::clean_table_name("[dbo]"), "dbo");
    assert_eq!(BatchExportState::clean_table_name("`orders`"), "orders");
    assert_eq!(BatchExportState::clean_table_name("plain"), "plain");
}

// ========== BatchImportState Tests ==========

#[test]
fn batch_import_state_new() {
    let schemas = make_schemas();
    let state = BatchImportState::new(&schemas);
    assert_eq!(state.tables.len(), 3);
    assert_eq!(state.active_field, 1);
    assert_eq!(state.directory, ".");
}

#[test]
fn batch_import_state_toggle_select_deselect() {
    let schemas = make_schemas();
    let mut state = BatchImportState::new(&schemas);

    state.toggle_selected();
    assert!(state.tables[0].2);

    state.select_all();
    assert!(state.tables.iter().all(|(_, _, s)| *s));

    state.deselect_all();
    assert!(state.tables.iter().all(|(_, _, s)| !*s));
}

#[test]
fn batch_import_state_get_selected_tables() {
    let schemas = make_schemas();
    let mut state = BatchImportState::new(&schemas);
    state.tables[1].2 = true;

    let selected = state.get_selected_tables();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0], ("public".into(), "orders".into()));
}

// ========== BatchTruncateState Tests ==========

#[test]
fn batch_truncate_state_new() {
    let schemas = make_schemas();
    let state = BatchTruncateState::new(&schemas);
    assert_eq!(state.tables.len(), 3);
    assert_eq!(state.selected_index, 0);
}

#[test]
fn batch_truncate_state_toggle_select_all_deselect() {
    let schemas = make_schemas();
    let mut state = BatchTruncateState::new(&schemas);

    state.toggle_selected();
    assert!(state.tables[0].2);

    state.select_all();
    assert!(state.tables.iter().all(|(_, _, s)| *s));

    state.deselect_all();
    assert!(state.tables.iter().all(|(_, _, s)| !*s));
}

#[test]
fn batch_truncate_state_get_selected_tables() {
    let schemas = make_schemas();
    let mut state = BatchTruncateState::new(&schemas);
    state.tables[0].2 = true;
    state.tables[2].2 = true;

    let selected = state.get_selected_tables('"', '"');
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0], "\"public\".\"users\"");
    assert_eq!(selected[1], "\"admin\".\"logs\"");
}

#[test]
fn batch_truncate_state_get_selected_tables_brackets() {
    let schemas = make_schemas();
    let mut state = BatchTruncateState::new(&schemas);
    state.tables[0].2 = true;

    let selected = state.get_selected_tables('[', ']');
    assert_eq!(selected[0], "[public].[users]");
}

// ========== CSV Export/Import ==========

#[test]
fn export_to_csv_basic() {
    let result = make_query_result();
    let csv = export_to_csv(&result);

    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines.len(), 3); // header + 2 rows
    assert_eq!(lines[0], "id,name");
    assert_eq!(lines[1], "1,Alice");
    assert_eq!(lines[2], "2,Bob");
}

#[test]
fn export_to_csv_with_special_chars() {
    let result = QueryResult {
        columns: vec![Column {
            name: "data".into(),
            type_name: "text".into(),
            nullable: true,
            is_primary_key: false,
        }],
        rows: vec![
            vec!["hello, world".into()],
            vec!["has \"quotes\"".into()],
            vec!["normal".into()],
        ],
        rows_affected: 3,
        execution_time_ms: 1,
    };
    let csv = export_to_csv(&result);
    let lines: Vec<&str> = csv.trim().lines().collect();
    assert_eq!(lines[1], "\"hello, world\"");
    assert_eq!(lines[2], "\"has \"\"quotes\"\"\"");
    assert_eq!(lines[3], "normal");
}

#[test]
fn export_to_sql_insert_basic() {
    let result = make_query_result();
    let sql = export_to_sql_insert(&result, "users", '"', '"');

    assert!(sql.contains("INSERT INTO users"));
    assert!(sql.contains("\"id\", \"name\""));
    assert!(sql.contains("1, 'Alice'"));
    // id is integer type → should not be quoted
    let lines: Vec<&str> = sql.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn export_to_sql_insert_empty() {
    let result = QueryResult::default();
    let sql = export_to_sql_insert(&result, "empty_table", '"', '"');
    assert!(sql.contains("No data to export"));
}

#[test]
fn export_to_sql_insert_null_handling() {
    let result = QueryResult {
        columns: vec![
            Column {
                name: "id".into(),
                type_name: "int".into(),
                nullable: false,
                is_primary_key: true,
            },
            Column {
                name: "val".into(),
                type_name: "text".into(),
                nullable: true,
                is_primary_key: false,
            },
        ],
        rows: vec![vec!["1".into(), "NULL".into()], vec!["2".into(), "".into()]],
        rows_affected: 2,
        execution_time_ms: 1,
    };
    let sql = export_to_sql_insert(&result, "tbl", '"', '"');
    // Both NULL and empty should become NULL
    assert!(sql.contains("NULL"));
}

// ========== CSV Parsing ==========

#[test]
fn parse_csv_basic() {
    let csv = "id,name,age\n1,Alice,30\n2,Bob,25\n";
    let (cols, rows) = parse_csv(csv).unwrap();
    assert_eq!(cols, vec!["id", "name", "age"]);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], vec!["1", "Alice", "30"]);
    assert_eq!(rows[1], vec!["2", "Bob", "25"]);
}

#[test]
fn parse_csv_quoted_fields() {
    let csv = "name,description\nAlice,\"hello, world\"\nBob,\"has \"\"quotes\"\"\"\n";
    let (cols, rows) = parse_csv(csv).unwrap();
    assert_eq!(cols, vec!["name", "description"]);
    assert_eq!(rows[0][1], "hello, world");
    assert_eq!(rows[1][1], "has \"quotes\"");
}

#[test]
fn parse_csv_empty() {
    let result = parse_csv("");
    assert!(result.is_err());
}

#[test]
fn parse_csv_header_only() {
    let csv = "col1,col2\n";
    let (cols, rows) = parse_csv(csv).unwrap();
    assert_eq!(cols, vec!["col1", "col2"]);
    assert!(rows.is_empty());
}

#[test]
fn parse_csv_mismatched_columns() {
    let csv = "a,b\n1,2,3\n";
    let result = parse_csv(csv);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("3 fields, expected 2"));
}

// ========== find_id_column ==========

#[test]
fn find_id_column_present() {
    let cols = vec!["name".into(), "id".into(), "age".into()];
    assert_eq!(find_id_column(&cols), Some(1));
}

#[test]
fn find_id_column_case_insensitive() {
    let cols = vec!["ID".into(), "name".into()];
    assert_eq!(find_id_column(&cols), Some(0));
}

#[test]
fn find_id_column_absent() {
    let cols = vec!["name".into(), "age".into()];
    assert_eq!(find_id_column(&cols), None);
}

// ========== build_upsert_import_actions ==========

#[test]
fn build_upsert_actions_with_id_col() {
    let columns = vec!["id".into(), "name".into(), "value".into()];
    let rows = vec![
        vec!["1".into(), "Alice".into(), "100".into()],
        vec!["".into(), "Bob".into(), "200".into()],
    ];

    let actions = build_upsert_import_actions("users", &columns, &rows, '"', '"');
    assert_eq!(actions.len(), 2);

    // First row has ID => Upsert
    match &actions[0] {
        ImportAction::Upsert {
            update_query,
            insert_query,
        } => {
            assert!(update_query.contains("UPDATE"));
            assert!(update_query.contains("WHERE \"id\" = '1'"));
            assert!(insert_query.contains("INSERT INTO"));
            // Insert without id column
            assert!(!insert_query.contains("\"id\""));
        }
        _ => panic!("Expected Upsert"),
    }

    // Second row has no ID => InsertOnly
    match &actions[1] {
        ImportAction::InsertOnly { query } => {
            assert!(query.contains("INSERT INTO"));
            assert!(!query.contains("\"id\"")); // No id column
        }
        _ => panic!("Expected InsertOnly"),
    }
}

#[test]
fn build_upsert_actions_without_id_col() {
    let columns = vec!["name".into(), "value".into()];
    let rows = vec![vec!["Alice".into(), "100".into()]];

    let actions = build_upsert_import_actions("data", &columns, &rows, '"', '"');
    assert_eq!(actions.len(), 1);

    match &actions[0] {
        ImportAction::InsertOnly { query } => {
            assert!(query.contains("INSERT INTO data"));
            assert!(query.contains("\"name\""));
            assert!(query.contains("'Alice'"));
        }
        _ => panic!("Expected InsertOnly"),
    }
}

#[test]
fn build_upsert_actions_null_id() {
    let columns = vec!["id".into(), "name".into()];
    let rows = vec![vec!["NULL".into(), "Charlie".into()]];

    let actions = build_upsert_import_actions("tbl", &columns, &rows, '[', ']');
    assert_eq!(actions.len(), 1);

    match &actions[0] {
        ImportAction::InsertOnly { query } => {
            assert!(query.contains("INSERT INTO tbl"));
            assert!(query.contains("[name]"));
            assert!(!query.contains("[id]"));
        }
        _ => panic!("Expected InsertOnly for NULL id"),
    }
}
