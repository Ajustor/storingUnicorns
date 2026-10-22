//! Ctrl+N: find a table across every open connection; Enter or a
//! double-click opens its data.

use egui::{Color32, Key, Modifiers, RichText};
use egui_phosphor::regular as icon;

use super::modal::{self, ModalButton, ModalResponse, ModalSize, ModalSpec};

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

    /// The table search modal (standard, large): "Ouvrir" (Enter, a
    /// double-click) answers the selected table, by its index (see
    /// [`TableSearch::table`]).
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        color_of: &dyn Fn(&str) -> Color32,
    ) -> ModalResponse<usize> {
        let any = !self.matches.is_empty();
        let open = ModalButton::primary(format!("{} Ouvrir", icon::TABLE), ())
            .enabled(any)
            .hint(if any {
                "Ouvrir les données de la table (Entrée)"
            } else {
                "Aucune table sélectionnée"
            });
        let mut activated = None;
        let response = modal::show(
            ui,
            ModalSpec::new("Rechercher une table", ModalSize::Large).icon(icon::MAGNIFYING_GLASS),
            |ui| activated = self.body(ui, color_of),
            None,
            std::slice::from_ref(&open),
        );
        let pos = match response {
            ModalResponse::Cancel => return ModalResponse::Cancel,
            // Enter or a double-click in the list.
            ModalResponse::None => match activated {
                Some(pos) => pos,
                None => return ModalResponse::None,
            },
            ModalResponse::Button(()) => self.selected,
        };
        match self.matches.get(pos) {
            Some(&i) => ModalResponse::Button(i),
            None => ModalResponse::None,
        }
    }

    /// `(connection, schema, table)` of the table at `index`.
    pub fn table(&self, index: usize) -> Option<&(String, String, String)> {
        self.tables.get(index)
    }

    /// Search field and list; the position in the list to open when Enter
    /// or a double-click asks for it.
    fn body(&mut self, ui: &mut egui::Ui, color_of: &dyn Fn(&str) -> Color32) -> Option<usize> {
        let screen = ui.ctx().screen_rect();
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
            .max_height(super::popup::modal_list_height(screen.height()))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (pos, &i) in self.matches[..shown].iter().enumerate() {
                    let (connection, schema, table) = &self.tables[i];
                    let r = table_row(
                        ui,
                        pos == self.selected,
                        &format!("{schema}.{table}"),
                        connection,
                        color_of(connection),
                    );
                    if (up || down) && pos == self.selected {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        self.selected = pos;
                    }
                    if r.double_clicked() {
                        open = Some(pos);
                    }
                }
            });
        if self.matches.len() > shown {
            ui.label(
                RichText::new(format!("{shown} premières sur {}", self.matches.len()))
                    .small()
                    .weak(),
            );
        }
        open
    }
}

/// Full-width row: the connection's colour dot, `name` (elided when the
/// row is too narrow, then shown whole in a tooltip) and the connection
/// name aligned right.
fn table_row(
    ui: &mut egui::Ui,
    selected: bool,
    name: &str,
    connection: &str,
    color: Color32,
) -> egui::Response {
    use super::popup;
    use egui::TextStyle;
    let pad = ui.spacing().button_padding;
    let gap = ui.spacing().item_spacing.x;
    let height = ui.text_style_height(&TextStyle::Monospace) + 2.0 * pad.y;
    let (response, rect) = popup::selectable_row(ui, selected, height);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let mut inner = rect.shrink2(pad);
    inner.min.x += 2.0;
    let painter = ui.painter_at(rect);
    painter.circle_filled(egui::pos2(inner.left() + 5.0, rect.center().y), 4.0, color);
    inner.min.x += 10.0 + gap;
    let text = if selected {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    // Weak text would vanish on the selection background.
    let side_text = || {
        let t = RichText::new(connection);
        if selected {
            t.color(text)
        } else {
            t.weak()
        }
    };
    let mut side = popup::one_line(ui, side_text(), f32::INFINITY, TextStyle::Body);
    let (name_width, side_width) = popup::split_row(inner.width(), side.size().x, 2.0 * gap);
    if side_width < side.size().x {
        side = popup::one_line(ui, side_text(), side_width, TextStyle::Body);
    }
    let galley = popup::one_line(
        ui,
        RichText::new(name).monospace(),
        name_width,
        TextStyle::Monospace,
    );
    let elided = galley.elided || side.elided;
    let y = |g: &egui::Galley| rect.center().y - g.size().y / 2.0;
    painter.galley(egui::pos2(inner.left(), y(&galley)), galley, text);
    painter.galley(
        egui::pos2(inner.right() - side.size().x, y(&side)),
        side,
        text,
    );
    if elided {
        response.on_hover_text(format!("{name} — {connection}"))
    } else {
        response
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

    // --- The modal, through egui -------------------------------------------

    use crate::gui::ui_test::{click, key, Driver};

    fn search() -> TableSearch {
        let t = |c: &str, s: &str, n: &str| (c.to_string(), s.to_string(), n.to_string());
        // Listed by name: log (3), orders (1), users (0).
        TableSearch::new(vec![
            t("dev", "main", "users"),
            t("prod", "public", "orders"),
            t("prod", "audit", "log"),
        ])
    }

    fn gray(_: &str) -> Color32 {
        Color32::GRAY
    }

    #[test]
    fn enter_opens_the_selected_table() {
        let mut s = search();
        let mut d = Driver::new();
        let mut frame = |events| d.modal(events, |ui| s.ui(ui, &gray));
        assert_eq!(frame(vec![]), ModalResponse::None);
        assert_eq!(frame(vec![key(Key::ArrowDown)]), ModalResponse::None);
        assert_eq!(frame(vec![key(Key::Enter)]), ModalResponse::Button(1));
        assert_eq!(s.table(1).unwrap().2, "orders");
    }

    #[test]
    fn open_button_opens_the_selected_table() {
        let mut s = search();
        let mut d = Driver::new();
        d.modal(vec![], |ui| s.ui(ui, &gray));
        for _ in 0..2 {
            d.modal(vec![key(Key::ArrowDown)], |ui| s.ui(ui, &gray));
        }
        let open = d.button("Ouvrir").expect("Ouvrir");
        assert!(open.enabled);
        let r = d.modal(click(open.center), |ui| s.ui(ui, &gray));
        assert_eq!(r, ModalResponse::Button(0), "users");
    }

    #[test]
    fn nothing_to_open_without_a_match() {
        let mut s = search();
        s.query = "zzz".into();
        s.matches = ranked(&s.query, &s.tables);
        let mut d = Driver::new();
        d.modal(vec![], |ui| s.ui(ui, &gray));
        assert!(!d.button("Ouvrir").unwrap().enabled);
        let r = d.modal(vec![key(Key::Enter)], |ui| s.ui(ui, &gray));
        assert_eq!(r, ModalResponse::None);
        assert_eq!(s.table(9), None);
    }
}
