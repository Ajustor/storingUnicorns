//! Query history popup (Ctrl+Alt+E): search (SQL and connection), newest
//! first, a list of one-line summaries and a preview of the whole query;
//! Enter, a double-click or "Insérer" inserts the query into the active
//! console.

use egui::{Key, Modifiers, RichText, TextStyle};
use egui_phosphor::regular as icon;

use crate::engine::services::history::{History, HistoryEntry};

use super::editor::HighlightCache;
use super::modal::{self, ModalButton, ModalResponse, ModalSize, ModalSpec};
use super::popup;
use super::theme::ERROR;

#[derive(Default)]
pub struct HistoryPopup {
    pub search: String,
    pub selected: usize,
    /// History index of the selected entry, when there is one.
    chosen: Option<usize>,
    focus_search: bool,
    results: SearchCache,
    /// Highlighted SQL of the preview pane.
    preview: HighlightCache,
}

/// Matches of the last search, kept until the query or the history change
/// (the popup is drawn every frame).
#[derive(Default)]
struct SearchCache {
    key: Option<(String, u64)>,
    /// `(entry index, summary of its SQL)`, newest first.
    matches: Vec<(usize, String)>,
}

impl SearchCache {
    fn get(&mut self, history: &History, query: &str) -> &[(usize, String)] {
        let fresh = matches!(&self.key, Some((q, g)) if q == query && *g == history.generation());
        if !fresh {
            self.matches = history
                .search_indices(query)
                .into_iter()
                .filter_map(|i| Some((i, summary(&history.get(i)?.sql))))
                .collect();
            self.key = Some((query.to_string(), history.generation()));
        }
        &self.matches
    }
}

/// Footer buttons of the history modal, acting on the selected entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryAction {
    /// Insert its SQL into the active console (and close).
    Insert,
    /// Copy its SQL to the clipboard (and stay open).
    Copy,
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

/// Characters of a summary at most (the row elides it to its width).
const SUMMARY_MAX: usize = 240;

/// One-line summary of `sql` for the list: leading `--` and `/* */`
/// comments skipped (kept when the query is nothing else), every run of
/// whitespace (line breaks, CR, tabs) collapsed to one space.
pub fn summary(sql: &str) -> String {
    // A huge script only needs its beginning.
    let head = match sql.char_indices().nth(SUMMARY_MAX * 8) {
        Some((i, _)) => &sql[..i],
        None => sql,
    };
    let code = skip_leading_comments(head);
    let source = if code.trim().is_empty() { head } else { code };
    let mut out = String::new();
    for (n, word) in source.split_whitespace().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.chars().count() > SUMMARY_MAX {
            out = out.chars().take(SUMMARY_MAX).collect();
            out.push('…');
            break;
        }
    }
    out
}

fn skip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.find('\n').map_or("", |i| &rest[i..]);
        } else if let Some(rest) = s.strip_prefix("/*") {
            match rest.find("*/") {
                Some(i) => s = &rest[i + 2..],
                None => return "",
            }
        } else {
            return s;
        }
    }
}

/// `sql` as shown in the preview: CR / CRLF line endings turned into line
/// breaks (egui draws nothing for a CR), trailing blank lines dropped.
pub fn preview_text(sql: &str) -> String {
    sql.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_string()
}

/// "connection · il y a 5 min · 12 ms" under a summary.
pub fn meta_line(e: &HistoryEntry, now: u64) -> String {
    format!(
        "{} · {} · {} ms",
        e.connection,
        relative_time(now, e.at),
        e.duration_ms
    )
}

