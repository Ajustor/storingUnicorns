use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::models::ConnectionConfig;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub connections: Vec<ConnectionConfig>,
    pub last_connection: Option<String>,
}

impl AppConfig {
    /// Get the config file path
    pub fn config_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
        let app_dir = config_dir.join("datagrip-tui");
        fs::create_dir_all(&app_dir)?;
        Ok(app_dir.join("config.toml"))
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
