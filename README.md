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
    ID account through the browser (MFA included, nothing to set up, see
    [Azure AD interactif](#azure-ad-interactif))
  - Cloud SQL PostgreSQL / MySQL: your Google account through `gcloud`, no
    password, tokens renewed by themselves (see
    [IAM Google Cloud](#iam-google-cloud-cloud-sql))
- **GUI** (default) with DataGrip ergonomics:
  - several connections open at once, each with its own colour (dot on its tabs)
  - database explorer: connection → schemas → tables → columns / keys / indexes,
    loaded lazily, with a filter and context menus (data, console, DDL, structure,
    export / import / truncate, single table or batch)
  - SQL consoles: highlighting, context-aware completion of tables and columns
    (see [Complétion contextuelle](#complétion-contextuelle)), formatting,
    one result tab per statement (pinnable) plus an "Sortie" log, results capped
    at 1 000 rows, persistent query history (searchable, with a SQL preview)
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

### Complétion contextuelle

La complétion (console, barres `WHERE` / `ORDER BY` de la vue d'une table, et
éditeur de l'interface terminal) propose des noms selon la position du curseur
dans l'instruction en cours, et non plus seulement les colonnes du dernier
résultat :

- **Colonnes des tables citées** : après `SELECT`, `WHERE`, `ORDER BY`,
  `GROUP BY`, `ON`… ce sont les colonnes des tables de `FROM` / `JOIN`
  (uniquement celles du niveau de sous-requête où se trouve le curseur).
- **`alias.`** (ou `table.`) : les colonnes de cette table seulement.
- **Jointures** : les colonnes de toutes les tables jointes ; un nom présent dans
  plusieurs tables est préfixé (`u.id`, `o.id`).
- **`INSERT INTO t (…)`** : les colonnes de `t` ; **`UPDATE t SET …`** : les
  colonnes de `t`.
- Chaque suggestion affiche le **type** de la colonne et `PK` pour une clé
  primaire.
- Les noms sont **protégés selon le moteur** quand c'est nécessaire (`"…"`
  PostgreSQL, `` `…` `` MySQL, `[…]` SQL Server).
- Les colonnes d'une table sont **chargées en arrière-plan** à la première
  utilisation ; la liste se complète dès qu'elles arrivent, sans bloquer la
  saisie.

Touches : `Ctrl+Espace` ouvre la liste, `↑` / `↓` choisissent, `Entrée` ou `Tab`
valident (la suggestion remplace le mot en cours), `Échap` ferme.

### Historique des requêtes

`Ctrl+Alt+E` ouvre l'historique : chaque entrée affiche un résumé de la requête,
sa **connexion**, l'**heure** et la **durée** d'exécution. Un volet d'**aperçu**
montre le SQL complet avec sa coloration. La recherche porte sur le SQL et sur
le nom de la connexion. **Insérer** place la requête dans la console, **Copier**
la met dans le presse-papiers.

### Clés étrangères

Les colonnes qui portent une clé étrangère sont marquées **FK** avec leur cible
(`→ table.colonne`) dans les en-têtes de la grille, l'explorateur, la structure
et la complétion ; les colonnes de clé primaire sont marquées d'une clé.

- **Grille** : une cellule FK est un lien. Cliquer sur l'icône de lien (ou
  `Alt+Entrée`) affiche la ligne liée dans le panneau de valeur : fil d'Ariane,
  bouton **Retour**, ses propres clés étrangères cliquables pour continuer, et
  la liste **Référencée par** (les lignes d'autres tables qui pointent vers
  elle). `Ctrl+clic` sur le lien, ou **Ouvrir dans un onglet**, ouvre un nouvel
  onglet « table (filtre) » filtré sur cette ligne.
- Si la clé ne correspond à aucune ligne, l'aperçu indique que la clé est
  orpheline.
- **Menu contextuel d'une cellule** : **Voir la ligne liée** et
  **Enregistrements liés** (les lignes des tables qui référencent la ligne
  courante, dans un onglet filtré).
- **Structure** : la colonne **Lien** donne la cible de chaque clé, et les
  sections **Clés étrangères** et **Référencée par** listent les clés sortantes
  et entrantes ; chaque lien est cliquable et ouvre la structure de l'autre
  table.

### Raccourcis (GUI)

| Touche                      | Action                                                   |
|-----------------------------|----------------------------------------------------------|
| `Ctrl+Entrée`               | Console : exécuter la sélection / l'instruction au curseur ; grille : Submit |
| `F5`                        | Exécuter tout le script                                  |
| `Échap`                     | Annuler la requête en cours / l'édition de cellule / fermer la modale |
| `Ctrl+Alt+L`                | Formater le SQL                                          |
| `Ctrl+Alt+E`                | Historique des requêtes (recherche, aperçu, Insérer / Copier) |
| `Ctrl+N`                    | Rechercher une table                                     |
| `Ctrl+T` / `Ctrl+W`         | Nouvelle console (connexion courante) / fermer l'onglet  |
| `Ctrl+S`                    | Enregistrer les consoles                                 |
| `Ctrl+Espace`               | Autocomplétion contextuelle (console, barres WHERE / ORDER BY) |
| `F2` / double-clic          | Éditer la cellule                                        |
| `Alt+Entrée`                | Cellule FK : voir la ligne liée dans le panneau de valeur |
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
| `←/→`       | Cycle the value (type, model, Azure auth, PostgreSQL / MySQL auth, SSL) |
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
name = "Cloud SQL"
db_type = "Postgres"
host = "34.12.34.56"
port = 5432
username = "jane.doe@example.com" # the Google account (no password stored)
database = "app"
db_auth = "GcpIam"      # Postgres / MySQL: Password (default) or GcpIam
ssl_mode = "Require"    # GcpIam connects with at least Require

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
username = "jane.doe@contoso.com"       # optional: the account to sign in with
# Advanced, both optional (InteractiveBrowser):
# tenant_id = "contoso.com"             # GUID, domain or "organizations"; default: asked from the server
# azure_client_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"  # the organization's app; default: Microsoft's drivers'
```

## Azure AD interactif

Connexion à Azure SQL avec **votre propre compte Entra ID** (MFA comprise), par
le navigateur, comme le mode *Active Directory Interactive* de SSMS ou de
DataGrip, sans installer Azure CLI et **sans rien configurer**.

### Dans storingUnicorns

Type **Azure SQL**, authentification **Azure AD interactif (navigateur)**
(interface terminal : `←/→` sur `Auth` jusqu'à `Azure AD (navigateur)`). Aucun
réglage n'est nécessaire : le **Compte** (facultatif,
`prenom.nom@contoso.com`) est seulement pré-rempli sur la page de connexion.

Comme les drivers SQL Server de Microsoft (et donc DataGrip) :

- le **tenant** est demandé au serveur : avant la connexion, Azure SQL indique
  où ses utilisateurs Entra ID se connectent (s'il ne le dit pas,
  `organizations`, c'est-à-dire l'annuaire de votre compte) ;
- l'**application** est celle des drivers Microsoft (ID client
  `7f98cb04-cd1e-40df-9140-3bf7e2cea4db`), publique et déjà autorisée pour
  Azure SQL : votre organisation n'a rien à enregistrer.

Pour une organisation qui bloque cette application, ou qui veut la sienne, voir
les [options avancées](#options-avancées).

À la première connexion, la page de connexion Microsoft s'ouvre dans le
navigateur par défaut ; terminez-la (5 minutes au plus) puis revenez à l'app.
Pendant l'attente, l'interface graphique affiche un bandeau avec **Copier le
lien** (si le navigateur ne s'est pas ouvert) et **Annuler** ; l'interface
terminal affiche le lien complet dans la barre d'état et `Échap` annule. Pour
copier le lien, sélectionnez-le avec **Maj+glisser** (**Option+glisser** dans
iTerm2) : l'interface capture la souris, un simple glisser ne sélectionne rien.
Avec certaines erreurs (application refusée par l'organisation, tenant
introuvable, URI de redirection absente de votre propre application), Microsoft
affiche l'erreur dans le navigateur sans revenir à l'app : annulez la
connexion.

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

### Options avancées

Dans **Options avancées** sous le formulaire (interface graphique, ouvertes
d'office quand l'un des deux est renseigné), ou dans les champs `Tenant` et
`ID app` (interface terminal, `auto` tant qu'ils sont vides) :

| Champ | Valeur |
|---|---|
| Tenant (facultatif) | vide : demandé au serveur (à défaut `organizations`). Sinon l'ID de l'annuaire (GUID) ou un domaine (`contoso.com` : lettres, chiffres, `.` et `-`, avec au moins une lettre ou un chiffre), ou `organizations` (le tenant d'origine du compte). `common` et `consumers` sont refusés (ils acceptent les comptes Microsoft personnels, qu'Azure SQL refuse) |
| ID client de l'application (facultatif) | l'ID d'application enregistrée par votre organisation (voir ci-dessous) |

Indiquer le **tenant** n'est utile que si le serveur ne donne pas le sien et que
votre compte doit se connecter à un autre annuaire que celui de son
organisation (un compte invité, par exemple).

Votre **propre application** sert à une organisation qui n'autorise pas celle
des drivers Microsoft et ne veut pas y consentir (Microsoft demande alors
l'approbation d'un administrateur, ou l'app signale un consentement manquant,
`AADSTS65001`), ou qui préfère la sienne. Un administrateur Entra ID
l'enregistre une fois :

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
5. Notez l'**ID d'application (client)** de la page **Vue d'ensemble** (et
   l'**ID de l'annuaire (locataire)** si vous indiquez aussi le tenant).

### Jetons

Le jeton d'accès reste en mémoire. Le jeton de renouvellement est gardé dans le
trousseau du système (Gestionnaire d'identification Windows, Trousseau macOS,
Secret Service sous Linux), sous le service `storingUnicorns` : les connexions
suivantes, même après un redémarrage, se font sans navigateur tant qu'il est
valable. Si le trousseau n'est pas disponible, il ne dure que la session (un
avertissement le signale). Les reconnexions automatiques (après une requête
interrompue, ou pour la session d'une transaction manuelle) n'ouvrent jamais le
navigateur : si le jeton de renouvellement ne suffit plus, l'opération échoue
avec « Session Azure AD expirée » ; déconnectez puis reconnectez la connexion.
Aucun jeton n'est écrit dans `config.toml` ni dans les journaux.

Pour l'oublier : **Oublier la connexion Azure AD** dans le menu de la connexion
(interface graphique) efface ses jetons :

- avec un tenant saisi, ceux de ce tenant ;
- sans tenant saisi, ceux du tenant du serveur (celui qu'il a donné pendant la
  session ou qui a été noté dans le trousseau ; à défaut, l'app le lui
  redemande, et s'il ne répond pas ces jetons peuvent rester dans le trousseau,
  ce que le journal signale), et aussi ceux de la connexion partagée faite sans
  tenant connu (`organizations`, même application et même compte) : les autres
  connexions de ce compte qui s'en servaient peuvent redemander de se connecter
  dans le navigateur.

Supprimer la connexion, ou changer son tenant, son application, son compte, sa
méthode d'authentification ou, sans tenant saisi, son serveur, l'oublie aussi,
sauf si une autre connexion utilise la même : même tenant saisi, même
application et même compte, ou, sans tenant saisi, même serveur, même
application et même compte (une autre base du même serveur, par exemple). Même
alors, les jetons dont une autre connexion se sert restent (ceux d'un tenant
commun à deux serveurs, ou de la connexion partagée `organizations`).

Le trousseau garde aussi de petites notes, sans secret : le tenant qu'utilise le
serveur d'une connexion sans tenant saisi, et celui où a abouti la connexion
partagée `organizations`. Elles sont effacées avec la connexion Azure AD
qu'elles décrivent.

Le trousseau est celui du compte du système : tous les profils de
storingUnicorns (`STORINGUNICORNS_CONFIG_DIR`) partagent ces jetons, et oublier
une connexion dans un profil l'oublie aussi pour les autres.

## IAM Google Cloud (Cloud SQL)

Connexion à Cloud SQL **PostgreSQL** ou **MySQL** avec **votre compte Google**
(authentification IAM de base de données), sans mot de passe ni proxy. L'app
demande un jeton à `gcloud` à la connexion, l'envoie à la place du mot de
passe et le renouvelle avant son expiration (environ une heure). Le jeton reste
en mémoire : il n'est écrit ni dans `config.toml` ni dans les journaux.

### Dans storingUnicorns

Type **PostgreSQL** ou **MySQL**, **Authentification** : **IAM Google Cloud**
(interface terminal : `←/→` sur `Auth`, à côté du port). Indiquez l'adresse IP
(ou le nom) de l'instance dans **Hôte** et votre **Compte Google**
(`prenom.nom@domaine.com`) : le formulaire affiche en dessous l'**utilisateur
base** correspondant. Il n'y a pas de mot de passe.

Le SSL est **au moins Obligatoire** : en mode Désactivé ou Préféré, l'app se
connecte quand même en TLS (le formulaire le rappelle). Choisissez *Vérifier
l'autorité (CA)* avec le certificat du serveur de l'instance (`server-ca.pem`,
onglet **Connexions → Sécurité** de la console) pour le vérifier.

### Prérequis

1. Le [SDK Google Cloud](https://cloud.google.com/sdk/docs/install) installé
   (`gcloud` dans le `PATH`) et votre compte connecté :

   ```sh
   gcloud auth login prenom.nom@domaine.com
   ```

   Si la session expire, l'app demande de relancer `gcloud auth login`.
2. L'authentification IAM activée sur l'instance (option
   `cloudsql.iam_authentication` pour PostgreSQL, `cloudsql_iam_authentication`
   pour MySQL), une adresse IP joignable (réseau autorisé, ou IP privée depuis
   le même réseau) et, pour votre compte, le rôle **Utilisateur d'instance
   Cloud SQL** (`roles/cloudsql.instanceUser`).
3. L'utilisateur IAM créé sur l'instance, puis ses droits dans la base :

   ```sh
   gcloud sql users create prenom.nom@domaine.com --instance=mon-instance --type=cloud_iam_user
   # compte de service :
   gcloud sql users create sa@mon-projet.iam.gserviceaccount.com --instance=mon-instance --type=cloud_iam_service_account
   ```

   ```sql
   -- PostgreSQL (connecté avec un administrateur)
   GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO "prenom.nom@domaine.com";
   -- MySQL
   GRANT SELECT, INSERT, UPDATE, DELETE ON app.* TO 'prenom.nom'@'%';
   ```

   Sans utilisateur ou sans droits, la connexion est refusée bien que le compte
   soit authentifié ; le message de l'app le rappelle.

### Noms d'utilisateur

| Moteur | Compte utilisateur | Compte de service |
|---|---|---|
| PostgreSQL | l'adresse complète : `prenom.nom@domaine.com` | sans `.gserviceaccount.com` : `sa@mon-projet.iam` |
| MySQL | la partie avant `@` : `prenom.nom` | la partie avant `@` : `sa` |

Dans le formulaire, saisissez toujours le compte Google ; l'indice
**utilisateur base** montre le nom que la base attend.

### Alternative : le proxy Cloud SQL

Si l'instance n'est joignable que par le connecteur (pas d'IP autorisée,
certificats clients exigés), lancez le
[proxy Cloud SQL](https://cloud.google.com/sql/docs/postgres/sql-proxy) avec
l'authentification IAM automatique :

```sh
cloud-sql-proxy --auto-iam-authn mon-projet:europe-west1:mon-instance
```

puis connectez-vous à `127.0.0.1` (port 5432 ou 3306) en **Mot de passe**,
avec l'utilisateur base ci-dessus, **sans mot de passe** et SSL **Désactivé** ou
**Préféré** : le proxy chiffre la liaison et ajoute le jeton lui-même.

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
