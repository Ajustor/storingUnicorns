# Dossiers de connexions et schémas masqués — spec et plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal :**
1. Ranger les connexions dans des **dossiers imbriqués** (ex. `Clients › ACME › Prod`)
   dans l'explorateur, avec création / renommage / suppression, déplacement par
   glisser-déposer ou « Déplacer vers… », et un champ « Dossier » dans le
   formulaire de connexion.
2. **Masquer des schémas** par connexion : ils disparaissent de l'explorateur,
   de la recherche de table (Ctrl+N) et de la complétion ; « Afficher les
   schémas masqués » les montre grisés pour les réafficher ; liste à cocher
   dans le formulaire.
3. GUI **et** TUI.

Branche : `feat/folders-hidden-schemas` depuis master (`c7c7474`).

## Conventions
- `RUSTC_WRAPPER="C:/Users/alexa/scoop/apps/sccache/current/sccache.exe" cargo test -q` (Bash). Base : 741 / 18 ; clippy 26.
- UI en français ; commentaires/commits en anglais terminés par une ligne vide
  puis `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. TDD.
- Compatibilité : un `config.toml` sans les nouveaux champs se charge à
  l'identique ; les nouveaux champs vides ne sont pas écrits.

## Contrat (moteur, `src/engine/config/` + `models/connection.rs`)

```rust
// ConnectionConfig
#[serde(default, skip_serializing_if = "Option::is_none")]
pub folder: Option<String>,            // "Clients/ACME/Prod" ; None = racine
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub hidden_schemas: Vec<String>,       // noms exacts tels que listés

// AppConfig
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub folders: Vec<String>,              // dossiers (même vides), chemins "A/B"

// engine/config/folders.rs (pur)
pub const SEP: char = '/';
/// "  a / b//c " -> "a/b/c" ; refuse vide, "." / "..", caractères de contrôle.
pub fn normalize(path: &str) -> Result<String, String>;
pub struct FolderNode { pub path: String, pub name: String,
                        pub folders: Vec<FolderNode>, pub connections: Vec<usize> }
/// Arbre trié (dossiers puis connexions, ordre alphabétique insensible à la
/// casse) à partir de `folders` + des `folder` des connexions (les ancêtres
/// manquants sont créés implicitement).
pub fn tree(config: &AppConfig) -> FolderNode;          // racine path ""
impl AppConfig {
    pub fn create_folder(&mut self, path: &str) -> Result<String, String>;
    /// Renomme / déplace un dossier et tout son contenu (connexions et
    /// sous-dossiers) ; refuse de le déplacer dans lui-même.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<(), String>;
    /// Supprime le dossier ; son contenu remonte d'un niveau.
    pub fn delete_folder(&mut self, path: &str) -> Result<(), String>;
    pub fn move_connection(&mut self, name: &str, folder: Option<&str>) -> Result<(), String>;
    pub fn set_schema_hidden(&mut self, connection: &str, schema: &str, hidden: bool);
}
impl ConnectionConfig { pub fn is_schema_hidden(&self, schema: &str) -> bool; }
```

## UX
- **GUI explorateur** : dossiers (icône dossier, repliables, état mémorisé
  comme les autres nœuds) contenant connexions et sous-dossiers ; menu
  contextuel d'un dossier : Nouvelle connexion ici, Nouveau sous-dossier,
  Renommer, Supprimer (contenu remonté) ; menu de la connexion : Déplacer
  vers… (liste des dossiers + « Racine » + « Nouveau dossier… ») ;
  glisser-déposer d'une connexion ou d'un dossier sur un dossier ou la racine
  (indicateur de dépôt). Bouton « Nouveau dossier » à côté de « + ». Le filtre
  de l'explorateur garde les dossiers qui contiennent un résultat.
- **Schémas masqués** : menu d'un schéma « Masquer le schéma » ; menu de la
  connexion « Afficher les schémas masqués » (bascule ; les masqués
  apparaissent grisés avec « Réafficher ») ; formulaire de connexion : section
  « Schémas affichés » (cases à cocher, chargées si la connexion est ouverte,
  sinon liste des noms masqués avec « Réafficher »). Les masqués sont exclus
  de Ctrl+N et de la complétion (tables proposées) ; une table d'un schéma
  masqué reste utilisable si on l'écrit.
- **TUI** : liste des connexions groupée par dossier (arborescence avec
  indentation, repli par Entrée / ←/→), touche pour déplacer (`m`) vers un
  dossier saisi, création de dossier ; schémas masqués cachés de la liste des
  tables et de la complétion, touche `h` pour masquer / réafficher le schéma
  courant, et une bascule pour voir les masqués.

### Task 1 (moteur) : contrat + tests (normalisation, arbre, renommage avec
contenu, suppression qui remonte, déplacement, refus de cycles, sérialisation
compatible, `is_schema_hidden`). Commit `feat(engine): connection folders and hidden schemas in the config`.
### Task 2 (GUI) : explorateur, menus, glisser-déposer, formulaire, filtres
Ctrl+N / complétion. Commit `feat(gui): connection folders and hidden schemas`.
### Task 3 (TUI) : arborescence, déplacement, masquage, filtres. Commit `feat(tui): connection folders and hidden schemas`.
### Task 4 : README + CHANGELOG `## [Non publié]`.
