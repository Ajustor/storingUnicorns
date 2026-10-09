//! Table data editor: one page of a table at a time, server-side filter and
//! ordering, inline edits submitted in one transaction.

use std::sync::Arc;

use egui::{Galley, Key, RichText};
use egui_phosphor::regular as icon;

use crate::engine::models::QueryResult;
use crate::engine::ops::query::has_full_key;
use crate::engine::ops::rows::NO_PRIMARY_KEY;
use crate::engine::sql::paging::{toggle_order, DataQuery};
use crate::gui::editor::highlight_job;
use crate::gui::grid::changes::PendingEdits;
use crate::gui::grid::{self, GridAction, GridOptions, GridState};
use crate::gui::status::one_line_label;
use crate::gui::theme::ERROR;
use crate::gui::worker::{Event, TabId};

pub const PAGE_SIZE: usize = 500;

/// Number of pages of `total` rows (at least one, even when empty).
pub fn page_count(total: u64, page_size: usize) -> usize {
    let size = page_size.max(1) as u64;
    total.div_ceil(size).max(1) as usize
}

/// Why a grid is read-only for now: its edits are being submitted, or its
/// rows are about to be replaced. Edits made meanwhile would be lost (a
/// successful submit clears the edits, a new page drops them).
pub fn busy_hint(submitting: bool, loading: bool) -> Option<&'static str> {
    if submitting {
        Some("Envoi en cours…")
    } else if loading {
        Some("Chargement…")
    } else {
        None
    }
}

/// Whether the page can be replaced without losing pending edits.
pub fn can_leave(edits: &PendingEdits) -> bool {
    edits.is_empty()
}

/// `name` between the database's identifier quotes (closing quote doubled).
pub fn quote_ident(name: &str, (q0, q1): (char, char)) -> String {
    format!("{q0}{}{q1}", name.replace(q1, &format!("{q1}{q1}")))
}

/// Sort arrow for an ORDER BY built by header clicks (`col ASC` / `col
/// DESC`, see `toggle_order`): column index and ascending. A hand-written
/// ordering shows no arrow.
pub fn order_indicator(order_by: &str, quoted_columns: &[String]) -> Option<(usize, bool)> {
    let order = order_by.trim();
    quoted_columns.iter().enumerate().find_map(|(i, c)| {
        if order.eq_ignore_ascii_case(&format!("{c} ASC")) {
            Some((i, true))
        } else if order.eq_ignore_ascii_case(&format!("{c} DESC")) {
            Some((i, false))
        } else {
            None
        }
    })
}

/// A change of the displayed rows, guarded by the pending edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Nav {
    Page(usize),
    Refresh,
    /// Apply the filter and ordering inputs.
    Apply,
    /// Header click: the new ORDER BY.
    Sort(String),
}

/// What the tab asks the app to do (it owns the worker and the sessions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataAction {
    /// Load `query`'s page, and count its rows again when `count`.
    Load { count: bool },
    /// Submit the pending edits.
    Submit,
    /// Ask "Abandonner les modifications ?" for `confirm`.
    Confirm,
    /// Open the DDL tab of the table.
    Ddl,
    /// Submit succeeded with this many changes: report it and reload.
    Applied(usize),
    /// Submit failed (rolled back): report it.
    SubmitFailed(String),
    /// Export the current page.
    Export,
}

pub struct DataTab {
    /// Qualified, quoted table name.
    pub table: String,
    pub query: DataQuery,
    /// WHERE editor, applied on Entrée.
    pub filter_input: String,
    /// ORDER BY editor, applied on Entrée.
    pub order_input: String,
    pub result: Option<QueryResult>,
    /// Rows matching the filter; `None` while counting.
    pub total: Option<u64>,
    pub grid: GridState,
    pub loading: bool,
    /// Last database error (page load or submit), shown above the grid.
    pub error: Option<String>,
    /// Identifier quotes of the connection.
    pub quotes: (char, char),
    /// Navigation waiting for the "Abandonner les modifications ?" answer.
    pub confirm: Option<Nav>,
    /// Navigation to apply once the running submit succeeds.
    pub after_submit: Option<Nav>,
    /// Close the tab once the running submit succeeds ("Submit" in the
    /// confirmation of its closing).
    pub close_after_submit: bool,
    /// No page requested yet (opened while disconnected…).
    pub needs_load: bool,
    /// Column names of the result (highlighting of the filter bar).
    columns: Vec<String>,
    /// Sort arrow matching `query.order_by`.
    sort: Option<(usize, bool)>,
}

