//! Query history popup (Ctrl+Alt+E): search, newest first; Enter or a
//! double-click inserts the query into the active console.

use egui::{Key, Modifiers, RichText};

use crate::engine::services::history::History;

use super::theme::ERROR;

#[derive(Default)]
pub struct HistoryPopup {
    pub search: String,
    pub selected: usize,
    focus_search: bool,
    results: SearchCache,
}

/// Matches of the last search, kept until the query or the history change
/// (the popup is drawn every frame).
#[derive(Default)]
struct SearchCache {
    key: Option<(String, u64)>,
    /// `(entry index, preview of its SQL)`, newest first.
    matches: Vec<(usize, String)>,
}

impl SearchCache {
    fn get(&mut self, history: &History, query: &str) -> &[(usize, String)] {
        let fresh = matches!(&self.key, Some((q, g)) if q == query && *g == history.generation());
        if !fresh {
            self.matches = history
                .search_indices(query)
                .into_iter()
                .filter_map(|i| Some((i, preview(&history.get(i)?.sql, 90))))
                .collect();
            self.key = Some((query.to_string(), history.generation()));
        }
        &self.matches
    }
}

pub enum HistoryAction {
    None,
    /// Insert this SQL into the active console.
    Insert(String),
}

/// "à l'instant", "il y a 5 min", "il y a 3 h", "il y a 2 j" (unix seconds).
pub fn relative_time(now: u64, at: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..=59 => "à l'instant".into(),
        60..=3_599 => format!("il y a {} min", secs / 60),
        3_600..=86_399 => format!("il y a {} h", secs / 3_600),
        _ => format!("il y a {} j", secs / 86_400),
    }
}

/// First line of `sql`, shortened to `max` characters.
pub fn preview(sql: &str, max: usize) -> String {
    let line = sql.trim().lines().next().unwrap_or("");
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max || sql.trim().lines().nth(1).is_some() {
        out.push('…');
    }
    out
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl HistoryPopup {
    pub fn new(search: String) -> Self {
        Self {
            search,
            selected: 0,
            focus_search: true,
            results: SearchCache::default(),
        }
    }

    /// Body of the modal (the caller draws the modal and closes it on Escape).
    pub fn ui(&mut self, ui: &mut egui::Ui, history: &History) -> HistoryAction {
        ui.heading("Historique");
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Rechercher…")
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
        if search.changed() {
            self.selected = 0;
        }
        let entries = self.results.get(history, &self.search);
        if entries.is_empty() {
            ui.label(RichText::new("Aucune requête").weak());
            return HistoryAction::None;
        }
        self.selected = self.selected.min(entries.len() - 1);
        let (down, up, enter) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if down {
            self.selected = (self.selected + 1).min(entries.len() - 1);
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        let mut action = HistoryAction::None;
        if let Some(e) = history.get(entries[self.selected].0).filter(|_| enter) {
            action = HistoryAction::Insert(e.sql.clone());
        }
        let now = now_secs();
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, (index, sql)) in entries.iter().enumerate() {
                    let Some(e) = history.get(*index) else {
                        continue;
                    };
                    let text = RichText::new(sql).monospace();
                    let text = if e.ok { text } else { text.color(ERROR) };
                    let r = ui
                        .selectable_label(i == self.selected, text)
                        .on_hover_text(&e.sql);
                    if (up || down) && i == self.selected {
                        r.scroll_to_me(None);
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} · {} · {} ms",
                            e.connection,
                            relative_time(now, e.at),
                            e.duration_ms
                        ))
                        .small()
                        .weak(),
                    );
                    if r.clicked() {
                        self.selected = i;
                    }
                    if r.double_clicked() {
                        action = HistoryAction::Insert(e.sql.clone());
                    }
                }
            });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(100, 100), "à l'instant");
        assert_eq!(relative_time(100, 200), "à l'instant", "clock skew");
        assert_eq!(relative_time(400, 100), "il y a 5 min");
        assert_eq!(relative_time(3 * 3_600 + 10, 0), "il y a 3 h");
        assert_eq!(relative_time(2 * 86_400, 0), "il y a 2 j");
    }

    #[test]
    fn search_is_cached_until_the_query_or_the_history_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::load_from(dir.path().join("h.json"));
        let entry = |sql: &str| crate::engine::services::history::HistoryEntry {
            sql: sql.into(),
            connection: "c".into(),
            at: 0,
            duration_ms: 1,
            ok: true,
        };
        history.push(entry("SELECT 1"));
        history.push(entry("select 2\nFROM t"));
        let mut cache = SearchCache::default();
        let found = |cache: &mut SearchCache, history: &History, q: &str| {
            cache
                .get(history, q)
                .iter()
                .map(|(_, p)| p.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found(&mut cache, &history, "SELECT"),
            ["select 2…", "SELECT 1"]
        );
        let key = cache.key.clone();
        assert_eq!(
            found(&mut cache, &history, "SELECT"),
            ["select 2…", "SELECT 1"]
        );
        assert_eq!(cache.key, key, "nothing changed");
        assert_eq!(found(&mut cache, &history, "1"), ["SELECT 1"]);
        history.push(entry("SELECT 11"));
        assert_eq!(found(&mut cache, &history, "1"), ["SELECT 11", "SELECT 1"]);
    }

    #[test]
    fn preview_keeps_the_first_line() {
        assert_eq!(preview("SELECT 1", 20), "SELECT 1");
        assert_eq!(preview("  SELECT 1\nFROM t", 20), "SELECT 1…");
        assert_eq!(preview("SELECT abcdef", 6), "SELECT…");
    }
}
