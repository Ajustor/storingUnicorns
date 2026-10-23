//! Connection folders (WIP stub, replaced by the engine implementation).
#![allow(dead_code)]

use super::AppConfig;
use crate::engine::models::ConnectionConfig;

pub const SEP: char = '/';

/// "  a / b//c " -> "a/b/c"; refuses empty, "." / "..", control characters.
pub fn normalize(path: &str) -> Result<String, String> {
    let parts: Vec<&str> = path
        .split(SEP)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("Le nom du dossier est vide".into());
    }
    for p in &parts {
        if *p == "." || *p == ".." {
            return Err(format!("Nom de dossier invalide : {p}"));
        }
        if p.chars().any(char::is_control) {
            return Err("Le nom du dossier contient des caractères de contrôle".into());
        }
    }
    Ok(parts.join("/"))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FolderNode {
    pub path: String,
    pub name: String,
    pub folders: Vec<FolderNode>,
    pub connections: Vec<usize>,
}

fn insert<'a>(node: &'a mut FolderNode, path: &str) -> &'a mut FolderNode {
    let mut cur = node;
    let mut acc = String::new();
    for part in path.split(SEP) {
        if !acc.is_empty() {
            acc.push(SEP);
        }
        acc.push_str(part);
        let idx = match cur.folders.iter().position(|f| f.name == part) {
            Some(i) => i,
            None => {
                cur.folders.push(FolderNode {
                    path: acc.clone(),
                    name: part.to_string(),
                    ..Default::default()
                });
                cur.folders.len() - 1
            }
        };
        cur = &mut cur.folders[idx];
    }
    cur
}

fn sort(node: &mut FolderNode, conns: &[ConnectionConfig]) {
    node.folders.sort_by_key(|f| f.name.to_lowercase());
    node.connections
        .sort_by_key(|&i| conns[i].name.to_lowercase());
    for f in &mut node.folders {
        sort(f, conns);
    }
}

/// Sorted tree from `folders` and the connections' `folder`.
pub fn tree(config: &AppConfig) -> FolderNode {
    let mut root = FolderNode::default();
    for f in &config.folders {
        if let Ok(p) = normalize(f) {
            insert(&mut root, &p);
        }
    }
    for (i, c) in config.connections.iter().enumerate() {
        match c.folder.as_deref().map(normalize) {
            Some(Ok(p)) => insert(&mut root, &p).connections.push(i),
            _ => root.connections.push(i),
        }
    }
    sort(&mut root, &config.connections);
    root
}

fn reparent(path: &str, from: &str, to: &str) -> Option<String> {
    if path == from {
        Some(to.to_string())
    } else {
        path.strip_prefix(from)
            .and_then(|r| r.strip_prefix(SEP))
            .map(|r| format!("{to}{SEP}{r}"))
    }
}

fn parent(path: &str) -> Option<String> {
    path.rsplit_once(SEP).map(|(p, _)| p.to_string())
}

impl AppConfig {
    pub fn create_folder(&mut self, path: &str) -> Result<String, String> {
        let p = normalize(path)?;
        if !self.folders.contains(&p) {
            self.folders.push(p.clone());
        }
        Ok(p)
    }

    pub fn rename_folder(&mut self, from: &str, to: &str) -> Result<(), String> {
        let from = normalize(from)?;
        let to = normalize(to)?;
        if reparent(&to, &from, "").is_some() && to != from {
            return Err("Impossible de déplacer un dossier dans lui-même".into());
        }
        for f in &mut self.folders {
            if let Some(n) = reparent(f, &from, &to) {
                *f = n;
            }
        }
        if !self.folders.contains(&to) {
            self.folders.push(to.clone());
        }
        let mut seen = std::collections::HashSet::new();
        self.folders.retain(|f| seen.insert(f.clone()));
        for c in &mut self.connections {
            if let Some(n) = c.folder.as_deref().and_then(|f| reparent(f, &from, &to)) {
                c.folder = Some(n);
            }
        }
        Ok(())
    }

    pub fn delete_folder(&mut self, path: &str) -> Result<(), String> {
        let path = normalize(path)?;
        let up = parent(&path);
        let lift = |f: &str| -> Option<Option<String>> {
            if f == path {
                Some(up.clone())
            } else {
                f.strip_prefix(&path)
                    .and_then(|r| r.strip_prefix(SEP))
                    .map(|r| {
                        Some(match &up {
                            Some(u) => format!("{u}{SEP}{r}"),
                            None => r.to_string(),
                        })
                    })
            }
        };
        self.folders = self
            .folders
            .iter()
            .filter_map(|f| match lift(f) {
                Some(n) => n,
                None => Some(f.clone()),
            })
            .collect();
        for c in &mut self.connections {
            if let Some(n) = c.folder.as_deref().and_then(lift) {
                c.folder = n;
            }
        }
        Ok(())
    }

    pub fn move_connection(&mut self, name: &str, folder: Option<&str>) -> Result<(), String> {
        let folder = folder.map(normalize).transpose()?;
        let c = self
            .connections
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or_else(|| format!("Connexion introuvable : {name}"))?;
        c.folder = folder;
        Ok(())
    }

    pub fn set_schema_hidden(&mut self, connection: &str, schema: &str, hidden: bool) {
        if let Some(c) = self.connections.iter_mut().find(|c| c.name == connection) {
            let pos = c.hidden_schemas.iter().position(|s| s == schema);
            match (hidden, pos) {
                (true, None) => c.hidden_schemas.push(schema.to_string()),
                (false, Some(i)) => {
                    c.hidden_schemas.remove(i);
                }
                _ => {}
            }
        }
    }
}

impl ConnectionConfig {
    pub fn is_schema_hidden(&self, schema: &str) -> bool {
        self.hidden_schemas.iter().any(|s| s == schema)
    }
}
