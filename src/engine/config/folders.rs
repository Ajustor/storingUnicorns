//! Connection folders: pure helpers over `AppConfig.folders` and the
//! connections' `folder` field.
//!
//! A folder is a `/`-separated path such as `"Clients/ACME/Prod"`. Only `/`
//! separates segments: `\` is an ordinary character. A folder exists when it
//! is listed in `AppConfig.folders` (created explicitly, kept even when
//! empty), when a connection lives in it, or when it is an ancestor of one of
//! those (implicit: it vanishes once nothing is left under it).
#![allow(dead_code)] // TODO(folders): used by the GUI/TUI tasks

use std::collections::BTreeSet;

use super::AppConfig;
use crate::engine::models::ConnectionConfig;

/// Separator between the segments of a folder path.
pub const SEP: char = '/';

/// Most segments a folder path may have.
pub const MAX_DEPTH: usize = 32;
/// Most characters (not bytes) a folder name may have.
pub const MAX_NAME_CHARS: usize = 128;

/// Unicode format (Cf), line (Zl) and paragraph (Zp) separator characters:
/// invisible, and the bidi controls among them can disguise a name.
fn is_format_or_separator(c: char) -> bool {
    matches!(
        c,
        '\u{AD}'
            | '\u{600}'..='\u{605}'
            | '\u{61C}'
            | '\u{6DD}'
            | '\u{70F}'
            | '\u{890}'..='\u{891}'
            | '\u{8E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Whether `c` shows something: not whitespace, nor one of the characters
/// rendered as blank (Hangul fillers, blank Braille pattern, combining
/// grapheme joiner).
fn is_visible(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '\u{34F}' | '\u{115F}' | '\u{1160}' | '\u{2800}' | '\u{3164}' | '\u{FFA0}'
        )
}

/// The checked segments of `path` (NFC, trimmed, empty ones dropped); an
/// empty list is the root.
fn segments(path: &str) -> Result<Vec<String>, String> {
    use unicode_normalization::UnicodeNormalization;
    if path.chars().any(char::is_control) {
        return Err("Le nom du dossier contient un caractère de contrôle".into());
    }
    if path.chars().any(is_format_or_separator) {
        return Err("Le nom du dossier contient un caractère invisible".into());
    }
    let nfc: String = path.nfc().collect();
    let segments: Vec<String> = nfc
        .split(SEP)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if segments.len() > MAX_DEPTH {
        return Err(format!(
            "Un dossier ne peut pas être imbriqué sur plus de {MAX_DEPTH} niveaux"
        ));
    }
    for s in &segments {
        if s == "." || s == ".." {
            return Err("« . » et « .. » ne sont pas des noms de dossier valides".into());
        }
        if !s.chars().any(is_visible) {
            return Err("Le nom du dossier doit contenir un caractère visible".into());
        }
        if s.chars().count() > MAX_NAME_CHARS {
            return Err(format!(
                "Un nom de dossier ne peut pas dépasser {MAX_NAME_CHARS} caractères"
            ));
        }
    }
    Ok(segments)
}

/// Normalize a folder path: NFC, segments trimmed, empty ones dropped
/// (`"  a / b//c "` -> `"a/b/c"`). Refuses an empty path (see `is_root`),
/// `.` / `..` segments, control, format and separator characters, a segment
/// without a visible character, more than `MAX_DEPTH` segments and a
/// segment longer than `MAX_NAME_CHARS` characters.
pub fn normalize(path: &str) -> Result<String, String> {
    let segments = segments(path)?;
    if segments.is_empty() {
        return Err("Le nom du dossier est vide".into());
    }
    Ok(segments.join(&SEP.to_string()))
}

/// Whether `path` designates the root, i.e. normalizes to nothing (`""`,
/// `"/"`, `" // "`). The root is not a folder.
pub fn is_root(path: &str) -> bool {
    segments(path).is_ok_and(|s| s.is_empty())
}

/// Refuse `path` when it, or one of its ancestors spelled differently,
/// matches an existing folder case-insensitively (`except` aside, so a
/// case-only rename works). Ancestors spelled exactly alike are fine.
fn check_collision(
    existing: &BTreeSet<String>,
    path: &str,
    except: Option<&str>,
) -> Result<(), String> {
    let mut prefix = path;
    while !prefix.is_empty() {
        let lower = prefix.to_lowercase();
        let clash = existing.iter().find(|e| {
            Some(e.as_str()) != except
                && e.to_lowercase() == lower
                && (prefix == path || e.as_str() != prefix)
        });
        if let Some(e) = clash {
            return Err(format!("Le dossier « {e} » existe déjà"));
        }
        prefix = parent(prefix);
    }
    Ok(())
}

/// The parent of a normalized path, `""` for a top-level folder.
pub fn parent(path: &str) -> &str {
    path.rsplit_once(SEP).map_or("", |(p, _)| p)
}

/// The last segment of a normalized path.
pub fn name(path: &str) -> &str {
    path.rsplit_once(SEP).map_or(path, |(_, n)| n)
}

/// Whether `path` is `ancestor` itself or lies under it (both normalized;
/// the root `""` contains everything).
pub fn is_within(path: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with(SEP))
}

