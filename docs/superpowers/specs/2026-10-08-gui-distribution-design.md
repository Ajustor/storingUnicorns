# Design — Interface graphique, mises à jour auto et distribution

Date : 2026-10-08
Statut : approuvé (en attente de revue du spec)

## Objectif

Donner à storingUnicorns une interface graphique propre et rapide, dans la veine
de codingUnicorns, tout en conservant le TUI et la ligne de commande actuels.
**Ergonomie de référence : JetBrains DataGrip**, en plus léger et plus rapide
(démarrage instantané, faible mémoire, grille virtualisée, aucune opération bloquante).
Simplifier l'installation et rendre les mises à jour automatiques via une page
de téléchargement GitHub Pages.

## Décisions

- **Un seul binaire, GUI par défaut.** Un seul fichier à télécharger et à mettre à jour.
- **CLI inchangée** dans ses capacités (pas de requêtes non interactives) : on ajoute
  seulement les sous-commandes `tui` et `update`.
- **GUI à parité fonctionnelle avec le TUI** dès cette version, avec en plus les
  comportements DataGrip retenus : éditeur de données inline, explorateur de base
  complet, console riche, plusieurs connexions ouvertes en même temps.
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

Sous Windows, le binaire reste en sous-système console (un binaire `windows` qui se
rattache à la console parente laisse le shell rendre la main et se disputer le
clavier avec le TUI). En mode GUI : lancé hors terminal (double-clic, menu
Démarrer), la console créée pour l'occasion est libérée (`FreeConsole`) ; lancé
depuis un terminal, le programme se relance détaché et rend la main au shell.

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
`rfd` (dialogues fichiers), `sqlformat` (formatage SQL).

### Disposition (inspirée de DataGrip)

```
┌────────────────┬──────────────────────────────────────────┬───────────┐
│ EXPLORATEUR 🔍 │ [● console prod] [● users] [● console dev]│ VALEUR    │
│ ● Prod PG      │ SELECT * FROM users WHERE …               │ {         │
│  ▾ public      │                                           │  "a": 1   │
│   ▾ users      ├───────────────────────────────────────────┤ }         │
│     ▸ colonnes │ [Résultat 1 📌] [Résultat 2] [Sortie]     │           │
│     ▸ clés     │ id │ name  │ email            ⟳  ✓ Submit │           │
│     ▸ index    │ 1  │ Alice*│ alice@…                       │           │
│ ○ Dev MySQL    │ + │ − │ WHERE [        ] ORDER BY [     ] │           │
└────────────────┴──────────────────────────────────────────┴───────────┘
 ● 2 connexions · 42 lignes · 12 ms            |< < page 1/9 > >|  v0.9.0
```

**Explorateur de base** (panneau gauche, filtre en haut)
- Arbre connexion → schémas → tables → colonnes / clés (PK, FK) / index, chargé
  paresseusement. Déplier une connexion la connecte.
- Chaque connexion a une couleur (choisie dans le formulaire), reprise par une
  pastille sur ses onglets, comme dans DataGrip, pour distinguer prod et dev.
- Double-clic sur une table : ouvre l'**éditeur de données** de la table dans un
  onglet. Menu contextuel : ouvrir les données, nouvelle console, DDL, structure,
  export / import / vidage (simple et par lot), rafraîchir, déconnecter,
  modifier / supprimer la connexion.
- `Ctrl+N` : recherche rapide d'une table dans toutes les connexions ouvertes,
  `Entrée` ouvre ses données.

**Onglets centraux** : des consoles et des éditeurs de données, chacun lié à une
connexion. Plusieurs connexions peuvent être ouvertes en même temps.

**Console**
- Éditeur SQL avec coloration, autocomplétion (tables et colonnes de sa connexion),
  formatage (`Ctrl+Alt+L`).
- `Ctrl+Entrée` exécute la sélection, sinon l'instruction (ou le bloc de transaction)
  au curseur ; `F5` exécute tout le script.
- **Un onglet de résultat par instruction** qui renvoie des lignes, plus un onglet
  « Sortie » qui journalise chaque exécution (durée, lignes affectées, erreurs).
  Les onglets de résultat sont remplacés à chaque exécution, sauf ceux épinglés.
- Les résultats sont limités à 1 000 lignes en console (lecture interrompue au-delà,
  signalée « 1 000+ lignes »), pour rester rapide sur de grosses tables.
- **Historique** des requêtes exécutées (`Ctrl+Alt+E`) : recherche, réinsertion
  dans la console ; persistant (500 dernières).

