use storing_unicorns::config::AppConfig;
use storing_unicorns::models::{
    AzureAuthMethod, Column, ConnectionConfig, DatabaseType, QueryResult, SchemaInfo,
};
use storing_unicorns::services::{ActivePanel, AppState, ConnectionField, DialogMode};

// ========== Helper ==========

fn make_app() -> AppState {
    AppState::new(AppConfig::default(), false, true)
}

fn make_app_with_connections(n: usize) -> AppState {
    let mut config = AppConfig::default();
    for i in 0..n {
        config.connections.push(ConnectionConfig {
            name: format!("conn_{}", i),
            db_type: DatabaseType::Postgres,
            host: Some("localhost".into()),
            port: Some(5432),
            username: Some("user".into()),
            password: None,
            database: format!("db_{}", i),
            azure_auth_method: None,
            tenant_id: None,
        });
    }
    AppState::new(config, false, true)
}

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
            Column {
                name: "age".into(),
                type_name: "integer".into(),
                nullable: true,
                is_primary_key: false,
            },
        ],
        rows: vec![
            vec!["1".into(), "Alice".into(), "30".into()],
            vec!["2".into(), "Bob".into(), "25".into()],
            vec!["3".into(), "Charlie".into(), "35".into()],
        ],
        rows_affected: 3,
        execution_time_ms: 10,
    }
}

fn make_schemas() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            name: "public".into(),
            tables: vec!["users".into(), "orders".into(), "products".into()],
            expanded: true,
        },
        SchemaInfo {
            name: "admin".into(),
            tables: vec!["logs".into(), "settings".into()],
            expanded: false,
        },
    ]
}

// ========== ConnectionField Tests ==========

#[test]
fn connection_field_next_full_cycle() {
    let fields = [
        ConnectionField::Name,
        ConnectionField::DbType,
        ConnectionField::AzureAuth,
        ConnectionField::TenantId,
        ConnectionField::Host,
        ConnectionField::Port,
        ConnectionField::Username,
        ConnectionField::Password,
        ConnectionField::Database,
    ];
    // next() should cycle through all fields in order
    let mut f = ConnectionField::Name;
    for expected in &fields[1..] {
        f = f.next();
        assert_eq!(f, *expected);
    }
    // Back to Name
    f = f.next();
    assert_eq!(f, ConnectionField::Name);
}

#[test]
fn connection_field_prev_full_cycle() {
    let mut f = ConnectionField::Name;
    f = f.prev();
    assert_eq!(f, ConnectionField::Database);
    f = f.prev();
    assert_eq!(f, ConnectionField::Password);
}

#[test]
fn connection_field_next_for_non_azure_skips_azure_fields() {
    let db = DatabaseType::Postgres;
    let auth = AzureAuthMethod::Credentials;

    let f = ConnectionField::DbType.next_for(&db, &auth);
    // Should skip AzureAuth and TenantId, go to Host
    assert_eq!(f, ConnectionField::Host);
}

#[test]
fn connection_field_prev_for_non_azure_skips_azure_fields() {
    let db = DatabaseType::MySQL;
    let auth = AzureAuthMethod::Credentials;

    let f = ConnectionField::Host.prev_for(&db, &auth);
    // Should skip TenantId and AzureAuth, go to DbType
    assert_eq!(f, ConnectionField::DbType);
}

#[test]
fn connection_field_next_for_azure_includes_azure_auth() {
    let db = DatabaseType::Azure;
    let auth = AzureAuthMethod::Credentials;

    let f = ConnectionField::DbType.next_for(&db, &auth);
    assert_eq!(f, ConnectionField::AzureAuth);
}

#[test]
fn connection_field_next_for_azure_interactive_includes_tenant_id() {
    let db = DatabaseType::Azure;
    let auth = AzureAuthMethod::Interactive;

    let f = ConnectionField::AzureAuth.next_for(&db, &auth);
    assert_eq!(f, ConnectionField::TenantId);
}

#[test]
fn connection_field_next_for_azure_credentials_skips_tenant_id() {
    let db = DatabaseType::Azure;
    let auth = AzureAuthMethod::Credentials;

    let f = ConnectionField::AzureAuth.next_for(&db, &auth);
    // Should skip TenantId (only for Interactive)
    assert_eq!(f, ConnectionField::Host);
}

// ========== NewConnectionState Tests ==========