/// Local date and time of `at` (unix seconds), "22/10/2026 10:14:03".
pub fn absolute_time(at: u64) -> String {
    chrono::DateTime::from_timestamp(at as i64, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m/%Y %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
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
            chosen: None,
            focus_search: true,
            results: SearchCache::default(),
            preview: HighlightCache::default(),
        }
    }

    /// The history modal (standard, large): "Insérer" (Enter, a
    /// double-click) and "Copier" act on the selected entry. Copying keeps
    /// the modal open; the answer to insert is the entry's index in
    /// `history`.
    pub fn ui(&mut self, ui: &mut egui::Ui, history: &History) -> ModalResponse<usize> {
        let any = !self.results.get(history, &self.search).is_empty();
        let buttons = footer(any);
        let mut activated = false;
        let response = modal::show(
            ui,
            ModalSpec::new("Historique", ModalSize::Large).icon(icon::CLOCK_COUNTER_CLOCKWISE),
            |ui| activated = self.body(ui, history),
            None,
            &buttons,
        );
        let response = match response {
            // Enter or a double-click in the list.
            ModalResponse::None if activated => ModalResponse::Button(HistoryAction::Insert),
            r => r,
        };
        let chosen = self.chosen.filter(|&i| history.get(i).is_some());
        match response {
            ModalResponse::None => ModalResponse::None,
            ModalResponse::Cancel => ModalResponse::Cancel,
            ModalResponse::Button(action) => match (action, chosen) {
                (HistoryAction::Insert, Some(i)) => ModalResponse::Button(i),
                (HistoryAction::Copy, Some(i)) => {
                    if let Some(e) = history.get(i) {
                        ui.ctx().copy_text(e.sql.clone());
                    }
                    ModalResponse::None
                }
                (_, None) => ModalResponse::None,
            },
        }
    }

    /// Search field, list and preview; true when Enter or a double-click
    /// asks to insert the selected entry.
    fn body(&mut self, ui: &mut egui::Ui, history: &History) -> bool {
        let screen = ui.ctx().screen_rect();
        let width = ui.available_width();
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text(format!(
                    "{} Rechercher dans le SQL ou le nom de connexion…",
                    icon::MAGNIFYING_GLASS
                ))
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
            self.chosen = None;
            ui.label(RichText::new("Aucune requête").weak());
            return false;
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
        let mut activated = enter;
        let count = entries.len();
        let now = now_secs();
        let height = popup::modal_list_height(screen.height());
        // Side by side when there is room, else the preview under the list.
        let side_by_side = width >= 640.0;
        let list_width = if side_by_side {
            (width * 0.45).round()
        } else {
            width
        };
        ui.separator();
        let mut list = |ui: &mut egui::Ui, selected: &mut usize| {
            ui.set_width(list_width);
            egui::ScrollArea::vertical()
                .id_salt("history_list")
                .max_height(if side_by_side { height } else { height * 0.5 })
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, (index, text)) in entries.iter().enumerate() {
                        let Some(e) = history.get(*index) else {
                            continue;
                        };
                        let r = entry_row(ui, i == *selected, text, e, now);
                        if (up || down) && i == *selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            *selected = i;
                        }
                        if r.double_clicked() {
                            activated = true;
                        }
                    }
                });
        };
        let chosen = history.get(entries[self.selected].0);
        let preview = |ui: &mut egui::Ui, cache: &mut HighlightCache| {
            if let Some(e) = chosen {
                preview_pane(
                    ui,
                    cache,
                    e,
                    if side_by_side { height } else { height * 0.5 },
                );
            }
        };
        if side_by_side {
            ui.horizontal_top(|ui| {
                // The separator spans the row's height: in the modal's
                // scrolling body, that would grow every frame.
                ui.set_max_height(height);
                ui.vertical(|ui| list(ui, &mut self.selected));
                ui.separator();
                ui.vertical(|ui| preview(ui, &mut self.preview));
            });
        } else {
            list(ui, &mut self.selected);
            ui.separator();
            preview(ui, &mut self.preview);
        }
        // A click in the list selects that entry: the buttons act on it.
        self.chosen = entries.get(self.selected).map(|&(i, _)| i);
        ui.label(
            RichText::new(format!(
                "{count} requête(s) · ↑↓ naviguer · Entrée ou double-clic : insérer · Échap : fermer"
            ))
            .small()
            .weak(),
        );
        activated
    }
}

/// Footer buttons, acting on the selected entry (none: disabled).
fn footer(any: bool) -> [ModalButton<HistoryAction>; 2] {
    let none = "Aucune requête sélectionnée";
    let button =
        |b: ModalButton<HistoryAction>, hint| b.enabled(any).hint(if any { hint } else { none });
    [
        button(
            ModalButton::secondary(format!("{} Copier", icon::COPY), HistoryAction::Copy),
            "Copier la requête dans le presse-papiers",
        ),
        button(
            ModalButton::primary(
                format!("{} Insérer", icon::ARROW_SQUARE_IN),
                HistoryAction::Insert,
            ),
            "Insérer dans la console (Entrée)",
        ),
    ]
}