/// `child` appended to `base` (`base` may be the root `""`).
fn join(base: &str, child: &str) -> String {
    if base.is_empty() {
        child.to_string()
    } else {
        format!("{base}{SEP}{child}")
    }
}

/// A folder of the explorer tree. The root has an empty `path` and `name`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderNode {
    pub path: String,
    pub name: String,
    /// Subfolders, sorted by name (case-insensitive).
    pub folders: Vec<FolderNode>,
    /// Indices into `AppConfig.connections`, sorted by connection name
    /// (case-insensitive).
    pub connections: Vec<usize>,
}

/// Case-insensitive order, then exact order so the sort is total.
fn name_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// The folder a connection lives in, normalized (`""` = root; an invalid
/// stored path counts as the root).
fn connection_folder(conn: &ConnectionConfig) -> String {
    conn.folder
        .as_deref()
        .and_then(|f| normalize(f).ok())
        .unwrap_or_default()
}

/// Every existing folder path: explicit ones, the connections' and all
/// their ancestors.
fn all_paths(config: &AppConfig) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let leaves = config
        .folders
        .iter()
        .filter_map(|f| normalize(f).ok())
        .chain(config.connections.iter().map(connection_folder));
    for leaf in leaves {
        let mut p = leaf.as_str();
        while !p.is_empty() && paths.insert(p.to_string()) {
            p = parent(p);
        }
    }
    paths
}

/// The explorer tree: folders first then connections, each sorted
/// alphabetically (case-insensitive). Built from `folders` and the
/// connections' `folder`; missing ancestors are created implicitly.
pub fn tree(config: &AppConfig) -> FolderNode {
    let paths = all_paths(config);
    let homes: Vec<String> = config.connections.iter().map(connection_folder).collect();
    build(config, &paths, &homes, String::new())
}

fn build(
    config: &AppConfig,
    paths: &BTreeSet<String>,
    homes: &[String],
    path: String,
) -> FolderNode {
    let mut folders: Vec<FolderNode> = paths
        .iter()
        .filter(|p| !p.is_empty() && parent(p) == path)
        .map(|p| build(config, paths, homes, p.clone()))
        .collect();
    folders.sort_by(|a, b| name_order(&a.name, &b.name));
    let mut connections: Vec<usize> = (0..config.connections.len())
        .filter(|&i| homes[i] == path)
        .collect();
    connections.sort_by(|&a, &b| {
        name_order(&config.connections[a].name, &config.connections[b].name).then_with(|| a.cmp(&b))
    });
    FolderNode {
        name: name(&path).to_string(),
        path,
        folders,
        connections,
    }
}

impl AppConfig {
    /// Whether the folder exists, explicitly or implicitly. False for the
    /// root (`""`, `"/"`), which is not a folder.
    pub fn folder_exists(&self, path: &str) -> bool {
        normalize(path).is_ok_and(|p| all_paths(self).contains(&p))
    }