#[test]
fn new_connection_state_default_values() {
    let state = storing_unicorns::services::app_state::NewConnectionState::default();
    assert_eq!(state.name, "New Connection");
    assert_eq!(state.db_type, DatabaseType::Postgres);
    assert_eq!(state.host, "localhost");
    assert_eq!(state.port, "5432");
    assert_eq!(state.username, "postgres");
    assert!(state.password.is_empty());
    assert_eq!(state.database, "postgres");
    assert_eq!(state.cursor_position, 14);
}

#[test]
fn new_connection_state_to_config_postgres() {
    let state = storing_unicorns::services::app_state::NewConnectionState::default();
    let config = state.to_config();
    assert_eq!(config.name, "New Connection");
    assert_eq!(config.db_type, DatabaseType::Postgres);
    assert_eq!(config.host, Some("localhost".into()));
    assert_eq!(config.port, Some(5432));
    assert_eq!(config.username, Some("postgres".into()));
    assert!(config.password.is_none()); // empty password => None
    assert!(config.azure_auth_method.is_none());
    assert!(config.tenant_id.is_none());
}

#[test]
fn new_connection_state_to_config_azure_with_tenant() {
    let mut state = storing_unicorns::services::app_state::NewConnectionState::default();
    state.db_type = DatabaseType::Azure;
    state.azure_auth_method = AzureAuthMethod::Interactive;
    state.tenant_id = "my-tenant-id".into();
    state.host = "server.database.windows.net".into();
    state.port = "1433".into();

    let config = state.to_config();
    assert_eq!(config.azure_auth_method, Some(AzureAuthMethod::Interactive));
    assert_eq!(config.tenant_id, Some("my-tenant-id".into()));
}

#[test]
fn new_connection_state_to_config_azure_credentials_no_tenant() {
    let mut state = storing_unicorns::services::app_state::NewConnectionState::default();
    state.db_type = DatabaseType::Azure;
    state.azure_auth_method = AzureAuthMethod::Credentials;
    state.tenant_id = "my-tenant-id".into();

    let config = state.to_config();
    assert_eq!(config.azure_auth_method, Some(AzureAuthMethod::Credentials));
    assert!(config.tenant_id.is_none()); // Not Interactive => no tenant
}

#[test]
fn new_connection_state_cycle_db_type() {
    let mut state = storing_unicorns::services::app_state::NewConnectionState::default();
    assert_eq!(state.db_type, DatabaseType::Postgres);

    state.cycle_db_type();
    assert_eq!(state.db_type, DatabaseType::MySQL);
    assert_eq!(state.port, "3306");

    state.cycle_db_type();
    assert_eq!(state.db_type, DatabaseType::SQLite);
    assert!(state.port.is_empty());

    state.cycle_db_type();
    assert_eq!(state.db_type, DatabaseType::SQLServer);
    assert_eq!(state.port, "1433");

    state.cycle_db_type();
    assert_eq!(state.db_type, DatabaseType::Azure);
    assert_eq!(state.port, "1433");

    state.cycle_db_type();
    assert_eq!(state.db_type, DatabaseType::Postgres);
    assert_eq!(state.port, "5432");
}

#[test]
fn new_connection_state_cycle_azure_auth() {
    let mut state = storing_unicorns::services::app_state::NewConnectionState::default();
    assert_eq!(state.azure_auth_method, AzureAuthMethod::Credentials);

    state.cycle_azure_auth_method();
    assert_eq!(state.azure_auth_method, AzureAuthMethod::Interactive);

    state.cycle_azure_auth_method();
    assert_eq!(state.azure_auth_method, AzureAuthMethod::ManagedIdentity);

    state.cycle_azure_auth_method();
    assert_eq!(state.azure_auth_method, AzureAuthMethod::Credentials);
}

#[test]
fn new_connection_state_get_active_field_value() {
    let mut state = storing_unicorns::services::app_state::NewConnectionState::default();

    state.active_field = ConnectionField::Name;
    assert_eq!(state.get_active_field_value(), "New Connection");

    state.active_field = ConnectionField::Host;
    assert_eq!(state.get_active_field_value(), "localhost");

    state.active_field = ConnectionField::Port;
    assert_eq!(state.get_active_field_value(), "5432");

    state.active_field = ConnectionField::DbType;
    assert_eq!(state.get_active_field_value(), ""); // handled separately

    state.active_field = ConnectionField::TenantId;
    assert_eq!(state.get_active_field_value(), "common");
}

