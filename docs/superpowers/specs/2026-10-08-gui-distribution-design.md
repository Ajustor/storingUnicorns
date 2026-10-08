# Design — Interface graphique, mises à jour auto et distribution

Date : 2026-10-08
Statut : approuvé (en attente de revue du spec)

## Objectif

Donner à storingUnicorns une interface graphique propre et rapide, dans la veine
de codingUnicorns, tout en conservant le TUI et la ligne de commande actuels.
Simplifier l'installation et rendre les mises à jour automatiques via une page
de téléchargement GitHub Pages.

## Décisions

- **Un seul binaire, GUI par défaut.** Un seul fichier à télécharger et à mettre à jour.
- **CLI inchangée** dans ses capacités (pas de requêtes non interactives) : on ajoute
  seulement les sous-commandes `tui` et `update`.
- **GUI à parité fonctionnelle avec le TUI** dès cette version.
- **Workflow de release dédié** au repo (calqué sur codingUnicorns), qui remplace
  l'appel au workflow partagé `Ajustor/workflows/cargo-release.yml`.
- **Un seul crate**, réorganisé en `engine` / `tui` / `gui` / `updater` (le nom `core` est évité : il masquerait le crate standard `core`)
  (pas de workspace Cargo).

## Lancement

| Commande                                   | Effet                                   |
|--------------------------------------------|-----------------------------------------|
| `storingUnicorns`                          | Ouvre la GUI                            |
| `storingUnicorns tui [--debug] [--no-animations]` | Lance le TUI                     |
| `storingUnicorns --debug` / `--no-animations` (sans sous-commande) | Lance le TUI (compatibilité) |
| `storingUnicorns update`                   | Vérifie et applique la mise à jour      |
| `storingUnicorns --version` / `-v`         | Affiche la version                      |
| `storingUnicorns --help` / `-h`            | Affiche l'aide                          |

Le parsing reste manuel (pas de `clap`) : le jeu d'options est minuscule.

Sous Windows, le binaire est compilé en sous-système `windows` (pas de console au
double-clic). Pour `tui`, `update`, `--version` et `--help`, le programme rattache
la console du terminal parent (`AttachConsole(ATTACH_PARENT_PROCESS)`) ; si aucune
console parente n'existe (lancé hors terminal), il en alloue une (`AllocConsole`)
pour le TUI.

## Architecture

```
src/
├── main.rs        # Parsing des arguments, dispatch GUI / TUI / update
├── engine/        # Indépendant de toute UI
│   ├── config/    # AppConfig (déplacé)
│   ├── db/        # Connecteurs (déplacés)
│   ├── models/    # ConnectionConfig, QueryResult… (déplacés)
│   ├── services/  # schema_service, export_import, query_tabs, table_cache (déplacés)
│   └── ops/       # NOUVEAU : opérations métier extraites de main.rs
├── tui/           # Code ratatui actuel (main loop, key_handlers, ui, AppState)
├── gui/           # NOUVEAU : frontend egui
└── updater/       # NOUVEAU : porté de codingUnicorns
```

### `engine::ops`

Fonctions async sans état UI, extraites de `main.rs` :

- `split_statements`, détection du bloc de transaction au curseur, `execute_script`
  (instruction au curseur, script complet, bloc `BEGIN…COMMIT` sur connexion dédiée).
- `save_row`, `insert_row`, `delete_row`, `truncate_table(s)`.
- `apply_schema_action` (créer/modifier/supprimer table et colonnes).
- `export_tables` (par lot), `import_csv` / `import_tables` (simple et par lot) ;
  l'export simple reste `services::export_import::export_to_file`.
