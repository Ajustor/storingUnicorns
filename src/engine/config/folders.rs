//! Connection folders (stub of the engine contract, replaced by the engine
//! commit).

use super::AppConfig;
use crate::engine::models::ConnectionConfig;

pub const SEP: char = '/';

/// "  a / b//c " -> "a/b/c"; refuses empty paths, "." / ".." and control
/// characters.
pub fn normalize(path: &str) -> Result<String, String> {
    let mut parts = Vec::new();
    for part in path.split(SEP) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return Err(format!("Nom de dossier invalide : « {part} »"));
        }
        if part.chars().any(char::is_control) {
            return Err("Le nom du dossier contient un caractère de contrôle".into());
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err("Le nom du dossier est vide".into());
    }
    Ok(parts.join(&SEP.to_string()))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderNode {
    pub path: String,
    pub name: String,
    pub folders: Vec<FolderNode>,
    pub connections: Vec<usize>,
}

impl FolderNode {
    fn child(&mut self, name: &str) -> &mut FolderNode {
        let pos = match self.folders.iter().position(|f| f.name == name) {
            Some(p) => p,
            None => {
                let path = if self.path.is_empty() {
                    name.to_string()
                } else {
                    format!("{}{SEP}{name}", self.path)
                };
                self.folders.push(FolderNode {
                    path,
                    name: name.to_string(),
                    ..Default::default()
                });
                self.folders.len() - 1
            }
        };
        &mut self.folders[pos]
    }

    fn at(&mut self, path: &str) -> &mut FolderNode {
        let mut node = self;
        for part in path.split(SEP) {
            node = node.child(part);
        }
        node
    }

    fn sort(&mut self, connections: &[ConnectionConfig]) {
        self.folders.sort_by_key(|f| f.name.to_lowercase());
        self.connections
            .sort_by_key(|&i| connections[i].name.to_lowercase());
        for f in &mut self.folders {
            f.sort(connections);
        }
    }
}

/// Sorted tree (folders then connections, case-insensitive alphabetical
/// order) from `folders` and the connections' `folder` (missing ancestors
/// are created implicitly).
pub fn tree(config: &AppConfig) -> FolderNode {
    let mut root = FolderNode::default();
    for f in &config.folders {
        if let Ok(p) = normalize(f) {
            root.at(&p);
        }
    }
    for (i, c) in config.connections.iter().enumerate() {
        match c.folder.as_deref().map(normalize) {
            Some(Ok(p)) => root.at(&p).connections.push(i),
            _ => root.connections.push(i),
        }
    }
    root.sort(&config.connections);
    root
}

/// `path` relocated from under `from` to under `to`, if it is `from` or one
/// of its descendants.
fn rebase(path: &str, from: &str, to: &str) -> Option<String> {
    if path == from {
        Some(to.to_string())
    } else {
        path.strip_prefix(from)
            .and_then(|rest| rest.strip_prefix(SEP))
            .map(|rest| format!("{to}{SEP}{rest}"))
    }
}

fn parent(path: &str) -> Option<&str> {
    path.rsplit_once(SEP).map(|(p, _)| p)
}

impl AppConfig {
    pub fn create_folder(&mut self, path: &str) -> Result<String, String> {
        let path = normalize(path)?;
        if !self.folders.contains(&path) {
            self.folders.push(path.clone());
        }
        Ok(path)
    }

    /// Renames / moves a folder and all its content (connections and
    /// subfolders); refuses to move it into itself.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<(), String> {
        let from = normalize(from)?;
        let to = normalize(to)?;
        if from == to {
            return Ok(());
        }
        if rebase(&to, &from, "").is_some() {
            return Err("Impossible de déplacer un dossier dans lui-même".into());
        }
        let mut folders: Vec<String> = Vec::new();
        for f in &self.folders {
            let f = rebase(f, &from, &to).unwrap_or_else(|| f.clone());
            if !folders.contains(&f) {
                folders.push(f);
            }
        }
        if !folders.contains(&to) {
            folders.push(to.clone());
        }
        self.folders = folders;
        for c in &mut self.connections {
            if let Some(f) = c.folder.as_deref().and_then(|f| rebase(f, &from, &to)) {
                c.folder = Some(f);
            }
        }
        Ok(())
    }

    /// Deletes the folder; its content moves up one level.
    pub fn delete_folder(&mut self, path: &str) -> Result<(), String> {
        let path = normalize(path)?;
        let up = parent(&path).map(str::to_string);
        let lift = |p: &str| -> Option<Option<String>> {
            let rest = if p == path {
                return Some(up.clone());
            } else {
                p.strip_prefix(&path)?.strip_prefix(SEP)?
            };
            Some(Some(match &up {
                Some(u) => format!("{u}{SEP}{rest}"),
                None => rest.to_string(),
            }))
        };
        let mut folders = Vec::new();
        for f in &self.folders {
            match lift(f) {
                None => folders.push(f.clone()),
                Some(Some(f)) => folders.push(f),
                Some(None) => {}
            }
        }
        folders.dedup();
        self.folders = folders;
        for c in &mut self.connections {
            if let Some(new) = c.folder.as_deref().and_then(lift) {
                c.folder = new;
            }
        }
        Ok(())
    }

    pub fn move_connection(&mut self, name: &str, folder: Option<&str>) -> Result<(), String> {
        let folder = folder.map(normalize).transpose()?;
        let conn = self
            .connections
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("Connexion introuvable : {name}"))?;
        conn.folder = folder;
        Ok(())
    }

    pub fn set_schema_hidden(&mut self, connection: &str, schema: &str, hidden: bool) {
        if let Some(c) = self.connections.iter_mut().find(|c| c.name == connection) {
            c.hidden_schemas.retain(|s| s != schema);
            if hidden {
                c.hidden_schemas.push(schema.to_string());
            }
        }
    }
}

impl ConnectionConfig {
    pub fn is_schema_hidden(&self, schema: &str) -> bool {
        self.hidden_schemas.iter().any(|s| s == schema)
    }
}