#[test]
fn new_connection_state_get_active_field_mut() {
    let mut state = storing_unicorns::services::app_state::NewConnectionState::default();

    state.active_field = ConnectionField::Name;
    assert!(state.get_active_field_mut().is_some());

    state.active_field = ConnectionField::DbType;
    assert!(state.get_active_field_mut().is_none());

    state.active_field = ConnectionField::AzureAuth;
    assert!(state.get_active_field_mut().is_none());

    state.active_field = ConnectionField::Database;
    if let Some(field) = state.get_active_field_mut() {
        *field = "my_database".into();
    }
    assert_eq!(state.database, "my_database");
}

// ========== AppState Panel Navigation ==========

#[test]
fn app_state_initial_panel() {
    let app = make_app();
    assert_eq!(app.active_panel, ActivePanel::Connections);
}

#[test]
fn app_state_next_panel_without_results() {
    let mut app = make_app();
    assert_eq!(app.active_panel, ActivePanel::Connections);

    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::Tables);

    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::QueryEditor);

    // No results => skip Results, loop back to Connections
    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::Connections);
}

#[test]
fn app_state_next_panel_with_results() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());

    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::Tables);
    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::QueryEditor);
    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::Results);
    app.next_panel();
    assert_eq!(app.active_panel, ActivePanel::Connections);
}

#[test]
fn app_state_prev_panel_without_results() {
    let mut app = make_app();
    assert_eq!(app.active_panel, ActivePanel::Connections);

    // No results => prev from Connections goes to QueryEditor
    app.prev_panel();
    assert_eq!(app.active_panel, ActivePanel::QueryEditor);

    app.prev_panel();
    assert_eq!(app.active_panel, ActivePanel::Tables);

    app.prev_panel();
    assert_eq!(app.active_panel, ActivePanel::Connections);
}

#[test]
fn app_state_prev_panel_with_results() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());

    // Prev from Connections => Results (since results exist)
    app.prev_panel();
    assert_eq!(app.active_panel, ActivePanel::Results);
}

// ========== AppState Selection ==========

#[test]
fn app_state_select_next_connections() {
    let mut app = make_app_with_connections(3);
    app.active_panel = ActivePanel::Connections;
    assert_eq!(app.selected_connection, 0);

    app.select_next();
    assert_eq!(app.selected_connection, 1);
    app.select_next();
    assert_eq!(app.selected_connection, 2);
    // Wrap around
    app.select_next();
    assert_eq!(app.selected_connection, 0);
}

#[test]
fn app_state_select_prev_connections() {
    let mut app = make_app_with_connections(3);
    app.active_panel = ActivePanel::Connections;
    assert_eq!(app.selected_connection, 0);

    // Prev from 0 wraps to last
    app.select_prev();
    assert_eq!(app.selected_connection, 2);
    app.select_prev();
    assert_eq!(app.selected_connection, 1);
}

#[test]
fn app_state_select_next_results() {
    let mut app = make_app();
    app.active_panel = ActivePanel::Results;
    app.query_result = Some(make_query_result());
    assert_eq!(app.selected_row, 0);

    app.select_next();
    assert_eq!(app.selected_row, 1);
    app.select_next();
    assert_eq!(app.selected_row, 2);
    // Wrap
    app.select_next();
    assert_eq!(app.selected_row, 0);
}

#[test]
fn app_state_select_prev_results() {
    let mut app = make_app();
    app.active_panel = ActivePanel::Results;
    app.query_result = Some(make_query_result());

    // Prev from 0 wraps to last row
    app.select_prev();
    assert_eq!(app.selected_row, 2);
}

// ========== AppState Dialog ==========

#[test]
fn app_state_dialog_open_close() {
    let mut app = make_app();
    assert!(!app.is_dialog_open());
    assert_eq!(app.dialog_mode, DialogMode::None);

    app.open_new_connection_dialog();
    assert!(app.is_dialog_open());
    assert_eq!(app.dialog_mode, DialogMode::NewConnection);

    app.close_dialog();
    assert!(!app.is_dialog_open());
    assert_eq!(app.dialog_mode, DialogMode::None);
}

#[test]
fn app_state_open_edit_connection_dialog() {
    let mut app = make_app_with_connections(2);
    app.open_edit_connection_dialog(0);
    assert_eq!(app.dialog_mode, DialogMode::EditConnection);
    assert_eq!(app.editing_connection_index, Some(0));
    assert_eq!(app.new_connection.name, "conn_0");
}