/// One entry of the list: the summary (error entries marked) over
/// "connection · relative time · duration".
fn entry_row(
    ui: &mut egui::Ui,
    selected: bool,
    text: &str,
    e: &HistoryEntry,
    now: u64,
) -> egui::Response {
    let pad = ui.spacing().button_padding;
    let mono = ui.text_style_height(&TextStyle::Monospace);
    let small = ui.text_style_height(&TextStyle::Small);
    let (response, rect) = popup::selectable_row(ui, selected, mono + small + 2.0 * pad.y + 2.0);
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let color = if selected {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    // Clear of the selection bar.
    let mut inner = rect.shrink2(pad);
    inner.min.x += 2.0;
    let mut x = inner.left();
    let painter = ui.painter_at(rect);
    if !e.ok {
        let mark = popup::one_line(
            ui,
            RichText::new(icon::WARNING_CIRCLE).color(ERROR),
            inner.width(),
            TextStyle::Body,
        );
        let w = mark.size().x + 4.0;
        painter.galley(egui::pos2(x, inner.top()), mark, ERROR);
        x += w;
    }
    let sql = RichText::new(text).monospace();
    let sql = if e.ok { sql } else { sql.color(ERROR) };
    let line = popup::one_line(ui, sql, inner.right() - x, TextStyle::Monospace);
    painter.galley(egui::pos2(x, inner.top()), line, color);
    let meta = RichText::new(meta_line(e, now)).small();
    let meta = popup::one_line(
        ui,
        if selected {
            meta.color(color)
        } else {
            meta.weak()
        },
        inner.width(),
        TextStyle::Small,
    );
    painter.galley(
        egui::pos2(inner.left(), inner.top() + mono + 2.0),
        meta,
        color,
    );
    response
}

/// Connection, date, duration and status of `e`, then its whole SQL,
/// highlighted, in a scroll area of `height` (the footer's buttons act on
/// it).
fn preview_pane(ui: &mut egui::Ui, cache: &mut HighlightCache, e: &HistoryEntry, height: f32) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&e.connection).strong());
        ui.label(RichText::new(format!("· {} · {} ms", absolute_time(e.at), e.duration_ms)).weak());
        if e.ok {
            ui.label(RichText::new(format!("{} OK", icon::CHECK_CIRCLE)).weak());
        } else {
            ui.label(RichText::new(format!("{} Erreur", icon::WARNING_CIRCLE)).color(ERROR));
        }
    });
    let text = preview_text(&e.sql);
    let header = ui.text_style_height(&TextStyle::Body) + ui.spacing().item_spacing.y;
    egui::ScrollArea::both()
        .id_salt("history_preview")
        .max_height((height - header).max(60.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let galley = cache.layout(ui, &text, &[], ui.available_width());
            ui.add(egui::Label::new(galley).selectable(true));
        });
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

    fn entry(sql: &str) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            connection: "c".into(),
            at: 0,
            duration_ms: 1,
            ok: true,
        }
    }

    #[test]
    fn search_is_cached_until_the_query_or_the_history_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::load_from(dir.path().join("h.json"));
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
            ["select 2 FROM t", "SELECT 1"]
        );
        let key = cache.key.clone();
        assert_eq!(
            found(&mut cache, &history, "SELECT"),
            ["select 2 FROM t", "SELECT 1"]
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

    #[test]
    fn summary_joins_the_lines_and_collapses_whitespace() {
        assert_eq!(
            summary(
                "SELECT\r\n\tid,\r\n\tnom AS \"prénom\"\r\nFROM clients\r\nWHERE nom LIKE 'Zoë%';"
            ),
            "SELECT id, nom AS \"prénom\" FROM clients WHERE nom LIKE 'Zoë%';"
        );
        assert_eq!(summary("  SELECT   1  "), "SELECT 1");
        assert!(!summary("a\rb").contains('\r'), "no CR left");
    }

    #[test]
    fn summary_skips_leading_comments() {
        assert_eq!(summary("-- requête commentée\nSELECT 1"), "SELECT 1");
        assert_eq!(
            summary("/* en-tête\n sur deux lignes */\n-- puis\nSELECT 1"),
            "SELECT 1"
        );
        assert_eq!(
            summary("SELECT 1 -- fin"),
            "SELECT 1 -- fin",
            "only leading ones"
        );
        assert_eq!(
            summary("-- rien d'autre"),
            "-- rien d'autre",
            "kept when alone"
        );
        assert_eq!(summary("/* pas fermé SELECT 1"), "/* pas fermé SELECT 1");
    }

    #[test]
    fn summary_is_bounded() {
        let long = "SELECT ".to_string() + &"colonne, ".repeat(10_000);
        let s = summary(&long);
        assert_eq!(s.chars().count(), SUMMARY_MAX + 1);
        assert!(s.ends_with('…'));
        assert_eq!(
            summary("é".repeat(10).as_str()),
            "é".repeat(10),
            "char boundaries"
        );
    }

    #[test]
    fn preview_text_turns_carriage_returns_into_line_breaks() {
        assert_eq!(
            preview_text("SELECT 1\r\nFROM t\rWHERE x\n\n"),
            "SELECT 1\nFROM t\nWHERE x"
        );
        assert_eq!(
            preview_text("\tSELECT 'é 🦄'"),
            "\tSELECT 'é 🦄'",
            "tabs and text kept"
        );
    }

    #[test]
    fn meta_line_names_the_connection_time_and_duration() {
        let mut e = entry("SELECT 1");
        e.connection = "Démo SQLite".into();
        e.at = 1_000;
        e.duration_ms = 12;
        assert_eq!(meta_line(&e, 1_300), "Démo SQLite · il y a 5 min · 12 ms");
    }

    // --- The modal, through egui -------------------------------------------

    use crate::gui::ui_test::{click, key, Driver};

    /// A history of three queries; the indexes of "c", "b", "a" (newest
    /// first, as listed).
    fn three() -> (tempfile::TempDir, History, Vec<usize>) {
        let dir = tempfile::tempdir().unwrap();
        let mut history = History::load_from(dir.path().join("h.json"));
        for sql in ["SELECT 'a'", "SELECT 'b'", "SELECT 'c'"] {
            history.push(entry(sql));
        }
        let listed = history.search_indices("");
        assert_eq!(history.get(listed[0]).unwrap().sql, "SELECT 'c'");
        (dir, history, listed)
    }

    #[test]
    fn enter_inserts_the_selected_entry() {
        let (_dir, history, listed) = three();
        let mut popup = HistoryPopup::new(String::new());
        let mut d = Driver::new();
        let mut frame = |events| d.modal(events, |ui| popup.ui(ui, &history));
        assert_eq!(frame(vec![]), ModalResponse::None);
        assert_eq!(frame(vec![key(Key::ArrowDown)]), ModalResponse::None);
        assert_eq!(
            frame(vec![key(Key::Enter)]),
            ModalResponse::Button(listed[1]),
            "the list's Enter inserts, once"
        );
    }

    #[test]
    fn footer_buttons_act_on_the_selected_entry() {
        let (_dir, history, listed) = three();
        let mut popup = HistoryPopup::new(String::new());
        let mut d = Driver::new();
        d.modal(vec![], |ui| popup.ui(ui, &history));
        d.modal(vec![key(Key::ArrowDown)], |ui| popup.ui(ui, &history));
        // Copier copies the selected query and keeps the modal open.
        let copy = d.button("Copier").expect("Copier");
        assert!(copy.enabled);
        let r = d.modal(click(copy.center), |ui| popup.ui(ui, &history));
        assert_eq!(r, ModalResponse::None);
        assert_eq!(d.copied(), ["SELECT 'b'"]);
        let insert = d.button("Insérer").expect("Insérer");
        let r = d.modal(click(insert.center), |ui| popup.ui(ui, &history));
        assert_eq!(r, ModalResponse::Button(listed[1]));
        let cancel = d.button("Annuler").expect("Annuler");
        let r = d.modal(click(cancel.center), |ui| popup.ui(ui, &history));
        assert_eq!(r, ModalResponse::Cancel);
    }

    #[test]
    fn nothing_to_insert_without_a_match() {
        let (_dir, history, _) = three();
        let mut popup = HistoryPopup::new("introuvable".into());
        let mut d = Driver::new();
        d.modal(vec![], |ui| popup.ui(ui, &history));
        for label in ["Insérer", "Copier"] {
            assert!(!d.button(label).expect(label).enabled, "{label}");
        }
        let r = d.modal(vec![key(Key::Enter)], |ui| popup.ui(ui, &history));
        assert_eq!(r, ModalResponse::None);
    }

    #[test]
    fn absolute_time_is_a_local_date() {
        let s = absolute_time(1_700_000_000);
        assert_eq!(s.len(), "14/11/2023 22:13:20".len(), "{s}");
        assert!(s.contains("/2023 "), "{s}");
    }
}
