# storingUnicorns 🦄

A fast database client inspired by JetBrains DataGrip, built with Rust: a
desktop GUI (egui) and a terminal UI (ratatui) sharing the same engine.

## Features

- PostgreSQL, MySQL, SQLite, SQL Server and Azure SQL
  - compatible products with a ready-made template (port, SSL mode): MariaDB,
    PlanetScale, CockroachDB, TimescaleDB, Supabase, Neon and Redshift
  - encrypted PostgreSQL / MySQL connections: SSL mode (disabled, preferred,
    required, verify CA, verify full) and an optional CA certificate; the
    system's certificates are trusted
  - paste a connection URL (`postgresql://user:pass@host/db?sslmode=require`)
    to fill in the connection form
- **GUI** (default) with DataGrip ergonomics:
  - several connections open at once, each with its own colour (dot on its tabs)
  - database explorer: connection → schemas → tables → columns / keys / indexes,
    loaded lazily, with a filter and context menus (data, console, DDL, structure,
    export / import / truncate, single table or batch)
  - SQL consoles: highlighting, completion of tables and columns, formatting,
    one result tab per statement (pinnable) plus an "Sortie" log, results capped
    at 1 000 rows, persistent query history
  - table data editor: 500-row pages, total counted in the background, `WHERE` /
    `ORDER BY` bar, click a header to sort, inline editing (changed cells
    highlighted, new rows green, deleted rows struck through in red), changes
    kept pending until **Submit** applies them in one transaction (or **Revert**)
  - console results read from a single table with a primary key are editable the same way
  - value panel (indented JSON, editable), read-only DDL tab, structure dialog
    (add / modify / rename / drop column), quick table search
  - dark and light themes (follows the system by default)
  - in-app updates
- **TUI** (`storingUnicorns tui`): the original terminal client
- Persistent configuration, consoles and history

## Running

```
storingUnicorns                 Open the graphical interface
storingUnicorns tui [OPTIONS]   Open the terminal interface
storingUnicorns update          Download and install the latest version
storingUnicorns --version       Print the version
storingUnicorns --help          Print this help

TUI OPTIONS:
    -d, --debug             Show generated SQL in the editor instead of running it
    -na, --no-animations    Disable animations
```

From the sources: `cargo run` (GUI) or `cargo run -- tui`.

## GUI

```
┌────────────────┬──────────────────────────────────────────┬───────────┐
│ EXPLORER  🔍   │ [● console prod] [● users] [● console dev]│ VALUE     │
│ ● Prod PG      │ SELECT * FROM users WHERE …               │ {         │
│  ▾ public      │                                           │  "a": 1   │
│   ▾ users      ├───────────────────────────────────────────┤ }         │
│     ▸ columns  │ [Résultat 1 📌] [Résultat 2] [Sortie]     │           │
│     ▸ keys     │ id │ name  │ email                        │           │
│     ▸ indexes  │ 1  │ Alice*│ alice@…                      │           │
│ ○ Dev MySQL    │ + │ − │ 2 modification(s)  ✓ Submit  Revert│          │
└────────────────┴──────────────────────────────────────────┴───────────┘
 ● ● 2 connexions · page 1/9 · 500 lignes · 12 ms              ◐ ▥ v0.9.0
```

- Expanding a connection in the explorer connects it; double-clicking a table
  opens its data editor.
- `Ctrl+Entrée` in a console runs the selection, otherwise the statement (or the
  whole `BEGIN … COMMIT` block) at the cursor; `F5` runs the whole script.
- Result tabs are replaced at each run, except pinned ones.
- `Échap` cancels the running query (a transaction block is rolled back by the
  database), the cell being edited, or closes the dialog.
- Errors are shown in red in the status bar (hover for the full text) and in
  the console's "Sortie" tab; nothing is swallowed.

### Raccourcis (GUI)

| Touche                      | Action                                                   |
|-----------------------------|----------------------------------------------------------|
| `Ctrl+Entrée`               | Console : exécuter la sélection / l'instruction au curseur ; grille : Submit |
| `F5`                        | Exécuter tout le script                                  |
| `Échap`                     | Annuler la requête en cours / l'édition de cellule / fermer la modale |
| `Ctrl+Alt+L`                | Formater le SQL                                          |
| `Ctrl+Alt+E`                | Historique des requêtes                                  |
| `Ctrl+N`                    | Rechercher une table                                     |
| `Ctrl+T` / `Ctrl+W`         | Nouvelle console (connexion courante) / fermer l'onglet  |
| `Ctrl+S`                    | Enregistrer les consoles                                 |
| `Ctrl+Espace`               | Autocomplétion                                           |
| `F2` / double-clic          | Éditer la cellule                                        |
| `Alt+Insert` / `Ctrl+Suppr` | Ajouter / supprimer une ligne                            |
| `Ctrl+R`                    | Rafraîchir (explorateur ou données)                      |

## Raccourcis (TUI)

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

### Layout

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

## Configuration

Every file lives in the app directory: `storing-unicorns/` under the platform
config directory (`%APPDATA%` on Windows, `~/.config` on Linux,
`~/Library/Application Support` on macOS):

| File                | Content                                          |
|---------------------|--------------------------------------------------|
| `config.toml`       | connections, theme, skipped update               |
| `queries.toml`      | saved consoles                                   |
| `history.json`      | last 500 executed queries (GUI)                  |
| `last_update_check` | time of the last automatic update check          |
| `debug.log`         | TUI log                                          |

Set `STORINGUNICORNS_CONFIG_DIR` to use another directory (a test profile, a
portable install…). The tests never touch the real directory.