- `detect_system_columns` (colonnes auto-générées exclues de l'INSERT).
- Tokenizer SQL et autocomplétion (`tokenize_sql`, `get_completions`) déplacés
  dans `engine::sql` ; seul le rendu ratatui reste dans le TUI.
- `extract_table_from_query`, quoting par SGBD.

Chaque fonction prend une `&DatabaseConnection` (ou la config) et des paramètres
explicites, et renvoie un `Result<…>` typé. Le TUI est rebranché sur ces fonctions :
son comportement ne doit pas changer.

`services::query_tabs` (onglets de requêtes persistés) est partagé : GUI et TUI
lisent et écrivent les mêmes fichiers.

## GUI

Stack : `eframe`/`egui` 0.31, `egui_extras` (tables), `egui-phosphor` (icônes),
`rfd` (dialogues fichiers pour export/import) — mêmes versions que codingUnicorns.

### Disposition

```
┌──────────────┬─────────────────────────────────────────────┐
│ CONNEXIONS   │ [requête 1] [requête 2] [+]                  │
│ ● Local PG   │ SELECT * FROM users WHERE …                  │
│ ○ Prod SQLSrv│                                              │
├──────────────┤                                              │
│ TABLES  🔍   ├─────────────────────────────────────────────┤
│ ▸ public     │ Résultats  🔍 filtre      42 lignes · 12 ms  │
│   users      │ id │ name  │ email                           │
│   orders     │ 1  │ Alice │ alice@…                         │
└──────────────┴─────────────────────────────────────────────┘
 ● Connecté à mydb (Postgres)                  ⬆ v0.9.1 disponible
```

- **Barre latérale** redimensionnable : liste des connexions (connecter, nouvelle,
  modifier, supprimer via boutons et menu contextuel), puis arbre schéma → tables
  avec filtre. Clic sur une table : `SELECT` dans l'onglet courant ; menu contextuel :
  structure, export, import, truncate. Les opérations par lot (export, import, vidage) passent par une fenêtre
  listant toutes les tables avec des cases à cocher.
- **Éditeur SQL** : onglets (ajout/fermeture/renommage), `TextEdit` multi-ligne avec
  coloration SQL via un `layouter` (réutilise le tokenizer de `sql_highlight`),
  popup d'autocomplétion des tables/colonnes connues.
- **Résultats** : grille virtualisée (`TableBuilder`, seules les lignes visibles sont
  rendues), colonnes redimensionnables, filtre texte, sélection de cellule, copie,
  double-clic → modale d'édition de ligne, menu contextuel ajouter/supprimer ligne.
- **Barre de statut** : état de connexion, durée et nombre de lignes, erreurs,
  indicateur de mise à jour.
- **Modales** : connexion (tous SGBD, y compris méthodes Azure AD), structure de
  table (colonnes, types, index, éditeur de colonnes), export (CSV / SQL INSERT) et import CSV, simples et par
  lot, confirmations destructives, à propos / mise à jour.

### Raccourcis

| Touche              | Action                                   |
|---------------------|------------------------------------------|
| `Ctrl+Entrée`       | Exécuter l'instruction (ou bloc de transaction) au curseur |
| `F5`                | Exécuter tout l'éditeur                  |
| `Échap`             | Annuler la requête en cours / fermer la modale |
| `Ctrl+T` / `Ctrl+W` | Nouvel onglet / fermer l'onglet          |
| `Ctrl+S`            | Sauvegarder les onglets                  |
| `Ctrl+Espace`       | Autocomplétion                           |
| `Ctrl+R`            | Rafraîchir les tables                    |

### Thème

Sombre et clair, suivant le thème système par défaut, choix mémorisé dans la
config. Palette inspirée de codingUnicorns. Pas de thèmes personnalisables.

### Concurrence

- Un runtime tokio multi-thread est créé au démarrage de la GUI.
- La GUI envoie des `Command` (`Connect`, `LoadTables`, `Execute`, `SaveRow`,
  `SchemaAction`, `Export`, `Import`, `Cancel`…) au runtime ; chaque commande est
  `spawn`ée et son résultat revient en `Event` sur un channel `std::sync::mpsc`,
  drainé à chaque frame. Le worker appelle `ctx.request_repaint()` après envoi.
- L'UI ne bloque jamais. Une exécution en cours garde son `JoinHandle` ;
  `Cancel` fait `abort()` dessus (pour un bloc de transaction, la connexion dédiée
  est relâchée sans `COMMIT`, donc le SGBD annule).
- Une seule connexion active à la fois, comme le TUI.

### Erreurs

Toute erreur d'opération remonte en `Event` portant le message d'erreur : affichée
en rouge dans la barre de statut (et dans le formulaire concerné pour la création
de connexion). Aucune erreur n'est avalée silencieusement.

## Mises à jour

Module `updater` porté de codingUnicorns, adapté :

- Manifeste : `https://ajustor.github.io/storingUnicorns/latest.json`
  (`version`, `notes`, `page_url`, `assets[{name,url,sha256}]`).
- Assets : `storingUnicorns-windows-x64.exe`, `storingUnicorns-linux-x64`,
  `storingUnicorns-macos-arm64`, `storingUnicorns-setup.msi`.
- Comparaison semver avec `CARGO_PKG_VERSION`, vérification SHA-256, plafond de taille.
- Installation : remplacement du binaire en place (`self-replace`) ; si installé via
  MSI (sous Program Files), téléchargement du MSI et lancement de `msiexec` à la
  sortie.
- Le cœur de l'updater ne dépend pas d'egui : la GUI l'utilise en tâche de fond
  (bannière « Mise à jour disponible » → « Installer » → « Redémarrer »), le TUI
  vérifie après sa sortie et affiche un message, `storingUnicorns update` fait
  vérification + installation en synchrone avec sortie console.
- Version ignorée (« Ne plus proposer cette version ») mémorisée dans la config.
- `update-notifier` est retiré des dépendances.

## Release et distribution

Nouveau `.github/workflows/release.yml`, déclenché par un tag `v*` :

1. **verify-version** : tag = version de `Cargo.toml` ; section présente dans
   `CHANGELOG.md` (hors pré-release).
2. **build** (matrice) : Linux x64, macOS arm64, Windows x64 ; MSI WiX sur Windows
   (raccourci menu Démarrer, ajout au PATH, MajorUpgrade) ; signature Authenticode
   optionnelle si les secrets existent.
3. **release** : GitHub Release avec la section du CHANGELOG comme notes.
4. **pages** : publie sur GitHub Pages `index.html` (page de téléchargement générée
   depuis `pages/index.html`, détection de l'OS du visiteur, notes et historique),
   `latest.json`, les binaires, `install.sh` et `install.ps1`.
5. **crates** : `cargo publish` (si `CARGO_REGISTRY_TOKEN` est défini).

`pages.yml` reconstruit le site sur push de `pages/**`, `CHANGELOG.md` ou des
scripts, à partir de la dernière release stable.

Scripts d'installation servis par Pages :

- `curl -fsSL https://ajustor.github.io/storingUnicorns/install.sh | sh` :
  détecte OS/arch, télécharge le binaire, vérifie le SHA-256 via `latest.json`,
  installe dans `~/.local/bin`, prévient si ce dossier n'est pas dans le `PATH`.
- `irm https://ajustor.github.io/storingUnicorns/install.ps1 | iex` : installe dans
  `%LOCALAPPDATA%\Programs\storingUnicorns`, ajoute au `PATH` utilisateur, crée un
  raccourci menu Démarrer.

Fichiers ajoutés : `CHANGELOG.md`, `pages/index.html`, `scripts/build-site.sh`,
`scripts/build-pages.py`, `scripts/changelog.py`, `scripts/install.sh`,
`scripts/install.ps1`, `wix/main.wxs`, `assets/icon.png` (+ `.ico` embarqué via
`build.rs`/`winresource`).

Action manuelle : passer la source GitHub Pages du repo de « Deploy from a branch
(master /docs) » à « GitHub Actions ».

Profil release : `opt-level = "s"`, `lto = true`, `codegen-units = 1`, `strip = true`.

## Tests

- `engine::ops` : tests unitaires sur SQLite en mémoire — découpage d'instructions,
  détection et exécution de blocs de transaction (commit et rollback sur erreur),
  save/insert/delete, export puis import aller-retour.
- `updater` : tests repris de codingUnicorns (manifeste, sélection d'asset, digest,
  états).
- Parsing des arguments : tests unitaires du dispatch.
- Non-régression TUI : `cargo test` + lancement manuel.
- GUI : pas de tests automatisés ; vérification manuelle (connexion SQLite,
  exécution, édition, export/import, mise à jour simulée).

## Hors périmètre

- CLI non interactive (`query`, `connections list`).
- macOS Intel.
- Thèmes personnalisables, plugins.
- Connexions multiples simultanées.