#[test]
fn app_state_open_edit_connection_invalid_index() {
    let mut app = make_app_with_connections(1);
    app.open_edit_connection_dialog(99);
    // Should not open dialog for invalid index
    assert_eq!(app.dialog_mode, DialogMode::None);
}

// ========== Text Selection ==========

#[test]
fn app_state_text_selection_basic() {
    let mut app = make_app();
    app.set_query("SELECT * FROM users".into());

    assert!(!app.has_selection());
    assert!(app.get_selected_text().is_none());

    // Start selection at position 7
    app.set_cursor_position(7);
    app.start_selection();
    assert!(app.has_selection());

    // Extend selection to position 8
    app.extend_selection(8);
    assert_eq!(app.get_selection_range(), Some((7, 8)));
    assert_eq!(app.get_selected_text(), Some("*".into()));
}

#[test]
fn app_state_text_selection_reversed() {
    let mut app = make_app();
    app.set_query("ABCDEFGH".into());

    app.selection_start = Some(5);
    app.selection_end = Some(2);
    // Normalized range should be (2, 5)
    assert_eq!(app.get_selection_range(), Some((2, 5)));
    assert_eq!(app.get_selected_text(), Some("CDE".into()));
}

#[test]
fn app_state_select_all() {
    let mut app = make_app();
    app.set_query("Hello World".into());

    app.select_all();
    assert!(app.has_selection());
    assert_eq!(app.get_selection_range(), Some((0, 11)));
    assert_eq!(app.get_selected_text(), Some("Hello World".into()));
}

#[test]
fn app_state_delete_selection() {
    let mut app = make_app();
    app.set_query("Hello World".into());

    app.selection_start = Some(5);
    app.selection_end = Some(11);
    let deleted = app.delete_selection();
    assert_eq!(deleted, Some(" World".into()));
    assert_eq!(app.query_input(), "Hello");
    assert!(!app.has_selection());
}

#[test]
fn app_state_clear_selection() {
    let mut app = make_app();
    app.set_query("test".into());
    app.start_selection();
    assert!(app.has_selection());

    app.clear_selection();
    assert!(!app.has_selection());
}

// ========== Filtering ==========

#[test]
fn app_state_get_filtered_schemas_no_filter() {
    let mut app = make_app();
    app.schemas = make_schemas();

    let filtered = app.get_filtered_schemas();
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].2.len(), 3); // public has 3 tables
    assert_eq!(filtered[1].2.len(), 2); // admin has 2 tables
}

#[test]
fn app_state_get_filtered_schemas_with_filter() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.tables_filter = "user".into();

    let filtered = app.get_filtered_schemas();
    // Only "public" schema should match (has "users" table)
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].1.name, "public");
    assert_eq!(filtered[0].2.len(), 1); // Only "users" matches
    assert_eq!(filtered[0].2[0].1, "users");
}

#[test]
fn app_state_get_filtered_schemas_schema_name_match() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.tables_filter = "admin".into();

    let filtered = app.get_filtered_schemas();
    // "admin" schema name matches => included with all its tables
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].1.name, "admin");
}

#[test]
fn app_state_get_filtered_results_no_filter() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());

    let results = app.get_filtered_results().unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn app_state_get_filtered_results_with_filter() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());
    app.results_filter = "alice".into();

    let results = app.get_filtered_results().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 0); // First row index
}

#[test]
fn app_state_get_filtered_results_no_match() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());
    app.results_filter = "zzz_nonexistent".into();

    let results = app.get_filtered_results().unwrap();
    assert_eq!(results.len(), 0);
}

// ========== Panel Resize ==========

#[test]
fn app_state_adjust_sidebar_width() {
    let mut app = make_app();
    assert_eq!(app.sidebar_width, 25);

    app.adjust_sidebar_width(5);
    assert_eq!(app.sidebar_width, 30);

    app.adjust_sidebar_width(-20);
    assert_eq!(app.sidebar_width, 15); // clamped to min 15

    app.adjust_sidebar_width(100);
    assert_eq!(app.sidebar_width, 50); // clamped to max 50
}

