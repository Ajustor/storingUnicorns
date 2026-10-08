# storingUnicorns 🦄

A terminal-based database client inspired by JetBrains DataGrip, built with Rust and ratatui.

## Features

- Multi-database support (PostgreSQL, MySQL, SQLite, SQL Server)
- Connection management with dialog-based creation
- Schema browser (tables list)
- SQL query editor
- Results table with navigation
- Persistent configuration
- Contextual help bar

## Project Structure

```
src/
├── main.rs                  # mod declarations, #[tokio::main] calling tui::run
├── engine/                  # UI-agnostic core
│   ├── mod.rs
│   ├── config/              # AppConfig: load/save connections
│   ├── db/                  # DatabaseConnection: unified DB interface + per-driver connectors
│   ├── models/              # ConnectionConfig, QueryResult, Column
│   ├── services/            # export/import, query tabs, schema SQL, table cache
│   ├── sql/
│   │   ├── lexer.rs         # SQL tokenizer + completions
│   │   └── statements.rs    # statement splitting, table extraction, quote chars
│   └── ops/                 # business operations shared by the front-ends
│       ├── query.rs         # run_query, run_unit, refresh_schemas
│       ├── rows.rs          # update/insert/delete row, truncate, system columns
│       ├── schema.rs        # fetch_columns, apply_modification
│       └── transfer.rs      # import_csv, import_tables, export_tables
└── tui/                     # Terminal UI (ratatui)
    ├── mod.rs               # run(), event loop, handlers
    ├── app_state.rs         # AppState: runtime state, dialogs
    ├── key_handlers/        # per-panel keybindings
    └── ui/                  # layout, widgets, modals, SQL highlighting
```

## Keybindings

### Main Interface

| Key         | Context        | Action                         |
|-------------|----------------|--------------------------------|
| `Tab`       | Any            | Next panel                     |
| `Shift+Tab` | Any            | Previous panel                 |
| `↑/k`       | Lists          | Select previous item           |
| `↓/j`       | Lists          | Select next item               |
| `Enter`     | Connections    | Connect to database            |
| `Enter`     | Tables         | Generate SELECT query          |
| `n`         | Connections    | New connection dialog          |
| `d`         | Connections    | Delete selected connection     |
| `F5`        | Any            | Execute query                  |
| `Ctrl+R`    | Any            | Refresh tables                 |
| `?`         | Any            | Show help in status bar        |
| `q`         | Outside editor | Quit                           |
| `Ctrl+Q`    | Any            | Force quit                     |

### New Connection Dialog

| Key         | Action                              |
|-------------|-------------------------------------|
| `Tab/↓`     | Next field                          |
| `Shift+Tab/↑` | Previous field                    |
| `←/→`       | Cycle database type (on Type field) |
| `Enter`     | Save connection                     |
| `Esc`       | Cancel                              |

## Configuration

Connections are stored in `~/.config/storingUnicorns/config.toml`:

```toml
[[connections]]
name = "Local Postgres"
db_type = "Postgres"
host = "localhost"
port = 5432
username = "postgres"
password = "secret"
database = "mydb"

[[connections]]
name = "SQLite DB"
db_type = "SQLite"
database = "./data.db"
```

## Building

```bash
cargo build --release
```

## Running

```bash
cargo run
# or after building:
./target/release/storingUnicorns
```

## Layout

```
┌─────────────┬─────────────────────────────────┐
│ Connections │  Query Editor                   │
├─────────────┤                                 │
│ Tables      ├─────────────────────────────────┤
│             │  Results                        │
└─────────────┴─────────────────────────────────┘
│ Status: Connected to mydb                     │
├───────────────────────────────────────────────┤
│ Enter Connect │ n New │ d Delete │ Tab Next  │
└───────────────────────────────────────────────┘
```

## TODO

- [x] Multi-line query editor with proper cursor movement
- [x] Table structure view (columns, types, indexes)
- [x] Query history
- [x] Result set export (CSV, JSON)
- [x] Syntax highlighting for SQL
- [ ] Async query execution with cancellation
- [ ] Tab completion for table/column names
- [x] Edit existing connections