```toml
theme = "dark"          # "system" (default), "dark" or "light"

[[connections]]
name = "Local Postgres"
db_type = "Postgres"
host = "localhost"
port = 5432
username = "postgres"
password = "secret"
database = "mydb"
color = [229, 83, 75]   # optional, GUI tab dot

[[connections]]
name = "Supabase"
db_type = "Postgres"
host = "db.abcd.supabase.co"
port = 5432
username = "postgres"
password = "p@ss:word"  # stored as typed, any character allowed
database = "postgres"
flavor = "Supabase"     # optional: MariaDb, PlanetScale, CockroachDb, TimescaleDb, Supabase, Neon, Redshift
ssl_mode = "VerifyFull" # Postgres / MySQL: Disable, Prefer (default), Require, VerifyCa, VerifyFull
ssl_ca = "C:/certs/ca.pem"  # optional extra trusted CA (PEM)

[[connections]]
name = "SQLite DB"
db_type = "SQLite"
database = "./data.db"
```

## Building

```bash
cargo build --release
cargo test
```

## Project Structure

```
src/
├── main.rs                  # entry point: dispatches GUI / TUI / update
├── cli.rs                   # command line parsing and help
├── console.rs               # Windows: detach the GUI from the launching console
├── engine/                  # UI-agnostic core
│   ├── config/              # AppConfig, app directory
│   ├── db/                  # DatabaseConnection: unified DB interface + per-driver connectors
│   ├── models/              # ConnectionConfig, QueryResult, Column, TableDetails
│   ├── services/            # export/import, query tabs, history, schema SQL, table cache
│   ├── sql/                 # lexer + completions, statements, paging, formatting
│   └── ops/                 # business operations shared by the front-ends
│       ├── query.rs         # run_query, run_table_page, run_at_cursor, run_script
│       ├── rows.rs          # submit_changes, row edits, truncate, system columns
│       ├── schema.rs        # table details, DDL, column modifications
│       └── transfer.rs      # import_csv, import_tables, export_tables
├── gui/                     # egui desktop client
│   ├── app.rs               # App: frame loop, event routing, global shortcuts
│   ├── worker.rs            # async operations on a tokio runtime, Event channel
│   ├── sessions.rs          # open connections and cached metadata
│   ├── explorer.rs          # database tree, filter, context menus
│   ├── tabs/                # console, data editor, DDL tabs
│   ├── grid/                # virtualised data grid + pending edits model
│   ├── editor/              # SQL editor, highlighting, completion popup
│   ├── dialogs/             # connection, structure, import/export
│   ├── value_panel.rs, history_popup.rs, table_search.rs, status.rs, theme.rs
├── tui/                     # terminal UI (ratatui)
│   ├── mod.rs               # run(), event loop, handlers
│   ├── app_state.rs         # AppState: runtime state, dialogs
│   ├── key_handlers/        # per-panel keybindings
│   └── ui/                  # layout, widgets, modals, SQL highlighting
└── updater/                 # release check, download, install, background check
```

## TODO

- [x] Multi-line query editor with proper cursor movement
- [x] Table structure view (columns, types, indexes)
- [x] Query history
- [x] Result set export (CSV, JSON)
- [x] Syntax highlighting for SQL
- [x] Async query execution with cancellation (GUI)
- [x] Tab completion for table/column names
- [x] Edit existing connections

## Installer

Depuis la [page de téléchargement](https://ajustor.github.io/storingUnicorns/) :

- macOS (Apple Silicon) : ouvrez `storingUnicorns-macos-arm64.dmg` et glissez **storingUnicorns**
  dans **Applications**. L'app est signée ad hoc, sans notarisation Apple : au premier lancement,
  clic droit → **Ouvrir** (ou Réglages Système → Confidentialité et sécurité → **Ouvrir quand même**).
- Linux (x86_64) : `chmod +x storingUnicorns-linux-x64.AppImage` puis lancez-la (FUSE requis :
  paquet `libfuse2` ou `fuse`) ; `./storingUnicorns-linux-x64.AppImage tui` ouvre l'interface terminal.

Au lancement, l'app et l'AppImage ajoutent la commande `storingUnicorns` au `PATH` : un lien dans
`/usr/local/bin` (macOS, s'il est accessible en écriture) ou `~/.local/bin`, ajouté si besoin au
fichier de démarrage du shell (`~/.zshrc`, `~/.bashrc` — `~/.bash_profile` sur macOS —, ou
`~/.config/fish/conf.d/`). Un `storingUnicorns` déjà installé par `install.sh` n'est pas remplacé.

Ces paquets se mettent à jour d'eux-mêmes (l'exécutable de l'app, ou le fichier `.AppImage`, est
remplacé). Ils sont produits par `scripts/build-macos-app.sh` (à lancer sur macOS) et
`scripts/build-appimage.sh`, à partir du binaire release et des fichiers de `packaging/`.

En ligne de commande (binaire seul) :

- Linux / macOS : `curl -fsSL https://ajustor.github.io/storingUnicorns/install.sh | sh`
- Windows : `irm https://ajustor.github.io/storingUnicorns/install.ps1 | iex`, ou l'installateur `.msi`
  sur la [page de téléchargement](https://ajustor.github.io/storingUnicorns/)
- Depuis les sources : `cargo install storingUnicorns`

## Publier une version

1. Mettre à jour `version` dans `Cargo.toml` et ajouter la section `## [X.Y.Z] - AAAA-MM-JJ` dans `CHANGELOG.md`.
2. Commit, puis `git tag vX.Y.Z && git push origin master vX.Y.Z`.
3. Le workflow Release construit les binaires, le MSI, l'AppImage et le `.dmg`, crée la release GitHub,
   publie la page de téléchargement + `latest.json` sur GitHub Pages, puis publie sur crates.io.