#[test]
fn app_state_adjust_query_editor_height() {
    let mut app = make_app();
    assert_eq!(app.query_editor_height, 40);

    app.adjust_query_editor_height(10);
    assert_eq!(app.query_editor_height, 50);

    app.adjust_query_editor_height(-50);
    assert_eq!(app.query_editor_height, 20); // clamped to min 20

    app.adjust_query_editor_height(100);
    assert_eq!(app.query_editor_height, 80); // clamped to max 80
}

// ========== Quote Chars ==========

#[test]
fn app_state_get_quote_chars_no_connection() {
    let app = make_app();
    assert_eq!(app.get_quote_chars(), ('"', '"'));
}

#[test]
fn app_state_get_quote_chars_mysql() {
    let mut app = make_app();
    app.current_connection_config = Some(ConnectionConfig {
        name: "test".into(),
        db_type: DatabaseType::MySQL,
        host: None,
        port: None,
        username: None,
        password: None,
        database: "test".into(),
        azure_auth_method: None,
        tenant_id: None,
    });
    assert_eq!(app.get_quote_chars(), ('`', '`'));
}

#[test]
fn app_state_get_quote_chars_sqlserver() {
    let mut app = make_app();
    app.current_connection_config = Some(ConnectionConfig {
        name: "test".into(),
        db_type: DatabaseType::SQLServer,
        host: None,
        port: None,
        username: None,
        password: None,
        database: "test".into(),
        azure_auth_method: None,
        tenant_id: None,
    });
    assert_eq!(app.get_quote_chars(), ('[', ']'));
}

// ========== Misc ==========

#[test]
fn app_state_set_status() {
    let mut app = make_app();
    app.set_status("Loading...");
    assert_eq!(app.status_message, "Loading...");
}

#[test]
fn app_state_is_connected() {
    let app = make_app();
    assert!(!app.is_connected());
}

#[test]
fn app_state_should_show_results() {
    let mut app = make_app();
    assert!(!app.should_show_results());

    app.query_result = Some(make_query_result());
    assert!(app.should_show_results());
}

#[test]
fn app_state_should_show_results_on_error() {
    let mut app = make_app();
    app.connection_error = Some("Connection failed".into());
    assert!(app.should_show_results());
}

#[test]
fn app_state_compute_col_widths() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());
    app.compute_col_widths();

    assert_eq!(app.cached_col_widths.len(), 3);
    // Minimum width is 25
    assert!(app.cached_col_widths.iter().all(|w| *w >= 25));
}

#[test]
fn app_state_compute_col_widths_no_result() {
    let mut app = make_app();
    app.compute_col_widths();
    assert!(app.cached_col_widths.is_empty());
}

#[test]
fn app_state_update_known_columns() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());
    app.update_known_columns();
    assert_eq!(app.known_columns, vec!["id", "name", "age"]);
}

#[test]
fn app_state_get_known_tables() {
    let mut app = make_app();
    app.schemas = make_schemas();
    let tables = app.get_known_tables();
    // Should contain both schema.table and table formats
    assert!(tables.contains(&"public.users".to_string()));
    assert!(tables.contains(&"users".to_string()));
    assert!(tables.contains(&"admin.logs".to_string()));
    assert!(tables.contains(&"logs".to_string()));
}

#[test]
fn app_state_query_tab_helpers() {
    let mut app = make_app();
    // Clear any loaded query from disk
    app.set_query(String::new());
    assert!(app.query_input().is_empty());

    app.set_query("SELECT 1".into());
    assert_eq!(app.query_input(), "SELECT 1");
    assert_eq!(app.cursor_position(), 8);

    app.set_cursor_position(3);
    assert_eq!(app.cursor_position(), 3);
}

// ========== Completion ==========

#[test]
fn app_state_hide_completion() {
    let mut app = make_app();
    app.show_completion = true;
    app.completion_suggestions = vec!["SELECT".into(), "SET".into()];
    app.completion_selected = 1;

    app.hide_completion();
    assert!(!app.show_completion);
    assert!(app.completion_suggestions.is_empty());
    assert_eq!(app.completion_selected, 0);
}

#[test]
fn app_state_completion_next_prev() {
    let mut app = make_app();
    app.completion_suggestions = vec!["a".into(), "b".into(), "c".into()];
    assert_eq!(app.completion_selected, 0);

    app.completion_next();
    assert_eq!(app.completion_selected, 1);
    app.completion_next();
    assert_eq!(app.completion_selected, 2);
    // Wrap
    app.completion_next();
    assert_eq!(app.completion_selected, 0);

    // Prev wraps backward
    app.completion_prev();
    assert_eq!(app.completion_selected, 2);
}