/// What the tab needs from the app to draw itself.
pub struct DataContext {
    pub tab: TabId,
    pub connected: bool,
    pub submitting: bool,
    /// A page load is pending.
    pub loading: bool,
}

impl DataTab {
    pub fn new(table: String, quotes: (char, char)) -> Self {
        Self {
            query: DataQuery {
                table: table.clone(),
                filter: String::new(),
                order_by: String::new(),
                page: 0,
                page_size: PAGE_SIZE,
            },
            table,
            filter_input: String::new(),
            order_input: String::new(),
            result: None,
            total: None,
            grid: GridState::default(),
            loading: false,
            error: None,
            quotes,
            confirm: None,
            after_submit: None,
            close_after_submit: false,
            needs_load: true,
            columns: Vec::new(),
            sort: None,
        }
    }

    /// Number of pages when the total is known.
    pub fn pages(&self) -> Option<usize> {
        self.total.map(|t| page_count(t, self.query.page_size))
    }

    /// Status bar text: "page 2/9 · 500 lignes · 12 ms".
    pub fn summary(&self) -> Option<String> {
        let r = self.result.as_ref()?;
        let pages = self
            .pages()
            .map_or_else(|| "?".to_string(), |p| p.to_string());
        let n = r.rows.len();
        Some(format!(
            "page {}/{pages} · {n} {} · {} ms",
            self.query.page + 1,
            if n == 1 { "ligne" } else { "lignes" },
            r.execution_time_ms
        ))
    }

    /// Write the open cell editor's text into the pending edits.
    pub fn commit_editor(&mut self) {
        if let Some(r) = &self.result {
            self.grid.commit_editing(&r.rows);
        }
    }

    /// Request `nav`, or ask for confirmation first when edits are pending
    /// (the text of an open cell editor included).
    pub fn navigate(&mut self, nav: Nav) -> DataAction {
        self.commit_editor();
        if can_leave(&self.grid.edits) {
            self.apply(nav)
        } else {
            self.confirm = Some(nav);
            DataAction::Confirm
        }
    }

    /// "Abandonner": drop the edits and do the navigation that was asked.
    pub fn discard_and_continue(&mut self) -> Option<DataAction> {
        let nav = self.confirm.take()?;
        self.grid.editing = None;
        self.grid.edits.clear();
        Some(self.apply(nav))
    }

    /// "Submit" in the confirmation: submit, then navigate on success.
    pub fn submit_and_continue(&mut self) -> DataAction {
        self.after_submit = self.confirm.take();
        DataAction::Submit
    }

    /// Change the query for `nav`; returns the load to run.
    fn apply(&mut self, nav: Nav) -> DataAction {
        match nav {
            Nav::Page(p) => {
                self.query.page = p;
                DataAction::Load { count: false }
            }
            Nav::Refresh => DataAction::Load { count: true },
            Nav::Apply => {
                self.query.filter = self.filter_input.trim().to_string();
                self.query.order_by = self.order_input.trim().to_string();
                self.query.page = 0;
                DataAction::Load { count: true }
            }
            Nav::Sort(order) => {
                self.order_input = order.clone();
                self.query.order_by = order;
                self.query.page = 0;
                DataAction::Load { count: false }
            }
        }
    }