    /// Create a folder (and its missing ancestors, so they outlive their
    /// content too). Returns the normalized path; refuses an existing one.
    pub fn create_folder(&mut self, path: &str) -> Result<String, String> {
        let path = normalize(path)?;
        check_collision(&all_paths(self), &path, None)?;
        let explicit: BTreeSet<String> = self
            .folders
            .iter()
            .filter_map(|f| normalize(f).ok())
            .collect();
        let mut missing = Vec::new();
        let mut p = path.as_str();
        while !p.is_empty() && !explicit.contains(p) {
            missing.push(p.to_string());
            p = parent(p);
        }
        self.folders.extend(missing.into_iter().rev());
        Ok(path)
    }

    /// Rename / move a folder with all its content (connections and
    /// subfolders). Refuses moving it into itself or a descendant, a
    /// destination colliding case-insensitively with another folder (no
    /// merge; a case-only rename is fine) and nesting deeper than
    /// `MAX_DEPTH`.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<(), String> {
        let from = normalize(from)?;
        let to = normalize(to)?;
        let paths = all_paths(self);
        if !paths.contains(&from) {
            return Err(format!("Le dossier « {from} » n'existe pas"));
        }
        if from == to {
            return Ok(());
        }
        if is_within(&to, &from) {
            return Err("Un dossier ne peut pas être déplacé dans lui-même".into());
        }
        check_collision(&paths, &to, Some(&from))?;
        let depth = |p: &str| p.split(SEP).count();
        let deepest = paths
            .iter()
            .filter(|p| is_within(p, &from))
            .map(|p| depth(p) - depth(&from) + depth(&to))
            .max()
            .unwrap_or(0);
        if deepest > MAX_DEPTH {
            return Err(format!(
                "Un dossier ne peut pas être imbriqué sur plus de {MAX_DEPTH} niveaux"
            ));
        }
        self.rewrite_folders(|p| is_within(p, &from).then(|| format!("{to}{}", &p[from.len()..])));
        Ok(())
    }

    /// Delete a folder: its direct children (connections and subfolders,
    /// with their content) move up to its parent. A subfolder whose name is
    /// already taken in the parent merges into it.
    pub fn delete_folder(&mut self, path: &str) -> Result<(), String> {
        let path = normalize(path)?;
        if !all_paths(self).contains(&path) {
            return Err(format!("Le dossier « {path} » n'existe pas"));
        }
        let up = parent(&path).to_string();
        self.rewrite_folders(|p| {
            if p == path {
                Some(up.clone())
            } else if is_within(p, &path) {
                Some(join(&up, &p[path.len() + 1..]))
            } else {
                None
            }
        });
        Ok(())
    }

    /// Move a connection into `folder` (`None`, or anything normalizing to
    /// nothing such as `""` or `"/"`, = root), which needn't exist yet.
    pub fn move_connection(&mut self, name: &str, folder: Option<&str>) -> Result<(), String> {
        let folder = match folder {
            Some(f) if !is_root(f) => Some(normalize(f)?),
            _ => None,
        };
        let conn = self
            .connections
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("La connexion « {name} » n'existe pas"))?;
        conn.folder = folder;
        Ok(())
    }

    /// Hide or show a schema of a connection; idempotent, and a no-op for
    /// an unknown connection.
    pub fn set_schema_hidden(&mut self, connection: &str, schema: &str, hidden: bool) {
        let Some(conn) = self.connections.iter_mut().find(|c| c.name == connection) else {
            return;
        };
        if !hidden {
            conn.hidden_schemas.retain(|s| s != schema);
        } else if !conn.is_schema_hidden(schema) {
            conn.hidden_schemas.push(schema.to_string());
        }
    }

    /// Apply `map` (normalized path -> new path, `""` = root, `None` =
    /// unchanged) to the explicit folders and the connections' folders.
    /// Explicit entries mapped to the root are dropped, duplicates removed.
    fn rewrite_folders(&mut self, map: impl Fn(&str) -> Option<String>) {
        let mut seen = BTreeSet::new();
        let folders = std::mem::take(&mut self.folders);
        for f in folders {
            let Ok(norm) = normalize(&f) else {
                continue;
            };
            let new = map(&norm).unwrap_or(norm);
            if !new.is_empty() && seen.insert(new.clone()) {
                self.folders.push(new);
            }
        }
        for conn in &mut self.connections {
            let home = connection_folder(conn);
            if home.is_empty() {
                continue;
            }
            if let Some(new) = map(&home) {
                conn.folder = (!new.is_empty()).then_some(new);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(name: &str, folder: Option<&str>) -> ConnectionConfig {
        ConnectionConfig {
            name: name.into(),
            folder: folder.map(Into::into),
            ..Default::default()
        }
    }

    fn config(folders: &[&str], conns: &[(&str, Option<&str>)]) -> AppConfig {
        AppConfig {
            folders: folders.iter().map(|f| f.to_string()).collect(),
            connections: conns.iter().map(|(n, f)| conn(n, *f)).collect(),
            ..Default::default()
        }
    }

    fn folder_of<'a>(c: &'a AppConfig, name: &str) -> Option<&'a str> {
        c.get_connection(name).unwrap().folder.as_deref()
    }

    /// The tree as `path: [connections]` lines, depth first.
    fn outline(c: &AppConfig) -> Vec<String> {
        fn walk(c: &AppConfig, n: &FolderNode, out: &mut Vec<String>) {
            let names: Vec<&str> = n
                .connections
                .iter()
                .map(|&i| c.connections[i].name.as_str())
                .collect();
            out.push(format!("{}: {}", n.path, names.join(",")));
            for f in &n.folders {
                walk(c, f, out);
            }
        }
        let mut out = Vec::new();
        walk(c, &tree(c), &mut out);
        out
    }

    #[test]
    fn normalize_trims_segments_and_drops_empty_ones() {
        assert_eq!(normalize("  a / b//c ").unwrap(), "a/b/c");
        assert_eq!(normalize("/a/").unwrap(), "a");
        assert_eq!(normalize("Clients/ACME/Prod").unwrap(), "Clients/ACME/Prod");
        assert_eq!(
            normalize("Mon dossier").unwrap(),
            "Mon dossier",
            "inner spaces kept"
        );
    }

    #[test]
    fn normalize_refuses_empty_dots_and_control_characters() {
        for bad in [
            "", "   ", "/", " // / ", ".", "a/../b", "a/./b", " .. ", "a\tb", "a\nb", "\u{7f}",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            normalize("...").unwrap(),
            "...",
            "only . and .. are special"
        );
        assert_eq!(normalize(".hidden").unwrap(), ".hidden");
    }

    #[test]
    fn normalize_keeps_unicode_and_backslashes() {
        assert_eq!(normalize(" Équipe / 日本 / 🦄 ").unwrap(), "Équipe/日本/🦄");
        assert_eq!(
            normalize(r"a\b / c").unwrap(),
            r"a\b/c",
            "\\ is not a separator"
        );
        // Unicode whitespace is trimmed like ASCII spaces.
        assert_eq!(normalize("\u{a0}a\u{2003}/b").unwrap(), "a/b");
    }

    #[test]
    fn path_helpers() {
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("a"), "");
        assert_eq!(name("a/b/c"), "c");
        assert_eq!(name("a"), "a");
        assert!(is_within("a/b", "a"));
        assert!(is_within("a", "a"));
        assert!(is_within("a", ""));
        assert!(
            !is_within("ab", "a"),
            "prefix of a segment is not an ancestor"
        );
        assert!(!is_within("a", "a/b"));
    }

    #[test]
    fn tree_sorts_folders_then_connections_case_insensitively() {
        let c = config(
            &["beta", "Alpha", "alpha2"],
            &[
                ("zeta", None),
                ("Bravo", None),
                ("alpha", None),
                ("x", Some("beta")),
            ],
        );
        let root = tree(&c);
        assert_eq!(root.path, "");
        assert_eq!(root.name, "");
        let folders: Vec<&str> = root.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(folders, ["Alpha", "alpha2", "beta"]);
        assert_eq!(
            outline(&c),
            [": alpha,Bravo,zeta", "Alpha: ", "alpha2: ", "beta: x"]
        );
        assert_eq!(root.connections, [2, 1, 0], "indices into connections");
    }

    #[test]
    fn tree_creates_missing_ancestors_implicitly() {
        let c = config(
            &["X/Y"],
            &[
                ("prod", Some("Clients/ACME/Prod")),
                ("dev", Some(" Clients / ACME ")),
            ],
        );
        assert_eq!(
            outline(&c),
            [
                ": ",
                "Clients: ",
                "Clients/ACME: dev",
                "Clients/ACME/Prod: prod",
                "X: ",
                "X/Y: ",
            ]
        );
        let acme = &tree(&c).folders[0].folders[0];
        assert_eq!(
            (acme.path.as_str(), acme.name.as_str()),
            ("Clients/ACME", "ACME")
        );
    }

    #[test]
    fn tree_puts_invalid_stored_folders_at_the_root() {
        let c = config(&["", "a/../b"], &[("c", Some("..")), ("d", Some("  "))]);
        assert_eq!(outline(&c), [": c,d"]);
    }

    #[test]
    fn create_folder_returns_the_normalized_path_and_refuses_existing() {
        let mut c = config(&[], &[("prod", Some("Live"))]);
        assert_eq!(c.create_folder(" A / B ").unwrap(), "A/B");
        assert_eq!(c.folders, ["A", "A/B"], "ancestors created explicitly");
        assert!(c.create_folder("A/B").is_err());
        assert!(c.create_folder("A").is_err());
        assert!(
            c.create_folder("Live").is_err(),
            "implicit folders exist too"
        );
        assert!(c.create_folder("..").is_err());
        assert_eq!(c.create_folder("A/C").unwrap(), "A/C");
        assert_eq!(c.folders, ["A", "A/B", "A/C"]);
        assert!(c.folder_exists("Live") && c.folder_exists(" A/C") && !c.folder_exists("Z"));
    }

    #[test]
    fn rename_folder_moves_descendants_and_connections() {
        let mut c = config(
            &["A", "A/B", "A/B/C", "AB", "Other"],
            &[
                ("a", Some("A")),
                ("ab", Some("A/B/C")),
                ("x", Some("AB")),
                ("r", None),
            ],
        );
        c.rename_folder("A", "Z/A2").unwrap();
        assert_eq!(c.folders, ["Z/A2", "Z/A2/B", "Z/A2/B/C", "AB", "Other"]);
        assert_eq!(folder_of(&c, "a"), Some("Z/A2"));
        assert_eq!(folder_of(&c, "ab"), Some("Z/A2/B/C"));
        assert_eq!(folder_of(&c, "x"), Some("AB"), "AB is not under A");
        assert_eq!(folder_of(&c, "r"), None);
    }

    #[test]
    fn rename_folder_works_on_implicit_folders() {
        let mut c = config(&[], &[("p", Some("A/B/C"))]);
        c.rename_folder("A/B", "Q").unwrap();
        assert_eq!(folder_of(&c, "p"), Some("Q/C"));
        assert!(c.folders.is_empty());
        assert_eq!(
            outline(&c),
            [": ", "Q: ", "Q/C: p"],
            "A vanished, it was implicit"
        );
    }

    #[test]
    fn rename_folder_refuses_cycles_collisions_and_unknown_folders() {
        let mut c = config(&["A", "A/B", "C"], &[("p", Some("A/B"))]);
        let before = c.clone();
        assert!(c.rename_folder("A", "A/B/A").is_err(), "into a descendant");
        assert!(c.rename_folder("A", "A/X").is_err(), "into itself");
        assert!(c.rename_folder("A", "C").is_err(), "name taken");
        assert!(c.rename_folder("A/B", "A").is_err(), "onto its parent");
        assert!(c.rename_folder("Nope", "D").is_err());
        assert!(c.rename_folder("A", "  ").is_err());
        assert_eq!(c.folders, before.folders);
        assert_eq!(c.connections, before.connections);
        c.rename_folder(" A ", "A").unwrap();
        assert_eq!(c.folders, before.folders, "same name: no-op");
        c.rename_folder("A", "a").unwrap();
        assert_eq!(c.folders, ["a", "a/B", "C"], "case-only rename allowed");
    }

    #[test]
    fn rename_folder_into_a_sibling_moves_it() {
        let mut c = config(&["A", "B"], &[("p", Some("A"))]);
        c.rename_folder("A", "B/A").unwrap();
        assert_eq!(outline(&c), [": ", "B: ", "B/A: p"]);
    }

    #[test]
    fn delete_folder_hoists_direct_children_to_the_parent() {
        let mut c = config(
            &["P", "P/D", "P/D/Sub", "P/D/Sub/Deep", "P/Keep"],
            &[
                ("in", Some("P/D")),
                ("deep", Some("P/D/Sub/Deep")),
                ("k", Some("P/Keep")),
            ],
        );
        c.delete_folder("P/D").unwrap();
        assert_eq!(c.folders, ["P", "P/Sub", "P/Sub/Deep", "P/Keep"]);
        assert_eq!(folder_of(&c, "in"), Some("P"));
        assert_eq!(folder_of(&c, "deep"), Some("P/Sub/Deep"));
        assert_eq!(folder_of(&c, "k"), Some("P/Keep"));
    }

    #[test]
    fn delete_top_level_folder_hoists_to_the_root_and_merges() {
        let mut c = config(&["A", "A/X", "X"], &[("a", Some("A")), ("ax", Some("A/X"))]);
        c.delete_folder("A").unwrap();
        assert_eq!(c.folders, ["X"], "A/X merged into X");
        assert_eq!(folder_of(&c, "a"), None);
        assert_eq!(folder_of(&c, "ax"), Some("X"));
        assert!(c.delete_folder("A").is_err(), "gone");
        assert!(c.delete_folder("").is_err());
    }

    #[test]
    fn delete_implicit_folder() {
        let mut c = config(&[], &[("p", Some("A/B"))]);
        c.delete_folder("A").unwrap();
        assert_eq!(folder_of(&c, "p"), Some("B"));
    }

    #[test]
    fn move_connection_sets_normalized_folder() {
        let mut c = config(&["Keep"], &[("p", None), ("q", Some("Keep"))]);
        c.move_connection("p", Some(" New / Sub ")).unwrap();
        assert_eq!(folder_of(&c, "p"), Some("New/Sub"));
        assert_eq!(outline(&c), [": ", "Keep: q", "New: ", "New/Sub: p"]);
        c.move_connection("p", None).unwrap();
        assert_eq!(folder_of(&c, "p"), None);
        assert_eq!(outline(&c), [": p", "Keep: q"], "implicit New vanished");
        c.move_connection("q", Some("  ")).unwrap();
        assert_eq!(folder_of(&c, "q"), None, "blank = root");
        assert_eq!(
            outline(&c),
            [": p,q", "Keep: "],
            "explicit folder kept empty"
        );
        assert!(c.move_connection("p", Some("a/../b")).is_err());
        assert_eq!(folder_of(&c, "p"), None, "unchanged on error");
        assert!(c.move_connection("missing", None).is_err());
    }

    #[test]
    fn removing_a_connection_keeps_only_explicit_folders() {
        let mut c = config(
            &["Explicit"],
            &[("a", Some("Explicit")), ("b", Some("Implicit"))],
        );
        c.remove_connection("a");
        c.remove_connection("b");
        assert_eq!(outline(&c), [": ", "Explicit: "]);
    }

    #[test]
    fn hidden_schemas_toggle_is_idempotent() {
        let mut c = config(&[], &[("p", None)]);
        c.set_schema_hidden("p", "audit", true);
        c.set_schema_hidden("p", "audit", true);
        c.set_schema_hidden("p", "Audit", true);
        let p = c.get_connection("p").unwrap();
        assert_eq!(p.hidden_schemas, ["audit", "Audit"], "exact names");
        assert!(p.is_schema_hidden("audit") && !p.is_schema_hidden("public"));
        c.set_schema_hidden("p", "audit", false);
        c.set_schema_hidden("p", "audit", false);
        assert_eq!(c.get_connection("p").unwrap().hidden_schemas, ["Audit"]);
        c.set_schema_hidden("missing", "x", true);
    }

    #[test]
    fn renamed_connection_keeps_its_folder_and_hidden_schemas() {
        let mut c = config(&[], &[("old", Some("F"))]);
        c.set_schema_hidden("old", "s", true);
        c.connections[0].name = "new".into();
        assert_eq!(folder_of(&c, "new"), Some("F"));
        assert!(c.get_connection("new").unwrap().is_schema_hidden("s"));
    }

    #[test]
    fn normalize_refuses_invisible_format_and_separator_characters() {
        for bad in [
            "a\u{200B}b",
            "\u{FEFF}a",
            "a/\u{202E}gpj.exe",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{00AD}b",
            "a\u{2066}b\u{2069}",
            "\u{E0041}a",
        ] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn normalize_requires_a_visible_character_per_segment() {
        for bad in ["\u{3164}", "a/\u{3164}\u{115F}/b", "\u{2800}", "\u{034F}"] {
            assert!(normalize(bad).is_err(), "{bad:?}");
        }
        assert_eq!(normalize("a\u{3164}").unwrap(), "a\u{3164}");
    }

    #[test]
    fn normalize_composes_to_nfc() {
        assert_eq!(normalize("E\u{301}quipe").unwrap(), "\u{C9}quipe");
    }

    #[test]
    fn normalize_limits_depth_and_segment_length() {
        let deep = vec!["d"; 32].join("/");
        assert_eq!(normalize(&deep).unwrap(), deep);
        let err = normalize(&format!("{deep}/e")).unwrap_err();
        assert!(err.contains("32"), "{err}");
        let long = "é".repeat(128);
        assert_eq!(normalize(&long).unwrap(), long, "characters, not bytes");
        let err = normalize(&format!("a/{long}x")).unwrap_err();
        assert!(err.contains("128"), "{err}");
    }

    #[test]
    fn rename_folder_refuses_nesting_too_deep() {
        let deep = vec!["d"; 30].join("/");
        let mut c = config(&[&deep, "M/N"], &[]);
        assert!(
            c.rename_folder("M", &format!("{deep}/d/x")).is_err(),
            "M/N would land 33 levels deep"
        );
        c.rename_folder("M", &format!("{}/x", vec!["d"; 29].join("/")))
            .unwrap();
    }

    #[test]
    fn is_root_and_folder_exists_on_the_root() {
        for root in ["", " ", "/", " // / "] {
            assert!(is_root(root), "{root:?}");
        }
        for not_root in ["a", "a/../b", "\u{200B}"] {
            assert!(!is_root(not_root), "{not_root:?}");
        }
        let c = config(&["A"], &[("p", None)]);
        assert!(!c.folder_exists(""), "the root is not a folder");
        assert!(!c.folder_exists("/"));
    }

    #[test]
    fn create_folder_collisions_are_case_insensitive() {
        let mut c = config(&["Clients"], &[("p", Some("Live/Prod"))]);
        assert!(c.create_folder("clients").is_err());
        assert!(c.create_folder("LIVE").is_err(), "implicit too");
        assert!(c.create_folder("live/prod").is_err());
        assert!(
            c.create_folder("clients/X").is_err(),
            "ancestor in another case"
        );
        assert_eq!(c.create_folder("Clients/X").unwrap(), "Clients/X");
        assert!(c.create_folder("Clients/x").is_err());
        assert_eq!(c.folders, ["Clients", "Clients/X"]);
    }

    #[test]
    fn rename_folder_collisions_are_case_insensitive() {
        let mut c = config(&["Alpha", "Beta", "Beta/Sub"], &[]);
        assert!(c.rename_folder("Alpha", "beta").is_err());
        assert!(c.rename_folder("Alpha", "BETA/sub").is_err());
        assert!(
            c.rename_folder("Alpha", "beta/New").is_err(),
            "ancestor in another case"
        );
        c.rename_folder("Alpha", "Beta/New").unwrap();
        c.rename_folder("Beta/Sub", "Beta/SUB").unwrap();
        assert_eq!(
            c.folders,
            ["Beta/New", "Beta", "Beta/SUB"],
            "case-only rename"
        );
        c.rename_folder("Beta", "BETA").unwrap();
        assert_eq!(c.folders, ["BETA/New", "BETA", "BETA/SUB"]);
    }

    #[test]
    fn move_connection_to_a_path_normalizing_to_empty_means_root() {
        let mut c = config(&[], &[("p", Some("A"))]);
        for root in ["/", " // ", ""] {
            c.move_connection("p", Some("A")).unwrap();
            c.move_connection("p", Some(root)).unwrap();
            assert_eq!(folder_of(&c, "p"), None, "{root:?}");
        }
        assert!(c.move_connection("p", Some("/\u{200B}/")).is_err());
    }
}
