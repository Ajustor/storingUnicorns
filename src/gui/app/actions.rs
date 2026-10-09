//! What the data and console tabs ask the app to do.

use crate::engine::sql::paging::{build_count, build_select};
use crate::gui::dialogs::{transfer, Dialog};
use crate::gui::history_popup::HistoryPopup;
use crate::gui::status::{Status, StatusKind};
use crate::gui::tabs::console::{self, ConsoleAction, RunKind};
use crate::gui::tabs::data::DataAction;
use crate::gui::tabs::{TabId, TabKind};

use super::App;

impl App {
    /// Apply what data tab `id` asked for.
    pub fn data_action(&mut self, id: TabId, action: DataAction) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Data(d) = &mut tab.kind else {
            return;
        };
        match action {
            DataAction::Load { count } => {
                let Some(session) = self.sessions.get(&tab.connection) else {
                    return;
                };
                let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
                d.needs_load = false;
                d.loading = true;
                let sql = build_select(&d.query, &db_type);
                tab.runs.page = Some(
                    self.worker
                        .load_page(id, conn.clone(), d.table.clone(), sql),
                );
                if count {
                    d.total = None;
                    d.count_error = None;
                    tab.runs.count = Some(self.worker.count(id, conn, build_count(&d.query)));
                }
            }
            DataAction::Submit => {
                if tab.runs.submit.is_some() || d.grid.edits.is_empty() {
                    d.after_submit = None;
                    d.close_after_submit = false;
                    return;
                }
                let Some(result) = &d.result else {
                    return;
                };
                let Some(session) = self.sessions.get(&tab.connection) else {
                    d.after_submit = None;
                    d.close_after_submit = false;
                    let text = format!("{} n'est pas connectée", tab.connection);
                    self.error(text);
                    return;
                };
                let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
                let run = self.worker.submit(
                    id,
                    conn,
                    db_type,
                    d.table.clone(),
                    result.columns.clone(),
                    d.grid.edits.to_row_changes(&result.rows),
                );
                d.error = None;
                tab.runs.submit = Some(run);
                tab.summary = Some("Submit…".into());
            }
            DataAction::Confirm => self.dialog = Some(Dialog::DiscardEdits(id)),
            DataAction::Ddl => {
                let (connection, table, title) =
                    (tab.connection.clone(), d.table.clone(), tab.title.clone());
                self.open_ddl(&connection, &table, &title);
            }
            DataAction::Applied(n) => {
                let close = std::mem::take(&mut d.close_after_submit);
                self.success(format!("{n} modification(s) appliquée(s)"));
                if close {
                    self.close_tab_id(id);
                } else {
                    self.data_action(id, DataAction::Load { count: true });
                }
            }
            DataAction::SubmitFailed(e) => {
                self.error(format!("Submit annulé (transaction annulée) : {e}"))
            }
            DataAction::Export => {
                let Some(result) = d.result.clone() else {
                    return;
                };
                let (connection, table) = (tab.connection.clone(), d.table.clone());
                transfer::export_result(self, &connection, result, table);
            }
        }
    }

    /// Apply what the console at `index` asked for.
    pub(super) fn console_action(&mut self, index: usize, action: ConsoleAction) {
        let Some(tab) = self.tabs.list.get_mut(index) else {
            return;
        };
        let TabKind::Console(c) = &mut tab.kind else {
            return;
        };
        match action {
            ConsoleAction::Run(kind) => {
                // The run replaces the unpinned results: ask before
                // dropping their pending edits.
                if c.rerun_loses_edits() {
                    self.dialog = Some(Dialog::ConfirmRerun { id: tab.id, kind });
                    return;
                }
                let id = tab.id;
                self.run_console(id, kind);
            }
            ConsoleAction::Cancel => {
                self.worker.cancel(tab.id);
                tab.runs.script = None;
                c.refreshing = None;
                c.cancelled();
                tab.summary = Some("Annulé".into());
            }
            ConsoleAction::Rebind(name) => {
                tab.connection = name;
                c.completion = Default::default();
                self.consoles_changed();
            }
            ConsoleAction::History(search) => {
                self.dialog = Some(Dialog::History(HistoryPopup::new(search)));
            }
            ConsoleAction::Export {
                result,
                table,
                connection,
            } => transfer::export_result(self, &connection, result, table),
            ConsoleAction::Submit {
                result_index,
                table,
                columns,
                changes,
            } => {
                if tab.runs.submit.is_some() {
                    return;
                }
                let Some(r) = c.results.get_mut(result_index) else {
                    return;
                };
                // The connection the rows were read from, not the console's
                // current one (it may have been rebound since).
                let open = |name: &str| self.sessions.open.contains_key(name);
                let session = match console::result_connection(r, open) {
                    Ok(name) => &self.sessions.open[name],
                    Err(e) => {
                        self.status = Status {
                            text: e,
                            kind: StatusKind::Error,
                        };
                        return;
                    }
                };
                r.error = None;
                c.submitting = Some(r.id);
                let (conn, db_type) = (session.conn.clone(), session.config.db_type.clone());
                let run = self
                    .worker
                    .submit(tab.id, conn, db_type, table, columns, changes);
                tab.runs.submit = Some(run);
                tab.summary = Some("Submit…".into());
            }
        }
    }

    /// Run `kind` in console `id` (without asking about pending edits).
    pub fn run_console(&mut self, id: TabId, kind: RunKind) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Console(c) = &mut tab.kind else {
            return;
        };
        let Some(conn) = self.sessions.conn(&tab.connection) else {
            self.status = Status {
                text: format!("{} n'est pas connectée", tab.connection),
                kind: StatusKind::Error,
            };
            return;
        };
        let max = Some(console::MAX_ROWS);
        let run = match kind {
            RunKind::Script(text) => self.worker.run_script(tab.id, conn, text, max),
            RunKind::AtCursor { text, cursor } => {
                self.worker.run_at_cursor(tab.id, conn, text, cursor, max)
            }
        };
        tab.runs.script = Some(run);
        c.refreshing = None;
        c.running_since = Some(std::time::Instant::now());
        tab.summary = Some("Exécution…".into());
    }
}
