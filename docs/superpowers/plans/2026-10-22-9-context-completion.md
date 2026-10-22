# Complétion contextuelle des colonnes — spec et plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** quand une requête cible une ou plusieurs tables, proposer leurs
colonnes selon la position du curseur, dans la console GUI, les barres WHERE /
ORDER BY de la visualisation de table, et l'éditeur du TUI.

**Aujourd'hui :** `engine::sql::lexer::get_completions(query, cursor,
known_columns, known_tables)` décide table / colonne par « dernier mot-clé »,
et les colonnes viennent des **derniers résultats** (`known_columns`), pas des
tables de la requête. GUI : `gui/editor/completion.rs` (popup), console
`gui/tabs/console/mod.rs`. Visualisation : `gui/tabs/data.rs` (`filter_bar`,
champs `filter_input` / `order_input`), sans complétion. TUI : état
`completion_*` dans `tui/app_state.rs` + cache de colonnes. Cache de colonnes
moteur : `engine/services/table_cache.rs` (utilisé par l'explorateur).

Branche : `feat/context-completion` depuis master (beaf3fa).

## Conventions
- `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q` (Bash). Base : 576 réussis / 12 ignorés ; clippy 27.
- UI en français ; commentaires/commits en anglais terminés par une ligne vide
  puis `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. TDD, un
  commit par tâche ; ne formater que son propre code dans les fichiers qui ne
  sont pas déjà propres.

## Contrat (moteur `src/engine/sql/completion.rs`, pur)

```rust
pub struct TableRef { pub schema: Option<String>, pub name: String, pub alias: Option<String> }
pub struct ColumnInfo { pub name: String, pub type_name: String, pub primary_key: bool }
pub enum Kind { Column, Table, Schema, Keyword }
pub struct Suggestion {
    pub label: String,   // shown, e.g. "email"
    pub detail: String,  // shown dimmed, e.g. "varchar(255)" / "PK int" / "table"
    pub insert: String,  // inserted text, quoted for the engine when needed
    pub kind: Kind,
}
pub struct Context<'a> {
    pub quotes: (char, char),                       // engine quoting
    pub tables: &'a [String],                       // known tables ("schema.table")
    pub columns: &'a dyn Fn(&TableRef) -> Option<Vec<ColumnInfo>>, // None = not loaded yet
    pub fallback_columns: &'a [String],             // last results (old behaviour)
}
pub struct Completions { pub items: Vec<Suggestion>, pub missing: Vec<TableRef> } // missing = to load
/// Statement under the cursor (scripts split like the console does).
pub fn complete(text: &str, cursor: usize, cx: &Context) -> Completions;
/// Tables referenced by the statement at `cursor` (FROM, JOIN, UPDATE,
/// INSERT INTO, DELETE FROM; commas; optional AS; quoted / qualified names;
/// only the nesting level of the cursor for subqueries).
pub fn tables_in_scope(text: &str, cursor: usize) -> Vec<TableRef>;
/// For a single-table context (data view filter / order bar).
pub fn complete_for_table(fragment: &str, cursor: usize, mode: BarMode, table: &TableRef, cx: &Context) -> Completions;
pub enum BarMode { Where, OrderBy }
```

Règles (spec) :

| Curseur | Suggestions |
|---|---|
| après `alias.` / `table.` | colonnes de cette table seulement |
| `SELECT`, `WHERE`, `ON`, `GROUP BY`, `ORDER BY`, `HAVING`, `SET`, `AND`/`OR` | colonnes des tables en portée (préfixe `alias.` si le nom est ambigu entre tables), puis mots-clés |
| `INSERT INTO t (` | colonnes de `t` |
| `FROM`, `JOIN`, `UPDATE`, `INTO` | tables ; `schema.` filtre |
| aucune table citée | ancien comportement (`fallback_columns`, mots-clés) |
| barre WHERE | colonnes de la table + `AND OR NOT IS NULL IS NOT NULL LIKE IN BETWEEN` |
| barre ORDER BY | colonnes + `ASC DESC NULLS FIRST NULLS LAST` |

Filtre préfixe insensible à la casse, colonnes avant mots-clés, PK signalée.
Insertion entre guillemets du moteur si le nom l'exige (mot réservé, majuscules
sur PostgreSQL, espaces…) ; réutiliser `engine::db::utils::quote_ident` si
pertinent. Hors périmètre : fonctions, colonnes de CTE / sous-requêtes dérivées.

---

### Task 1 (moteur) : `engine/sql/completion.rs`
- [ ] Contrat + tests : alias (`FROM users u` / `AS u`), jointures multiples,
  virgules, `schema.table`, noms entre guillemets (`"…"`, `` `…` ``, `[…]`),
  sous-requête (seules les tables du niveau du curseur), CTE simple (la CTE
  elle-même n'apporte pas de colonnes mais ne casse rien), script à plusieurs
  instructions (seule l'instruction courante), chaque ligne du tableau, barres
  WHERE / ORDER BY, insertion quotée par moteur, `missing` rempli quand
  `columns` renvoie `None`.
- [ ] Commit `feat(engine): context-aware SQL completion of the tables in scope`.

### Task 2 (GUI) : console + visualisation
- [ ] `gui/editor/completion.rs` : utiliser `complete`; popup avec libellé +
  détail (faible) ; **largeur adaptée au contenu** (au moins la largeur du plus
  long libellé + détail, plafonnée à ~60 % de la fenêtre), plus de libellés
  tronqués. Colonnes manquantes : demander leur chargement au worker (même
  mécanisme que l'explorateur, `TableCache`), et rafraîchir le popup à
  l'arrivée si toujours ouvert.
- [ ] `gui/tabs/data.rs` : complétion dans WHERE et ORDER BY (même popup,
  `complete_for_table` avec la table affichée) ; touches identiques à la console.
- [ ] Tests sur l'état (popup, insertion, rafraîchissement après chargement).
- [ ] Commit `feat(gui): context-aware column completion in consoles and the table view`.

### Task 3 (TUI)
- [ ] Brancher l'éditeur du TUI sur `complete` ; afficher le type ; chargement
  des colonnes manquantes via le cache existant du TUI.
- [ ] Commit `feat(tui): context-aware column completion`.

### Task 4 : docs
- [ ] README (complétion contextuelle), CHANGELOG `## [Non publié]`.