    /// Outcomes of this tab's page loads, counts and submits.
    pub fn on_event(&mut self, ev: Event) -> Option<DataAction> {
        match ev {
            Event::Page { outcome, .. } => {
                self.loading = false;
                match outcome {
                    Ok(r) => {
                        self.columns = r.columns.iter().map(|c| c.name.clone()).collect();
                        let quoted: Vec<String> = self
                            .columns
                            .iter()
                            .map(|c| quote_ident(c, self.quotes))
                            .collect();
                        self.sort = order_indicator(&self.query.order_by, &quoted);
                        self.result = Some(r);
                        self.grid.new_result();
                        self.error = None;
                    }
                    // The previous page stays visible under the error.
                    Err(e) => self.error = Some(e),
                }
                None
            }
            Event::Count { outcome, .. } => {
                self.total = outcome.ok();
                None
            }
            Event::Submitted { outcome, .. } => match outcome {
                Ok(n) => {
                    self.grid.editing = None;
                    self.grid.edits.clear();
                    self.error = None;
                    if let Some(nav) = self.after_submit.take() {
                        self.apply(nav);
                    }
                    Some(DataAction::Applied(n))
                }
                Err(e) => {
                    self.after_submit = None;
                    self.close_after_submit = false;
                    self.error = Some(format!("Submit annulé (transaction annulée) : {e}"));
                    Some(DataAction::SubmitFailed(e))
                }
            },
            _ => None,
        }
    }

    /// Rows can be edited: the table has a primary key, all of it shown.
    pub fn editable(&self) -> bool {
        self.result.as_ref().is_some_and(has_full_key)
    }