**Éditeur de données** (onglet d'une table)
- Grille paginée (500 lignes par page, navigation de page, total compté en arrière-plan).
- Barre `WHERE` / `ORDER BY` ; clic sur un en-tête de colonne pour trier.
- **Édition inline** : double-clic (ou `F2`) sur une cellule pour l'éditer dans la
  grille ; cellules modifiées surlignées, lignes ajoutées en vert, supprimées
  barrées en rouge. `Alt+Insert` ajoute une ligne, `Ctrl+Suppr` marque la ligne
  supprimée, « Mettre à NULL » dans le menu contextuel.
- Les changements restent **en attente** jusqu'à « Submit » (`Ctrl+Entrée` dans la
  grille) qui les applique dans une transaction unique, ou « Revert ».
- Les résultats de console issus d'une seule table avec clé primaire sont éditables
  de la même façon.

**Panneau Valeur** (droite, repliable) : affiche la cellule sélectionnée en entier,
JSON indenté si la valeur est du JSON ; éditable (alimente les changements en attente).

**DDL** : onglet en lecture seule avec le `CREATE TABLE` (natif pour SQLite et MySQL,
généré depuis les métadonnées pour PostgreSQL et SQL Server), copiable.

**Structure** : modale d'ajout / modification / renommage / suppression de colonne
(parité TUI).

**Barre de statut** : connexions ouvertes, durée et lignes de la dernière exécution,
erreurs en rouge, progression des imports/exports, version et mise à jour.

### Raccourcis

| Touche                | Action                                                   |
|-----------------------|----------------------------------------------------------|
| `Ctrl+Entrée`         | Console : exécuter la sélection / l'instruction au curseur ; grille : Submit |
| `F5`                  | Exécuter tout le script                                  |
| `Échap`               | Annuler la requête en cours / l'édition de cellule / fermer la modale |
| `Ctrl+Alt+L`          | Formater le SQL                                          |
| `Ctrl+Alt+E`          | Historique des requêtes                                  |
| `Ctrl+N`              | Rechercher une table                                     |
| `Ctrl+T` / `Ctrl+W`   | Nouvelle console (connexion courante) / fermer l'onglet  |
| `Ctrl+S`              | Enregistrer les consoles                                 |
| `Ctrl+Espace`         | Autocomplétion                                           |
| `F2` / double-clic    | Éditer la cellule                                        |
| `Alt+Insert` / `Ctrl+Suppr` | Ajouter / supprimer une ligne                      |
| `Ctrl+R`              | Rafraîchir (explorateur ou données)                      |

### Thème

Sombre et clair, suivant le thème système par défaut, choix mémorisé dans la
config. Palette inspirée de codingUnicorns. Pas de thèmes personnalisables.

### Concurrence

- Un runtime tokio multi-thread est créé au démarrage de la GUI.
- Chaque opération est `spawn`ée ; son résultat revient en `Event` (portant
  l'identifiant de la connexion et de l'onglet concernés) sur un channel drainé
  à chaque frame. Le worker appelle `ctx.request_repaint()` après envoi.
- L'UI ne bloque jamais. Une exécution en cours garde son `JoinHandle` par onglet ;
  `Échap` fait `abort()` dessus (pour un bloc de transaction, la connexion dédiée
  est relâchée sans `COMMIT`, donc le SGBD annule).
- Plusieurs connexions ouvertes simultanément, chacune partagée entre ses onglets
  (`Arc<DatabaseConnection>`).

### Erreurs

Toute erreur d'opération remonte en `Event` portant le message d'erreur : affichée
en rouge dans la barre de statut et dans l'onglet « Sortie » de la console concernée
(et dans le formulaire concerné pour la création de connexion). Aucune erreur n'est
avalée silencieusement.

### Ajouts au moteur pour la GUI

- Métadonnées : index et clés étrangères par SGBD, DDL (`engine::ops::schema`).
- Requêtes paginées / triées / filtrées par dialecte (`engine::sql::paging`).
- Exécution d'un script en plusieurs résultats, avec plafond de lignes (`engine::ops::query`).
- Application groupée de changements (UPDATE / INSERT / DELETE) en transaction
  (`engine::ops::rows::submit_changes`).
- Historique persistant (`engine::services::history`), formatage SQL (`engine::sql::format`).
- `ConnectionConfig.color` et `QueryTab.connection` (champs optionnels, ignorés par le TUI).

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
- Plan d'exécution (EXPLAIN visuel), diagrammes, comparaison de schémas.
