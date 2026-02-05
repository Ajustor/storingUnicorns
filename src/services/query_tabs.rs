use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A single query tab
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTab {
    pub name: String,
    pub query: String,
    pub cursor_position: usize,
    #[serde(default)]
    pub is_modified: bool,
}

impl Default for QueryTab {
    fn default() -> Self {
        Self {
            name: String::from("Query 1"),
            query: String::new(),
            cursor_position: 0,
            is_modified: false,
        }
    }
}

impl QueryTab {
    pub fn new(name: String) -> Self {
        Self {
            name,
            query: String::new(),
            cursor_position: 0,
            is_modified: false,
        }
    }

    #[allow(dead_code)]
    pub fn with_query(name: String, query: String) -> Self {
        Self {
            name,
            cursor_position: query.len(),
            query,
            is_modified: false,
        }
    }
}

/// Manager for query tabs with persistence
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryTabsState {
    pub tabs: Vec<QueryTab>,
    pub active_tab: usize,
}

impl QueryTabsState {
    /// Get the queries file path
    pub fn queries_path() -> Result<PathBuf> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;
        let app_dir = config_dir.join("storing-unicorns");
        fs::create_dir_all(&app_dir)?;
        Ok(app_dir.join("queries.toml"))
    }

    /// Load saved queries from disk
    pub fn load() -> Result<Self> {
        let path = Self::queries_path()?;
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let mut state: QueryTabsState = toml::from_str(&content)?;
            // Reset modified flags on load
            for tab in &mut state.tabs {
                tab.is_modified = false;
            }
            // Ensure at least one tab exists
            if state.tabs.is_empty() {
                state.tabs.push(QueryTab::default());
            }
            Ok(state)
        } else {
            Ok(Self::new())
        }
    }

    /// Save queries to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::queries_path()?;
        let content = toml::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    /// Create a new state with one default tab
    pub fn new() -> Self {
        Self {
            tabs: vec![QueryTab::default()],
            active_tab: 0,
        }
    }

    /// Get the current active tab
    pub fn current_tab(&self) -> &QueryTab {
        &self.tabs[self.active_tab]
    }

    /// Get the current active tab mutably
    pub fn current_tab_mut(&mut self) -> &mut QueryTab {
        &mut self.tabs[self.active_tab]
    }

    /// Add a new tab and switch to it
    pub fn add_tab(&mut self) {
        let tab_num = self.tabs.len() + 1;
        self.tabs.push(QueryTab::new(format!("Query {}", tab_num)));
        self.active_tab = self.tabs.len() - 1;
    }

    /// Add a new tab with a specific query
    #[allow(dead_code)]
    pub fn add_tab_with_query(&mut self, name: String, query: String) {
        self.tabs.push(QueryTab::with_query(name, query));
        self.active_tab = self.tabs.len() - 1;
    }

    /// Close the current tab (if more than one tab exists)
    pub fn close_current_tab(&mut self) -> bool {
        if self.tabs.len() > 1 {
            self.tabs.remove(self.active_tab);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
            true
        } else {
            false
        }
    }

    /// Switch to the next tab
    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    /// Switch to the previous tab
    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = self
                .active_tab
                .checked_sub(1)
                .unwrap_or(self.tabs.len() - 1);
        }
    }

    /// Switch to a specific tab by index
    pub fn switch_to_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active_tab = index;
        }
    }

    /// Rename the current tab
    #[allow(dead_code)]
    pub fn rename_current_tab(&mut self, name: String) {
        self.tabs[self.active_tab].name = name;
    }
}