    /// Why a loaded page is read-only.
    pub fn read_only_hint(&self) -> Option<&'static str> {
        (self.result.is_some() && !self.editable()).then_some(NO_PRIMARY_KEY)
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: DataContext) -> Option<DataAction> {
        if self.needs_load && cx.connected && !self.loading {
            return Some(DataAction::Load { count: true });
        }
        let mut action = self.toolbar(ui, &cx);
        if let Some(a) = self.filter_bar(ui, &cx) {
            action = Some(a);
        }
        if let Some(e) = &self.error {
            egui::Frame::new()
                .fill(ERROR.gamma_multiply(0.18))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    one_line_label(ui, &format!("{} {e}", icon::WARNING), ERROR);
                });
        }
        ui.separator();
        if let Some(hint) = self.read_only_hint() {
            ui.label(RichText::new(format!("{} {hint}", icon::LOCK)).weak());
        }
        let busy = busy_hint(cx.submitting, cx.loading);
        if let (Some(hint), true) = (busy, self.editable()) {
            ui.label(RichText::new(format!("{} {hint}", icon::HOURGLASS)).weak());
        }
        let editable = self.editable() && busy.is_none();
        let Some(result) = &self.result else {
            if self.loading {
                ui.spinner();
            }
            return action;
        };
        let grid_action = grid::show(
            ui,
            egui::Id::new(("data_grid", cx.tab)),
            &mut self.grid,
            GridOptions {
                result,
                editable,
                server_sort: true,
                sort_indicator: self.sort,
            },
        );
        match grid_action {
            GridAction::SortBy(col) => {
                if let Some(name) = self.columns.get(col) {
                    let order = toggle_order(&self.query.order_by, &quote_ident(name, self.quotes));
                    action = Some(self.navigate(Nav::Sort(order)));
                }
            }
            GridAction::Submit if !self.grid.edits.is_empty() && !cx.submitting => {
                action = Some(DataAction::Submit);
            }
            GridAction::Export => action = Some(DataAction::Export),
            _ => {}
        }
        action
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &DataContext) -> Option<DataAction> {
        let mut nav = None;
        let mut action = None;
        ui.horizontal(|ui| {
            let can = cx.connected && !cx.submitting;
            if ui
                .add_enabled(can, egui::Button::new(icon::ARROW_CLOCKWISE))
                .on_hover_text("Rafraîchir (Ctrl+R)")
                .clicked()
            {
                nav = Some(Nav::Refresh);
            }
            ui.separator();
            let page = self.query.page;
            let pages = self.pages();
            let full_page = self
                .result
                .as_ref()
                .is_some_and(|r| r.rows.len() >= self.query.page_size);
            let has_next = match pages {
                Some(p) => page + 1 < p,
                None => full_page,
            };
            let button = |ui: &mut egui::Ui, text: &str, enabled: bool, hint: &str| {
                ui.add_enabled(can && enabled, egui::Button::new(text))
                    .on_hover_text(hint)
                    .clicked()
            };
            if button(ui, icon::CARET_LINE_LEFT, page > 0, "Première page") {
                nav = Some(Nav::Page(0));
            }
            if button(ui, icon::CARET_LEFT, page > 0, "Page précédente") {
                nav = Some(Nav::Page(page - 1));
            }
            let total = pages.map_or_else(|| "?".to_string(), |p| p.to_string());
            ui.label(format!("page {} / {total}", page + 1))
                .on_hover_text(match self.total {
                    Some(t) => format!("{t} ligne(s)"),
                    None => "Comptage…".to_string(),
                });
            if button(ui, icon::CARET_RIGHT, has_next, "Page suivante") {
                nav = Some(Nav::Page(page + 1));
            }
            let last = pages.filter(|&p| page + 1 < p);
            if button(ui, icon::CARET_LINE_RIGHT, last.is_some(), "Dernière page") {
                if let Some(p) = last {
                    nav = Some(Nav::Page(p - 1));
                }
            }
            ui.label(RichText::new(format!("{} lignes/page", self.query.page_size)).weak());
            if self.loading || cx.submitting {
                ui.spinner();
            }
            ui.separator();
            if ui
                .add_enabled(
                    cx.connected,
                    egui::Button::new(format!("{} DDL", icon::CODE)),
                )
                .on_hover_text("CREATE TABLE")
                .clicked()
            {
                action = Some(DataAction::Ddl);
            }
        });
        match nav {
            Some(nav) => Some(self.navigate(nav)),
            None => action,
        }
    }

    /// `WHERE [ … ]  ORDER BY [ … ]`; Entrée applies both.
    fn filter_bar(&mut self, ui: &mut egui::Ui, cx: &DataContext) -> Option<DataAction> {
        let dark = ui.visuals().dark_mode;
        let size = egui::TextStyle::Monospace.resolve(ui.style()).size;
        let columns = &self.columns;
        let mut layouter = |ui: &egui::Ui, text: &str, _wrap: f32| -> Arc<Galley> {
            ui.fonts(|f| f.layout_job(highlight_job(text, columns, dark, size)))
        };
        let mut apply = false;
        ui.horizontal(|ui| {
            let width = ((ui.available_width() - 140.0) / 2.0).max(80.0);
            ui.label(RichText::new("WHERE").monospace().strong());
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.filter_input)
                    .id(egui::Id::new(("data_where", cx.tab)))
                    .code_editor()
                    .desired_width(width)
                    .hint_text("id > 100")
                    .layouter(&mut layouter),
            );
            apply |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.label(RichText::new("ORDER BY").monospace().strong());
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.order_input)
                    .id(egui::Id::new(("data_order", cx.tab)))
                    .code_editor()
                    .desired_width(width)
                    .hint_text("id DESC")
                    .layouter(&mut layouter),
            );
            apply |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        });
        (apply && cx.connected && !cx.submitting).then(|| self.navigate(Nav::Apply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::Column;
    use crate::gui::grid::changes::RowRef;

    fn column(name: &str) -> Column {
        Column {
            name: name.into(),
            type_name: "INTEGER".into(),
            nullable: false,
            is_primary_key: name == "id",
        }
    }

    fn page(rows: usize) -> QueryResult {
        QueryResult {
            columns: vec![column("id"), column("name")],
            rows: (0..rows).map(|i| vec![i.to_string(), "x".into()]).collect(),
            rows_affected: 0,
            execution_time_ms: 12,
            truncated: false,
            primary_key: vec!["id".into()],
        }
    }

    #[test]
    fn table_without_primary_key_is_read_only() {
        let mut t = DataTab::new("\"main\".\"t\"".into(), ('"', '"'));
        assert!(!t.editable(), "nothing loaded");
        t.on_event(page_event(Ok(page(2))));
        assert!(t.editable());
        assert_eq!(t.read_only_hint(), None);
        let mut unkeyed = page(2);
        unkeyed.primary_key.clear();
        for c in &mut unkeyed.columns {
            c.is_primary_key = false;
        }
        t.on_event(page_event(Ok(unkeyed)));
        assert!(!t.editable());
        assert_eq!(
            t.read_only_hint(),
            Some("Lecture seule : pas de clé primaire")
        );
    }

    fn page_event(outcome: Result<QueryResult, String>) -> Event {
        Event::Page {
            tab: 1,
            run: 1,
            outcome,
        }
    }

    fn tab_with_page() -> DataTab {
        let mut t = DataTab::new("\"main\".\"t\"".into(), ('"', '"'));
        t.on_event(page_event(Ok(page(3))));
        t
    }

    fn edit(t: &mut DataTab) {
        let rows = t.result.as_ref().unwrap().rows.clone();
        t.grid.edits.set(&rows, RowRef::Base(0), 1, "y".into());
    }

    #[test]
    fn page_count_rounds_up_with_at_least_one_page() {
        assert_eq!(page_count(0, 500), 1);
        assert_eq!(page_count(1, 500), 1);
        assert_eq!(page_count(500, 500), 1);
        assert_eq!(page_count(501, 500), 2);
        assert_eq!(page_count(4321, 500), 9);
        assert_eq!(page_count(10, 0), 10, "no division by zero");
    }

    #[test]
    fn grid_is_read_only_while_submitting_or_loading() {
        assert_eq!(busy_hint(false, false), None);
        assert_eq!(busy_hint(true, false), Some("Envoi en cours…"));
        assert_eq!(busy_hint(false, true), Some("Chargement…"));
        assert_eq!(busy_hint(true, true), Some("Envoi en cours…"));
    }

    #[test]
    fn can_leave_only_without_pending_edits() {
        let mut edits = PendingEdits::default();
        assert!(can_leave(&edits));
        edits.add_row(2);
        assert!(!can_leave(&edits));
    }

    #[test]
    fn quote_ident_doubles_the_closing_quote() {
        assert_eq!(quote_ident("a", ('"', '"')), "\"a\"");
        assert_eq!(quote_ident("a]b", ('[', ']')), "[a]]b]");
        assert_eq!(quote_ident("a`b", ('`', '`')), "`a``b`");
    }

    #[test]
    fn order_indicator_reads_header_orderings_only() {
        let cols = ["\"id\"".to_string(), "\"name\"".to_string()];
        assert_eq!(order_indicator("\"name\" ASC", &cols), Some((1, true)));
        assert_eq!(order_indicator(" \"id\" desc ", &cols), Some((0, false)));
        assert_eq!(order_indicator("", &cols), None);
        assert_eq!(order_indicator("\"id\" DESC, \"name\"", &cols), None);
        assert_eq!(order_indicator("id ASC", &cols), None, "not quoted");
    }

    #[test]
    fn navigation_changes_the_query() {
        let mut t = tab_with_page();
        assert_eq!(t.navigate(Nav::Page(3)), DataAction::Load { count: false });
        assert_eq!(t.query.page, 3);
        assert_eq!(t.navigate(Nav::Refresh), DataAction::Load { count: true });
        assert_eq!(t.query.page, 3);

        t.filter_input = " id > 100 ".into();
        t.order_input = "name".into();
        assert_eq!(t.navigate(Nav::Apply), DataAction::Load { count: true });
        assert_eq!(
            (
                t.query.filter.as_str(),
                t.query.order_by.as_str(),
                t.query.page
            ),
            ("id > 100", "name", 0)
        );

        t.query.page = 2;
        assert_eq!(
            t.navigate(Nav::Sort("\"id\" ASC".into())),
            DataAction::Load { count: false }
        );
        assert_eq!(t.query.order_by, "\"id\" ASC");
        assert_eq!(t.order_input, "\"id\" ASC");
        assert_eq!(t.query.page, 0);
    }

    #[test]
    fn pending_edits_need_confirmation_to_leave() {
        let mut t = tab_with_page();
        edit(&mut t);
        assert_eq!(t.navigate(Nav::Page(1)), DataAction::Confirm);
        assert_eq!(t.query.page, 0, "nothing moves before the answer");
        assert_eq!(t.confirm, Some(Nav::Page(1)));

        assert_eq!(
            t.discard_and_continue(),
            Some(DataAction::Load { count: false })
        );
        assert_eq!(t.query.page, 1);
        assert!(t.grid.edits.is_empty());
        assert_eq!(t.discard_and_continue(), None, "answered once");
    }

    #[test]
    fn an_open_cell_editor_counts_as_pending_edits() {
        let mut t = tab_with_page();
        t.grid.editing = Some((RowRef::Base(0), 1, "typed".into()));
        assert_eq!(t.navigate(Nav::Refresh), DataAction::Confirm);
        assert!(t.grid.editing.is_none());
        let rows = &t.result.as_ref().unwrap().rows;
        assert_eq!(t.grid.edits.value(rows, RowRef::Base(0), 1), "typed");
    }

    #[test]
    fn failed_submit_cancels_the_close_it_was_for() {
        let mut t = tab_with_page();
        edit(&mut t);
        t.close_after_submit = true;
        t.on_event(Event::Submitted {
            tab: 1,
            run: 2,
            outcome: Err("x".into()),
        });
        assert!(!t.close_after_submit);
    }

    #[test]
    fn submit_then_navigate_on_success_only() {
        let mut t = tab_with_page();
        edit(&mut t);
        t.navigate(Nav::Page(1));
        assert_eq!(t.submit_and_continue(), DataAction::Submit);
        assert_eq!(t.confirm, None);
        let failed = t.on_event(Event::Submitted {
            tab: 1,
            run: 2,
            outcome: Err("NOT NULL".into()),
        });
        assert_eq!(failed, Some(DataAction::SubmitFailed("NOT NULL".into())));
        assert!(!t.grid.edits.is_empty(), "edits kept");
        assert!(t.error.as_deref().unwrap().contains("NOT NULL"));
        assert_eq!(t.query.page, 0, "navigation dropped");

        edit(&mut t);
        t.navigate(Nav::Page(1));
        t.submit_and_continue();
        let ok = t.on_event(Event::Submitted {
            tab: 1,
            run: 3,
            outcome: Ok(1),
        });
        assert_eq!(ok, Some(DataAction::Applied(1)));
        assert!(t.grid.edits.is_empty());
        assert_eq!(t.error, None);
        assert_eq!(t.query.page, 1);
    }

    #[test]
    fn page_error_keeps_the_previous_page() {
        let mut t = tab_with_page();
        t.loading = true;
        t.on_event(page_event(Err("no such column".into())));
        assert!(!t.loading);
        assert_eq!(t.result.as_ref().unwrap().rows.len(), 3);
        assert_eq!(t.error.as_deref(), Some("no such column"));
        t.on_event(page_event(Ok(page(2))));
        assert_eq!(t.error, None);
        assert_eq!(t.result.as_ref().unwrap().rows.len(), 2);
    }

    #[test]
    fn page_sets_sort_and_summary() {
        let mut t = DataTab::new("\"t\"".into(), ('"', '"'));
        assert_eq!(t.summary(), None);
        t.query.order_by = "\"name\" DESC".into();
        t.query.page = 1;
        t.on_event(page_event(Ok(page(500))));
        assert_eq!(t.sort, Some((1, false)));
        assert_eq!(t.summary().unwrap(), "page 2/? · 500 lignes · 12 ms");
        t.on_event(Event::Count {
            tab: 1,
            run: 4,
            outcome: Ok(4321),
        });
        assert_eq!(t.summary().unwrap(), "page 2/9 · 500 lignes · 12 ms");
    }
}
