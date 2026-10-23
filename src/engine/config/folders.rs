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

/// Normalize a folder path: segments trimmed, empty ones dropped
/// (`"  a / b//c "` -> `"a/b/c"`). Refuses an empty path, `.` / `..`
/// segments and control characters.
pub fn normalize(path: &str) -> Result<String, String> {
    if path.chars().any(char::is_control) {
        return Err("Le nom du dossier contient un caractère de contrôle".into());
    }
    let segments: Vec<&str> = path
        .split(SEP)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return Err("Le nom du dossier est vide".into());
    }
    if segments.iter().any(|s| *s == "." || *s == "..") {
        return Err("« . » et « .. » ne sont pas des noms de dossier valides".into());
    }
    Ok(segments.join(&SEP.to_string()))
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
    /// Whether the folder exists, explicitly or implicitly.
    pub fn folder_exists(&self, path: &str) -> bool {
        normalize(path).is_ok_and(|p| all_paths(self).contains(&p))
    }

    /// Create a folder (and its missing ancestors, so they outlive their
    /// content too). Returns the normalized path; refuses an existing one.
    pub fn create_folder(&mut self, path: &str) -> Result<String, String> {
        let path = normalize(path)?;
        if all_paths(self).contains(&path) {
            return Err(format!("Le dossier « {path} » existe déjà"));
        }
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
    /// subfolders). Refuses moving it into itself or a descendant, and a
    /// destination that already exists (no merge).
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
        if paths.contains(&to) {
            return Err(format!("Le dossier « {to} » existe déjà"));
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

    /// Move a connection into `folder` (`None`, or a blank string, = root),
    /// which needn't exist yet.
    pub fn move_connection(&mut self, name: &str, folder: Option<&str>) -> Result<(), String> {
        let folder = match folder {
            Some(f) if !f.trim().is_empty() => Some(normalize(f)?),
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
}
