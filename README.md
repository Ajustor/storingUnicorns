# DataGrip TUI

A terminal-based database client inspired by JetBrains DataGrip, built with Rust and ratatui.

## Features

- Multi-database support (PostgreSQL, MySQL, SQLite)
- Connection management with saved configurations
- Schema browser (tables list)
- SQL query editor
- Results table with navigation
- Persistent configuration

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
│   └── app_state.rs # AppState: runtime state management
└── ui/              # Terminal UI
    ├── mod.rs
    ├── layout.rs    # Main layout, panel arrangement
    └── widgets.rs   # Panel renderers (connections, tables, editor, results)
```

## Keybindings

| Key         | Action                              |
|-------------|-------------------------------------|
| `Tab`       | Next panel                          |
| `Shift+Tab` | Previous panel                      |
| `↑/k`       | Select previous item                |
| `↓/j`       | Select next item                    |
| `Enter`     | Connect (in Connections panel)      |
| `Enter`     | Generate SELECT query (in Tables)   |
| `F5`        | Execute query                       |
| `Ctrl+N`    | Add new connection                  |
| `Ctrl+R`    | Refresh tables                      |
| `q`         | Quit (outside query editor)         |
| `Ctrl+Q`    | Force quit                          |

## Configuration

Connections are stored in `~/.config/datagrip-tui/config.toml`:

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
./target/release/datagrip_tui
```

## TODO

- [ ] Multi-line query editor with proper cursor movement
- [ ] Connection dialog popup (instead of Ctrl+N placeholder)
- [ ] Table structure view (columns, types, indexes)
- [ ] Query history
- [ ] Result set export (CSV, JSON)
- [ ] Syntax highlighting for SQL
- [ ] Async query execution with cancellation
- [ ] SSH tunnel support
- [ ] Tab completion for table/column names