// ========== Schema Toggle ==========

#[test]
fn app_state_toggle_schema() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.selected_schema = 0;
    assert!(app.schemas[0].expanded);

    app.toggle_schema();
    assert!(!app.schemas[0].expanded);

    app.toggle_schema();
    assert!(app.schemas[0].expanded);
}

// ========== Table Navigation ==========

#[test]
fn app_state_navigate_tables() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.active_panel = ActivePanel::Tables;
    // Start at schema header (0, 0)
    app.selected_schema = 0;
    app.selected_table = 0;

    app.select_next(); // should move to first table: (0, 1)
    assert_eq!(app.selected_schema, 0);
    assert_eq!(app.selected_table, 1);

    app.select_next(); // (0, 2) - orders
    assert_eq!(app.selected_table, 2);

    app.select_next(); // (0, 3) - products
    assert_eq!(app.selected_table, 3);

    app.select_next(); // (1, 0) - admin schema header (collapsed)
    assert_eq!(app.selected_schema, 1);
    assert_eq!(app.selected_table, 0);

    // admin is collapsed, so next wraps back
    app.select_next();
    assert_eq!(app.selected_schema, 0);
    assert_eq!(app.selected_table, 0);
}

#[test]
fn app_state_get_selected_table_full_name_on_header() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.selected_schema = 0;
    app.selected_table = 0; // Schema header

    assert!(app.get_selected_table_full_name().is_none());
}

#[test]
fn app_state_get_selected_table_full_name_on_table() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.selected_schema = 0;
    app.selected_table = 1; // "users"

    let name = app.get_selected_table_full_name().unwrap();
    // Default quotes are double quotes
    assert_eq!(name, "\"public\".\"users\"");
}

// ========== Open Dialogs with State ==========

#[test]
fn app_state_open_export_dialog_with_result() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());
    app.set_query("SELECT * FROM users".into());

    app.open_export_dialog();
    assert_eq!(app.dialog_mode, DialogMode::Export);
    assert!(app.export_state.is_some());
}

#[test]
fn app_state_open_export_dialog_without_result() {
    let mut app = make_app();
    app.open_export_dialog();
    // No query_result => no export dialog
    assert_eq!(app.dialog_mode, DialogMode::None);
    assert!(app.export_state.is_none());
}

#[test]
fn app_state_open_batch_export_dialog() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.open_batch_export_dialog();
    assert_eq!(app.dialog_mode, DialogMode::BatchExport);
    let batch = app.batch_export_state.as_ref().unwrap();
    assert_eq!(batch.tables.len(), 5); // 3 public + 2 admin
}

#[test]
fn app_state_open_truncate_confirm() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.selected_schema = 0;
    app.selected_table = 1; // "users"

    app.open_truncate_confirm();
    assert_eq!(app.dialog_mode, DialogMode::TruncateConfirm);
    assert!(app.truncate_table_name.is_some());
}

#[test]
fn app_state_open_truncate_confirm_on_schema_header() {
    let mut app = make_app();
    app.schemas = make_schemas();
    app.selected_schema = 0;
    app.selected_table = 0; // Schema header, not a table

    app.open_truncate_confirm();
    // Should not open because no table is selected
    assert_eq!(app.dialog_mode, DialogMode::None);
}

#[test]
fn app_state_open_delete_row_confirm() {
    let mut app = make_app();
    app.query_result = Some(make_query_result());
    app.set_query("SELECT * FROM users".into());

    app.open_delete_row_confirm();
    assert_eq!(app.dialog_mode, DialogMode::DeleteRowConfirm);
}

#[test]
fn app_state_open_batch_truncate() {
    let mut app = make_app();
    app.schemas = make_schemas();

    app.open_batch_truncate_dialog();
    assert_eq!(app.dialog_mode, DialogMode::BatchTruncate);
    let batch = app.batch_truncate_state.as_ref().unwrap();
    assert_eq!(batch.tables.len(), 5);
}

// ========== System Column Detection ==========

#[test]
fn app_state_is_system_column() {
    let mut app = make_app();
    app.system_columns = vec![0, 5, 6];
    assert!(app.is_system_column(0));
    assert!(app.is_system_column(5));
    assert!(!app.is_system_column(1));
}
