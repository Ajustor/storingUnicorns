use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::engine::models::ConnectionConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub connections: Vec<ConnectionConfig>,
    pub last_connection: Option<String>,
    /// Release the user chose not to be offered again.
    #[serde(default)]
    pub skipped_version: Option<String>,
    /// GUI theme: "system" (default), "dark" or "light".
    #[serde(default)]
    pub theme: Option<String>,
}

/// Environment variable overriding the directory holding every file the app
/// writes (config, consoles, history, update check, debug log).
#[cfg_attr(test, allow(dead_code))]
pub const CONFIG_DIR_ENV: &str = "STORINGUNICORNS_CONFIG_DIR";

/// Pick the app directory: a non-empty override wins, otherwise
/// `<platform config dir>/storing-unicorns`.
fn resolve_app_dir(
    override_dir: Option<std::ffi::OsString>,
    platform_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    match override_dir.filter(|d| !d.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => platform_dir.map(|d| d.join("storing-unicorns")),
    }
}

/// Directory holding the app's files, created if missing. Unit tests always
/// get a private temporary directory so they never touch the user's files.
pub fn app_dir() -> Result<PathBuf> {
    let dir =
        app_dir_path().ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(not(test))]
fn app_dir_path() -> Option<PathBuf> {
    resolve_app_dir(std::env::var_os(CONFIG_DIR_ENV), dirs::config_dir())
}

/// `%TEMP%/storing-unicorns-tests/<pid>`: one directory per test process,
/// under one parent whose leftovers older than a day are removed on first
/// use (a test run can't clean up after itself, the process just exits).
#[cfg(test)]
fn app_dir_path() -> Option<PathBuf> {
    static CLEANED: std::sync::Once = std::sync::Once::new();
    let parent = std::env::temp_dir().join("storing-unicorns-tests");
    let own = std::process::id().to_string();
    CLEANED.call_once(|| {
        let day = std::time::Duration::from_secs(24 * 3600);
        let now = std::time::SystemTime::now();
        remove_stale_dirs(&parent, "", day, now, &own);
        // Directories of the former layout, one per process directly in %TEMP%.
        remove_stale_dirs(
            &std::env::temp_dir(),
            "storing-unicorns-test-",
            day,
            now,
            "",
        );
    });
    Some(parent.join(own))
}

/// Remove the subdirectories of `parent` named `prefix…` (but `keep`) not
/// modified for `max_age` at `now`. Best effort: errors are ignored.
#[cfg(test)]
fn remove_stale_dirs(
    parent: &Path,
    prefix: &str,
    max_age: std::time::Duration,
    now: std::time::SystemTime,
    keep: &str,
) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > max_age));
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let name = entry.file_name();
        let named = name
            .to_str()
            .is_some_and(|n| n.starts_with(prefix) && n != keep);
        if is_dir && stale && named {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Replace `path` with `contents` through a uniquely named temporary file
/// in the same directory, renamed over it: a crash mid-write never leaves a
/// truncated file, and concurrent writers (two instances, overlapping
/// background saves) never share a temporary file. The last rename wins.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    // Windows refuses to replace a file that another rename or a reader is
    // holding for a moment: retry briefly.
    let mut attempts = 0;
    loop {
        match tmp.persist(path) {
            Ok(_) => return Ok(()),
            Err(e) if attempts < 20 && e.error.kind() == std::io::ErrorKind::PermissionDenied => {
                attempts += 1;
                tmp = e.file;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(e.error.into()),
        }
    }
}

impl AppConfig {
    /// Get the config file path
    pub fn config_path() -> Result<PathBuf> {
        Ok(app_dir()?.join("config.toml"))
    }

    /// Load configuration from disk
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let config: AppConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(AppConfig::default())
        }
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        let content = toml::to_string_pretty(self)?;
        write_atomic(&path, content.as_bytes())
    }

    /// Add a new connection
    pub fn add_connection(&mut self, conn: ConnectionConfig) {
        self.connections.push(conn);
    }

    /// Remove a connection by name
    #[allow(dead_code)]
    pub fn remove_connection(&mut self, name: &str) {
        self.connections.retain(|c| c.name != name);
    }

    /// Get a connection by name
    #[allow(dead_code)]
    pub fn get_connection(&self, name: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_new_fields_still_parses() {
        let c: AppConfig = toml::from_str("connections = []").unwrap();
        assert!(c.skipped_version.is_none() && c.theme.is_none());
    }

    #[test]
    fn override_dir_wins_when_non_empty() {
        let platform = Some(PathBuf::from("/cfg"));
        assert_eq!(
            resolve_app_dir(Some("/tmp/x".into()), platform.clone()),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            resolve_app_dir(Some("".into()), platform.clone()),
            Some(PathBuf::from("/cfg").join("storing-unicorns"))
        );
        assert_eq!(
            resolve_app_dir(None, platform),
            Some(PathBuf::from("/cfg").join("storing-unicorns"))
        );
        assert_eq!(resolve_app_dir(None, None), None);
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("f.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["f.json"]);
    }

    #[test]
    fn stale_test_dirs_are_removed() {
        let parent = tempfile::tempdir().unwrap();
        for d in ["old", "mine", "other-old"] {
            fs::create_dir(parent.path().join(d)).unwrap();
        }
        fs::write(parent.path().join("file"), "x").unwrap();
        let day = std::time::Duration::from_secs(24 * 3600);
        let now = std::time::SystemTime::now();
        remove_stale_dirs(parent.path(), "", day, now, "mine");
        assert!(parent.path().join("old").exists(), "recent: kept");
        remove_stale_dirs(parent.path(), "other-", day, now + 2 * day, "mine");
        assert!(
            !parent.path().join("other-old").exists(),
            "prefixed and old"
        );
        assert!(parent.path().join("old").exists(), "not prefixed");
        remove_stale_dirs(parent.path(), "", day, now + 2 * day, "mine");
        assert!(!parent.path().join("old").exists(), "older than a day");
        assert!(parent.path().join("mine").exists(), "this process's own");
        assert!(parent.path().join("file").exists(), "only directories");
        remove_stale_dirs(&parent.path().join("missing"), "", day, now, "mine");
    }

    #[test]
    fn tests_never_use_the_real_config_dir() {
        let dir = app_dir().unwrap();
        assert!(dir.starts_with(std::env::temp_dir().join("storing-unicorns-tests")));
        if let Some(real) = dirs::config_dir() {
            assert!(!dir.starts_with(real.join("storing-unicorns")));
        }
    }
}
