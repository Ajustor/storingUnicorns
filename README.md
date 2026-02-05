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
├── main.rs          # Entry point, event loop, keybindings
├── config/          # Configuration management
│   └── mod.rs       # AppConfig: load/save connections
├── db/              # Database layer
│   ├── mod.rs
│   └── connector.rs # DatabaseConnection: unified DB interface
├── models/          # Data structures
│   ├── mod.rs
│   └── connection.rs # ConnectionConfig, QueryResult, Column
├── services/        # Application logic
│   ├── mod.rs
│   └── app_state.rs # AppState: runtime state, dialogs
└── ui/              # Terminal UI
    ├── mod.rs
    ├── layout.rs    # Main layout, panel arrangement
    └── widgets.rs   # Panel renderers, dialogs, help bar
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

- [ ] Multi-line query editor with proper cursor movement
- [ ] Table structure view (columns, types, indexes)
- [ ] Query history
- [ ] Result set export (CSV, JSON)
- [ ] Syntax highlighting for SQL
- [ ] Async query execution with cancellation
- [ ] SSH tunnel support
- [ ] Tab completion for table/column names
- [x] Edit existing connections
