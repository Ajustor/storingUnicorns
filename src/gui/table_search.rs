//! Ctrl+N: find a table across every open connection; Enter or a
//! double-click opens its data.

use egui::{Color32, Key, Modifiers, RichText};

/// Rows shown at most.
pub const MAX_SHOWN: usize = 50;

/// Fuzzy score of `name` for `query` (case-insensitive), lower is better:
/// prefix < contains < subsequence; `None` when the letters don't appear in
/// order. An empty query matches everything equally.
pub fn rank(query: &str, name: &str) -> Option<u32> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let n: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(0);
    }
    let cap = |v: usize| v.min(9_999) as u32;
    let extra = n.len().saturating_sub(q.len());
    if n.starts_with(&q) {
        return Some(cap(extra));
    }
    if let Some(pos) = n.windows(q.len()).position(|w| w == q.as_slice()) {
        return Some(10_000 + pos.min(99) as u32 * 100 + extra.min(99) as u32);
    }
    // Greedy subsequence; the score is the gaps between matched letters.
    let mut it = q.iter().peekable();
    let (mut first, mut last) = (None, 0);
    for (i, c) in n.iter().enumerate() {
        if it.peek() == Some(&c) {
            it.next();
            first.get_or_insert(i);
            last = i;
        }
    }
    if it.peek().is_some() {
        return None;
    }
    let span = last - first.unwrap_or(0) + 1;
    Some(20_000 + cap(span - q.len()))
}

/// Indexes of `tables` (connection, schema, table) matching `query`, best
/// first: by score of the table name (or `schema.table`, ranked after), then
/// by table name.
pub fn ranked(query: &str, tables: &[(String, String, String)]) -> Vec<usize> {
    let mut scored: Vec<(u32, usize)> = tables
        .iter()
        .enumerate()
        .filter_map(|(i, (_, schema, table))| {
            rank(query, table)
                .or_else(|| rank(query, &format!("{schema}.{table}")).map(|s| s + 100_000))
                .map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| tables[a.1].2.cmp(&tables[b.1].2))
    });
    scored.into_iter().map(|(_, i)| i).collect()
}

pub struct TableSearch {
    pub query: String,
    /// `(connection, schema, table)` of every open connection, when opened.
    tables: Vec<(String, String, String)>,
    /// Matches of `query`, best first (recomputed when the query changes).
    matches: Vec<usize>,
    selected: usize,
    focus: bool,
}

impl TableSearch {
    pub fn new(tables: Vec<(String, String, String)>) -> Self {
        let matches = ranked("", &tables);
        Self {
            query: String::new(),
            tables,
            matches,
            selected: 0,
            focus: true,
        }
    }

    /// Body of the modal; returns the `(connection, schema, table)` to open.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        color_of: &dyn Fn(&str) -> Color32,
    ) -> Option<(String, String, String)> {
        ui.heading("Rechercher une table");
        // Keys are taken before the text field sees them.
        let (down, up, enter) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        let field = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Nom de table…")
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus) {
            field.request_focus();
        }
        if field.changed() {
            self.matches = ranked(&self.query, &self.tables);
            self.selected = 0;
        }
        if self.matches.is_empty() {
            ui.label(RichText::new("Aucune table").weak());
            return None;
        }
        let shown = self.matches.len().min(MAX_SHOWN);
        if down {
            self.selected = (self.selected + 1).min(shown - 1);
        }
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        self.selected = self.selected.min(shown - 1);
        let mut open = enter.then_some(self.selected);
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(420.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (pos, &i) in self.matches[..shown].iter().enumerate() {
                    let (connection, schema, table) = &self.tables[i];
                    ui.horizontal(|ui| {
                        super::theme::dot(ui, color_of(connection));
                        let r = ui.selectable_label(
                            pos == self.selected,
                            RichText::new(format!("{schema}.{table}")).monospace(),
                        );
                        ui.label(RichText::new(connection).weak());
                        if (up || down) && pos == self.selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            self.selected = pos;
                        }
                        if r.double_clicked() {
                            open = Some(pos);
                        }
                    });
                }
            });
        if self.matches.len() > shown {
            ui.label(
                RichText::new(format!("{shown} premières sur {}", self.matches.len()))
                    .small()
                    .weak(),
            );
        }
        open.map(|pos| self.tables[self.matches[pos]].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_orders_prefix_contains_subsequence() {
        let prefix = rank("us", "users").unwrap();
        let contains = rank("ser", "users").unwrap();
        let subseq = rank("urs", "users").unwrap();
        assert!(prefix < contains && contains < subseq);
        assert_eq!(rank("xyz", "users"), None);
        assert_eq!(rank("sru", "users"), None, "letters must be in order");
    }

    #[test]
    fn rank_is_case_insensitive_and_prefers_tight_matches() {
        assert_eq!(rank("USERS", "users"), Some(0), "exact match is best");
        assert!(rank("user", "users").unwrap() < rank("user", "user_roles").unwrap());
        assert!(rank("ord", "orders").unwrap() < rank("ord", "my_orders").unwrap());
        assert!(rank("ord", "my_orders").unwrap() < rank("ord", "the_big_orders").unwrap());
        assert!(rank("uo", "u_o").unwrap() < rank("uo", "u_____o").unwrap());
        assert_eq!(rank("", "anything"), Some(0));
        assert_eq!(rank("é", "Été"), Some(2));
    }

    #[test]
    fn ranked_sorts_and_falls_back_to_schema() {
        let t = |c: &str, s: &str, n: &str| (c.to_string(), s.to_string(), n.to_string());
        let tables = vec![
            t("prod", "public", "user_roles"),
            t("prod", "public", "orders"),
            t("dev", "main", "users"),
            t("prod", "audit", "log"),
        ];
        assert_eq!(ranked("user", &tables), vec![2, 0]);
        assert_eq!(ranked("audit.l", &tables), vec![3]);
        assert_eq!(ranked("", &tables), vec![3, 1, 0, 2], "by name");
    }
}
