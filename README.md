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
  - Azure SQL: SQL login, Azure CLI token, managed identity, or your own Entra
    ID account through the browser (MFA included, see
    [Azure AD interactif](#azure-ad-interactif))
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
  - transaction mode per connection, as in DataGrip: **Tx : Auto** commits each
    statement, **Tx : Manuel** keeps console statements and Submits in one
    transaction (on a dedicated session; grids show the pending changes) until
    **Commit** or **Rollback**
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
| `Esc`       | Connecting     | Cancel the connection          |
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
| `←/→`       | Cycle the value (type, model, Azure auth, SSL) |
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

[[connections]]
name = "Azure SQL"
db_type = "Azure"
host = "myserver.database.windows.net"
port = 1433
database = "app"
azure_auth_method = "InteractiveBrowser" # Credentials, Interactive (Azure CLI), InteractiveBrowser, ManagedIdentity
tenant_id = "contoso.com"               # GUID or domain
azure_client_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"  # InteractiveBrowser: the organization's app
username = "jane.doe@contoso.com"       # optional: the account to sign in with
```

## Azure AD interactif

Connexion à Azure SQL avec **votre propre compte Entra ID** (MFA comprise), par
le navigateur, comme le mode *Active Directory Interactive* de SSMS, sans
installer Azure CLI. Chaque organisation enregistre une fois sa propre
application dans Entra ID.

### Enregistrer l'application (une fois, administrateur Entra ID)

1. Portail Azure → **Microsoft Entra ID** → **Inscriptions d'applications** →
   **Nouvelle inscription** : un nom (par exemple `storingUnicorns`), comptes de
   **cet annuaire organisationnel uniquement**, sans URI de redirection pour
   l'instant.
2. **Authentification** → **Ajouter une plateforme** → **Applications mobiles et
   de bureau** → URI de redirection personnalisée `http://localhost` (sans port :
   l'app écoute sur un port libre de `127.0.0.1`).
3. Toujours dans **Authentification**, **Autoriser les flux clients publics** :
   **Oui** (application publique, sans secret).
4. **Autorisations d'API** → **Ajouter une autorisation** → **API utilisées par
   mon organisation** → **Azure SQL Database** → **Autorisations déléguées** →
   `user_impersonation`, puis **Accorder un consentement d'administrateur**.
5. Notez l'**ID d'application (client)** et l'**ID de l'annuaire (locataire)**
   de la page **Vue d'ensemble**.

### Dans storingUnicorns

Type **Azure SQL**, authentification **Azure AD interactif (navigateur)**
(interface terminal : `←/→` sur `Auth` jusqu'à `Azure AD (navigateur)`) :

| Champ | Valeur |
|---|---|
| Tenant | ID de l'annuaire ou domaine (`contoso.com`) ; pas `common` ni `organizations` |
| ID client de l'application | l'ID d'application noté plus haut |
| Compte (facultatif) | `prenom.nom@contoso.com`, pré-rempli sur la page de connexion |

À la première connexion, la page de connexion Microsoft s'ouvre dans le
navigateur par défaut ; terminez-la (5 minutes au plus) puis revenez à l'app.
Pendant l'attente, l'interface graphique affiche un bandeau avec **Copier le
lien** (si le navigateur ne s'est pas ouvert) et **Annuler** ; l'interface
terminal affiche le lien complet dans la barre d'état et `Échap` annule. Pour
copier le lien, sélectionnez-le avec **Maj+glisser** (**Option+glisser** dans
iTerm2) : l'interface capture la souris, un simple glisser ne sélectionne rien.
Avec certaines erreurs de configuration (URI de redirection absente,
application ou tenant introuvable), Microsoft affiche l'erreur dans le
navigateur sans revenir à l'app : annulez la connexion.

Interface terminal : le navigateur est ouvert comme le fait le système (sous
Linux, `$BROWSER` d'abord). Un navigateur en mode texte (`lynx`, `w3m`…)
prendrait la place de l'interface dans le terminal : faites pointer `$BROWSER`
vers un navigateur graphique, ou ouvrez vous-même le lien de la barre d'état
dans un navigateur de la même machine (la page de connexion revient sur
`http://localhost`).

### Créer l'utilisateur dans la base

Le compte doit exister dans la base. Un administrateur Entra ID du serveur
lance, connecté à **cette base** :

```sql
CREATE USER [prenom.nom@contoso.com] FROM EXTERNAL PROVIDER;
ALTER ROLE db_datareader ADD MEMBER [prenom.nom@contoso.com];
ALTER ROLE db_datawriter ADD MEMBER [prenom.nom@contoso.com];
```

Sans cela, la connexion échoue (erreur 18456) bien que le compte soit
authentifié ; le message de l'app le rappelle.

### Jetons

Le jeton d'accès reste en mémoire. Le jeton de renouvellement est gardé dans le
trousseau du système (Gestionnaire d'identification Windows, Trousseau macOS,
Secret Service sous Linux), sous le service `storingUnicorns` : les connexions
suivantes, même après un redémarrage, se font sans navigateur tant qu'il est
valable. Si le trousseau n'est pas disponible, il ne dure que la session (un
avertissement le signale). Les reconnexions automatiques (après une requête
interrompue, ou pour la session d'une transaction manuelle) n'ouvrent jamais le
navigateur : si le jeton de renouvellement ne suffit plus, l'opération échoue
avec « Session Azure AD expirée » ; déconnectez puis reconnectez la connexion. Aucun jeton n'est écrit dans `config.toml` ni dans
les journaux.

Pour l'oublier : **Oublier la connexion Azure AD** dans le menu de la connexion
(interface graphique). Supprimer la connexion, ou changer son tenant, son
application ou son compte, l'oublie aussi, sauf si une autre connexion utilise
le même compte (une autre base du même serveur, par exemple).

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

- macOS (Apple Silicon), recommandé : dans le Terminal,

  ```sh
  curl -fsSL https://ajustor.github.io/storingUnicorns/install-macos.sh | bash
  ```

  Le script (`scripts/install-macos.sh`) télécharge le `.dmg` de la dernière version, vérifie son
  SHA-256, installe l'app dans `~/Apps` (`STORINGUNICORNS_APP_DIR` pour un autre dossier, jamais un
  dossier nommé « Applications ») et ajoute `storingUnicorns` au `PATH`. Téléchargée par `curl`,
  l'app n'a pas l'attribut de quarantaine : ni avertissement Gatekeeper, ni blocage sous macOS 26.
  Relancez la commande pour réinstaller.
- macOS, sans le Terminal : ouvrez `storingUnicorns-macos-arm64.dmg` et glissez **storingUnicorns**
  sur le Bureau ou dans un dossier qui ne s'appelle pas « Applications » (macOS 26 y bloque les apps
  non notarisées ; sous macOS 15 et antérieur, **Applications** convient). L'app est signée ad hoc,
  sans notarisation Apple : au premier lancement, clic droit → **Ouvrir** (ou Réglages Système →
  Confidentialité et sécurité → **Ouvrir quand même**).
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
