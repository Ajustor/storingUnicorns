# Navigation par clés étrangères (GUI) — spec et plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal :** voir les liens d'une table (FK sortantes et entrantes) dans sa
structure, et depuis une cellule ou une ligne de données, récupérer l'élément
lié : aperçu dans le panneau de valeur (avec ses propres FK cliquables pour
continuer), ou ouverture d'un onglet de données filtré.

**Existant :** `ForeignKeyInfo { name, columns, ref_table, ref_columns }`
(`engine/models/connection.rs`), `DatabaseConnection::get_foreign_keys(table)`
pour les 5 moteurs, `TableDetails` (colonnes / index / FK, chargés par
l'explorateur), dialogue Structure (`gui/dialogs/structure.rs`, modale
standard), grille (`gui/grid/`), panneau de valeur (`gui/value_panel.rs`),
onglets de données (`gui/tabs/data.rs`, `DataQuery` / `build_select` dans
`engine/sql/paging.rs`), littéraux SQL (`engine::db::utils::sql_literal`),
`App::open_data(connection, table, title)`.

Branche : `feat/fk-navigation` depuis master (0.17.0). GUI seulement.

## Conventions
- `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q` (Bash). Base : 706 / 12 ; clippy 26.
- UI en français ; commentaires/commits en anglais terminés par une ligne vide
  puis `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. TDD.

## Contrat (moteur)

```rust
// engine/models/connection.rs
/// A foreign key of another table that points at this one.
pub struct IncomingForeignKey {
    pub name: String,
    pub table: String,          // referencing table, qualified like get_tables ("schema.table")
    pub columns: Vec<String>,   // its columns, in key order
    pub ref_columns: Vec<String>, // this table's columns, matching one to one
}
// DatabaseConnection
pub async fn get_incoming_foreign_keys(&self, table: &str) -> Result<Vec<IncomingForeignKey>>;
// TableDetails gains: pub incoming: Vec<IncomingForeignKey>, pub incoming_error: Option<String>
//   (loaded with the other details, degraded like indexes / FKs)

// engine/sql/relations.rs (pure)
/// The `WHERE` condition (without the keyword) selecting the rows of
/// `target_columns` equal to `values` (one per column; NULL_CELL → `IS NULL`),
/// quoted for the engine, literals typed from `target_types` when known.
pub fn match_condition(target_columns: &[String], values: &[String],
                       target_types: &[Option<String>], db: &DatabaseType) -> String;
/// For an outgoing FK and the current row (column name → value): the
/// referenced table and the condition selecting the referenced row.
/// None when a referencing value is NULL (no linked row).
pub fn follow(fk: &ForeignKeyInfo, row: &dyn Fn(&str) -> Option<String>,
              db: &DatabaseType) -> Option<(String /* ref_table */, String /* condition */)>;
/// For an incoming FK and the current row: the referencing table and the
/// condition selecting the rows that point at it.
pub fn referencing(fk: &IncomingForeignKey, row: &dyn Fn(&str) -> Option<String>,
                   db: &DatabaseType) -> Option<(String, String)>;
```
Requêtes « entrantes » par moteur : PostgreSQL (`pg_constraint` contype 'f'
avec `confrelid` = la table), MySQL (`information_schema.KEY_COLUMN_USAGE` où
`REFERENCED_TABLE_*` = la table), SQLite (parcourir `pragma_foreign_key_list`
de chaque table), SQL Server / Azure (`sys.foreign_keys` + `sys.foreign_key_columns`
où `referenced_object_id` = la table). Ordre des colonnes = ordre de la clé.

## UX (GUI)
- **Structure** : colonne « Lien » — `→ table.colonne` pour une colonne FK
  (clic : ouvre la structure de la table cible) ; section « Référencée par » :
  `table (colonnes) → colonnes` pour chaque FK entrante, clic : ouvre la
  structure de cette table. Les FK composées affichées une fois, groupées.
- **Grille** : icône de lien discrète dans les en-têtes et cellules des
  colonnes FK (sortantes). Clic sur l'icône d'une cellule (ou Ctrl+Entrée sur
  la cellule) → aperçu de la ligne liée dans le panneau de valeur ;
  Ctrl+clic → onglet de données de la table cible filtré (`WHERE` = condition
  de `follow`). Valeur NULL : pas d'icône.
- **Panneau de valeur — aperçu** : titre `table liée` + chemin de navigation
  (fil d'Ariane, retour arrière), toutes les colonnes de la ligne liée
  (nom, type, valeur ; NULL en italique), ses propres colonnes FK cliquables
  pour continuer, boutons « Ouvrir dans un onglet » et « Retour ». Chargement
  asynchrone par le worker (`SELECT * … WHERE <condition>` limité à 2 lignes ;
  0 → « Aucune ligne liée (clé orpheline) », >1 → « Plusieurs lignes
  correspondent » + la première).
- **Liens entrants** : menu contextuel d'une ligne « Enregistrements liés » →
  sous-menu par FK entrante `table (colonnes)` → onglet filtré (condition de
  `referencing`). Le panneau de valeur liste aussi ces liens sous l'aperçu.
- Lecture seule : l'aperçu ne modifie rien ; l'onglet ouvert est un onglet de
  données normal (éditable si la table l'est).

### Task 1 (moteur)
- [ ] `IncomingForeignKey`, `get_incoming_foreign_keys` pour les 5 moteurs,
  `TableDetails.incoming` (+ dégradation), `engine/sql/relations.rs` selon le
  contrat. Tests unitaires (conditions : simple, composée, NULL, quoting par
  moteur, littéraux typés, valeurs avec quotes) ; tests d'intégration
  `#[ignore]` PG / MySQL / MSSQL / SQLite pour les FK entrantes (simple +
  composée, plusieurs schémas). Commit `feat(engine): incoming foreign keys and row-matching conditions`.

### Task 2 (GUI)
- [ ] Structure, grille, panneau de valeur, menu contextuel, ouverture
  d'onglet filtré, worker (chargement de la ligne liée). Tests sur la logique
  (aperçu : 0 / 1 / >1 lignes, fil d'Ariane, condition passée à l'onglet).
  Commit `feat(gui): follow foreign keys from the structure, the grid and the value panel`.

### Task 3 : docs (README, CHANGELOG `## [Non publié]`).
