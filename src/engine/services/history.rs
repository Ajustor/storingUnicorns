//! Persistent history of executed queries (`history.json`, next to
//! `config.toml`).

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::engine::config::{write_atomic, AppConfig};

/// One executed query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sql: String,
    /// Name of the connection the query ran on.
    pub connection: String,
    /// Unix seconds.
    pub at: u64,
    pub duration_ms: u64,
    pub ok: bool,
}

/// Query history backed by a JSON file. Entries are kept oldest first.
/// Cloned to be saved off the UI thread.
#[derive(Clone)]
pub struct History {
    entries: Vec<HistoryEntry>,
    path: PathBuf,
    /// Bumped by every change (views caching a search compare it).
    generation: u64,
}

impl History {
    /// Maximum number of entries kept.
    pub const MAX: usize = 500;

    /// `history.json` in the same directory as `config.toml`.
    pub fn default_path() -> Result<PathBuf> {
        Ok(AppConfig::config_path()?.with_file_name("history.json"))
    }

    /// Missing or unreadable file → empty history (never fails the app).
    /// A UTF-8 BOM (left by Notepad or Windows PowerShell 5) is skipped.
    pub fn load_from(path: PathBuf) -> Self {
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();
        Self {
            entries,
            path,
            generation: 0,
        }
    }

    /// Newest last. Line endings are stored as `\n`. Skips an entry
    /// identical (same sql + connection) to the newest one (updates its
    /// `at`/`duration_ms`/`ok` instead). Keeps the last `MAX`.
    pub fn push(&mut self, mut entry: HistoryEntry) {
        if entry.sql.contains('\r') {
            entry.sql = entry.sql.replace("\r\n", "\n").replace('\r', "\n");
        }
        self.generation += 1;
        match self.entries.last_mut() {
            Some(last) if last.sql == entry.sql && last.connection == entry.connection => {
                last.at = entry.at;
                last.duration_ms = entry.duration_ms;
                last.ok = entry.ok;
            }
            _ => self.entries.push(entry),
        }
        if self.entries.len() > Self::MAX {
            let excess = self.entries.len() - Self::MAX;
            self.entries.drain(..excess);
        }
    }

    /// Write the history to its file through a uniquely named temporary file
    /// (`write_atomic`): a crash mid-write never leaves a truncated history,
    /// and concurrent saves (two instances, a background save) never clobber
    /// each other's temporary file.
    pub fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.entries)?;
        write_atomic(&self.path, json.as_bytes())
    }

    /// The entries [`History::search_indices`] finds.
    #[cfg(test)]
    pub fn search(&self, query: &str) -> Vec<&HistoryEntry> {
        self.search_indices(query)
            .into_iter()
            .map(|i| &self.entries[i])
            .collect()
    }

    /// Indexes (for [`History::get`]) of the entries whose `sql` or
    /// connection name contain every word of `query` (case-insensitive;
    /// blank query → all), newest first.
    pub fn search_indices(&self, query: &str) -> Vec<usize> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        (0..self.entries.len())
            .rev()
            .filter(|&i| {
                let e = &self.entries[i];
                let haystack = format!("{}\n{}", e.sql, e.connection).to_lowercase();
                words.iter().all(|w| haystack.contains(w.as_str()))
            })
            .collect()
    }

    pub fn get(&self, index: usize) -> Option<&HistoryEntry> {
        self.entries.get(index)
    }

    /// Changes at each [`History::push`].
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(sql: &str, at: u64) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            connection: "c".into(),
            at,
            duration_ms: 1,
            ok: true,
        }
    }

    #[test]
    fn push_dedupes_consecutive_and_caps() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load_from(dir.path().join("h.json"));
        h.push(e("SELECT 1", 1));
        h.push(e("SELECT 1", 2));
        assert_eq!(h.search("").len(), 1);
        assert_eq!(h.search("")[0].at, 2);
        for i in 0..(History::MAX + 10) {
            h.push(e(&format!("SELECT {i}"), i as u64));
        }
        assert_eq!(h.search("").len(), History::MAX);
    }

    #[test]
    fn search_is_newest_first_and_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load_from(dir.path().join("h.json"));
        h.push(e("select * from users", 1));
        h.push(e("SELECT * FROM orders", 2));
        h.push(e("SELECT * FROM Users WHERE id = 1", 3));
        let r = h.search("users");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].at, 3);
    }

    #[test]
    fn concurrent_saves_never_clobber_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.json");
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let mut h = History::load_from(path.clone());
                std::thread::spawn(move || {
                    for i in 0..20 {
                        h.push(e(&format!("SELECT {t}, {i}"), i));
                        h.save().unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // One writer's complete history, and no temporary file left behind.
        assert_eq!(History::load_from(path.clone()).search("").len(), 20);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn non_ascii_sql_round_trips_as_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.json");
        let sql = "SELECT 'é ç à ù', '🦄', \"prénom\"\n\tFROM t";
        let mut h = History::load_from(path.clone());
        h.push(e(sql, 1));
        h.save().unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(
            String::from_utf8(raw).unwrap().contains("'é ç à ù', '🦄'"),
            "stored as plain UTF-8, not escaped"
        );
        assert_eq!(History::load_from(path).search("")[0].sql, sql);
    }

    #[test]
    fn a_file_with_a_utf8_bom_still_loads() {
        // Notepad and Windows PowerShell 5 write UTF-8 with a BOM, which
        // serde_json rejects: the history came back empty and the next save
        // overwrote it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.json");
        let json = r#"[{"sql":"SELECT 'é'","connection":"c","at":1,"duration_ms":1,"ok":true}]"#;
        std::fs::write(&path, format!("\u{feff}{json}")).unwrap();
        let h = History::load_from(path);
        assert_eq!(h.search("")[0].sql, "SELECT 'é'");
    }

    #[test]
    fn push_normalizes_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load_from(dir.path().join("h.json"));
        h.push(e("SELECT 1\r\nFROM t\rWHERE x", 1));
        assert_eq!(h.search("")[0].sql, "SELECT 1\nFROM t\nWHERE x");
        h.push(e("SELECT 1\nFROM t\nWHERE x", 2));
        assert_eq!(
            h.search("").len(),
            1,
            "same query whatever its line endings"
        );
    }

    #[test]
    fn search_matches_every_word_in_the_sql_or_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = History::load_from(dir.path().join("h.json"));
        let mut entry = e("SELECT *\nFROM clients\nWHERE ville = 'Besançon'", 1);
        entry.connection = "Prod PostgreSQL".into();
        h.push(entry);
        h.push(e("SELECT * FROM commandes", 2));
        assert_eq!(
            h.search("clients besançon").len(),
            1,
            "words on several lines"
        );
        assert_eq!(h.search("BESANÇON").len(), 1, "case-insensitive, accents");
        assert_eq!(h.search("prod clients").len(), 1, "connection name");
        assert_eq!(h.search("postgresql").len(), 1);
        assert_eq!(h.search("prod commandes").len(), 0, "every word must match");
        assert_eq!(h.search("  ").len(), 2, "blank query → all");
    }

    #[test]
    fn save_and_reload_roundtrip_and_corrupt_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.json");
        let mut h = History::load_from(path.clone());
        h.push(e("SELECT 1", 1));
        h.save().unwrap();
        assert_eq!(History::load_from(path.clone()).search("").len(), 1);
        std::fs::write(&path, "{nope").unwrap();
        assert!(History::load_from(path).search("").is_empty());
    }
}
