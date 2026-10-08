use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
    let dir = if cfg!(test) {
        Some(std::env::temp_dir().join(format!("storing-unicorns-test-{}", std::process::id())))
    } else {
        resolve_app_dir(std::env::var_os(CONFIG_DIR_ENV), dirs::config_dir())
    }
    .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
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
        fs::write(path, content)?;
        Ok(())
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
    fn tests_never_use_the_real_config_dir() {
        let dir = app_dir().unwrap();
        assert!(dir.starts_with(std::env::temp_dir()));
        if let Some(real) = dirs::config_dir() {
            assert!(!dir.starts_with(real.join("storing-unicorns")));
        }
    }
}
