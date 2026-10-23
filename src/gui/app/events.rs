//! Worker events (connections, metadata, query outcomes) and the
//! application-wide keyboard shortcuts.

use egui::KeyboardShortcut;

use crate::engine::db::utils::display_qualified;
use crate::engine::ops::query::StatementOutcome;
use crate::engine::services::history::HistoryEntry;
use crate::gui::dialogs;
use crate::gui::history_popup;
use crate::gui::sessions::{self, Session};
use crate::gui::tabs::console::{self, ConsoleTab};
use crate::gui::tabs::data::Nav;
use crate::gui::tabs::{self, TabKind};
use crate::gui::worker::Event;

use super::{App, CLOSE_TAB, NEW_CONSOLE, REFRESH, SAVE_CONSOLES, TABLE_SEARCH};

impl App {
    fn on_connected(&mut self, session: Session) {
        let name = session.config.name.clone();
        self.sessions.errors.remove(&name);
        if session.config.db_type == crate::engine::models::DatabaseType::Redis {
            self.worker.key_counts(name.clone(), session.conn.clone());
        }
        self.sessions.open.insert(name.clone(), session);
        match self.sign_ins.take_warning() {
            Some(warning) => self.info(format!("Connecté à {name}. {warning}")),
            None => self.success(format!("Connecté à {name}")),
        }
        if self.config.last_connection.as_deref() != Some(name.as_str()) {
            self.config.last_connection = Some(name.clone());
            self.save_config();
        }

        let restored =
            tabs::take_consoles(&mut self.pending_consoles, &name, !self.unbound_restored);
        self.unbound_restored = true;
        for q in restored {
            let mut console = ConsoleTab::new(q.query, q.cursor_position);
            console.scope = self.nosql_scope_of(&name, q.nosql_scope);
            self.tabs
                .add(name.clone(), q.name, TabKind::Console(console));
        }
        if !self.tabs.list.iter().any(|t| t.connection == name) {
            self.new_console(&name, String::new());
        }
    }

    pub(super) fn handle_event(&mut self, ev: Event) {
        match ev {
            Event::Connected {
                name,
                conn,
                schemas,
            } => {
                if !self.sessions.connect_finished(&name) {
                    return; // Cancelled: the connection is dropped.
                }
                let Some(config) = self
                    .config
                    .connections
                    .iter()
                    .find(|c| c.name == name)
                    .cloned()
                else {
                    // Deleted or renamed while connecting.
                    return;
                };
                self.on_connected(Session::new(config, conn, schemas));
            }
            Event::ConnectFailed { name, error } => {
                if !self.sessions.connect_finished(&name) {
                    return; // Cancelled.
                }
                // The failure matters more than a keyring warning (logged).
                self.sign_ins.take_warning();
                self.error(format!("Connexion à {name} impossible : {error}"));
                self.sessions.errors.insert(name, error);
            }
            Event::Schemas { name, outcome } => match outcome {
                Ok(schemas) => {
                    if let Some(s) = self.sessions.get_mut(&name) {
                        s.schemas = schemas;
                    }
                }
                Err(e) => self.error(format!("{name} : lecture des tables impossible : {e}")),
            },
            Event::Details {
                name,
                table,
                run,
                outcome,
            } => {
                if let Some(s) = self.sessions.get_mut(&name) {
                    // Answers predating a refresh or a newer request are stale.
                    sessions::accept_details(&mut s.details, &mut s.loading, table, run, outcome);
                }
            }
            Event::LinkedRow { name, run, outcome } => {
                if let Some(p) = self.fk_preview.as_mut().filter(|p| p.connection == name) {
                    p.on_loaded(run, outcome);
                }
            }
            ev @ (Event::Script { .. } | Event::NoSqlScript { .. }) => self.on_script(ev),
            ev @ (Event::Keys { .. }
            | Event::KeyCounts { .. }
            | Event::Indexes { .. }
            | Event::NoSqlDone { .. }) => self.on_nosql_event(ev),
            ev @ Event::Submitted { .. } if self.is_console(&ev) => self.on_console_submitted(ev),
            ev @ (Event::Page { .. }
            | Event::Count { .. }
            | Event::Submitted { .. }
            | Event::Ddl { .. }
            | Event::DocPage { .. }
            | Event::Aggregated { .. }
            | Event::KeyPage { .. }
            | Event::KeyValue { .. }) => {
                // Tabs closed meanwhile drop their outcome; a submit still
                // reports whether it was applied.
                let Some(id) = ev.tab() else {
                    return;
                };
                if self.tabs.find(id).is_none() {
                    if let Event::Submitted { outcome, .. } = ev {
                        match outcome {
                            Ok(n) => self.success(format!("{n} modification(s) appliquée(s)")),
                            Err(e) => {
                                self.error(format!("Submit annulé (transaction annulée) : {e}"))
                            }
                        }
                    }
                    return;
                }
                if let Some(action) = self.tabs.find(id).and_then(|tab| tab.on_event(ev)) {
                    self.tab_action(id, action);
                }
            }
            Event::Progress {
                op,
                done,
                total,
                label,
            } => self.progress.update(op, done, total, label),
            Event::SchemaApplied {
                name,
                table,
                outcome,
            } => dialogs::structure::on_applied(self, name, table, outcome),
            Event::Exported(outcome) => match outcome {
                Ok((n, path)) => {
                    self.success(format!("{n} ligne(s) exportée(s) vers {}", path.display()))
                }
                Err(e) => self.error(format!("Export impossible : {e}")),
            },
            Event::Imported {
                op,
                name,
                table,
                outcome,
            } => {
                self.progress.finish(op);
                if outcome.as_ref().is_ok_and(|s| s.succeeded() > 0) {
                    self.reload_data_tabs(&name, Some(&table));
                }
                let table = display_qualified(&table);
                match outcome {
                    Ok(stats) if stats.errors.is_empty() => self.success(format!(
                        "{table} : {} insérée(s), {} mise(s) à jour",
                        stats.inserted, stats.updated
                    )),
                    Ok(stats) => self.error(format!(
                        "{table} : {} erreur(s), première : {}",
                        stats.errors.len(),
                        stats.errors[0]
                    )),
                    Err(e) => self.error(format!("{table} : {e}")),
                }
            }
            Event::Batch {
                op,
                name,
                kind,
                report,
            } => {
                self.progress.finish(op);
                if kind != "Export" && report.succeeded > 0 {
                    self.reload_data_tabs(&name, None);
                }
                if report.errors.is_empty() {
                    self.success(format!(
                        "{kind} ({name}) : {}/{} table(s), {} ligne(s)",
                        report.succeeded, report.total, report.rows_affected
                    ));
                } else {
                    self.error(format!(
                        "{kind} ({name}) : {} erreur(s), première : {}",
                        report.errors.len(),
                        report.errors[0]
                    ));
                }
            }
            ev @ Event::TestFinished(_) => dialogs::handle_event(self, ev),
            Event::TxOpened { name, outcome } => self.on_tx_opened(name, outcome),
            Event::TxEnded {
                name,
                commit,
                outcome,
            } => self.on_tx_ended(name, commit, outcome),
        }
    }

    /// Whether the outcome `ev` belongs to an open console tab.
    fn is_console(&mut self, ev: &Event) -> bool {
        ev.tab()
            .and_then(|id| self.tabs.find(id))
            .is_some_and(|t| matches!(t.kind, TabKind::Console(_)))
    }

    /// Submit of a console result: on success clear its edits and re-run its
    /// SQL to refresh it; on failure keep the edits and show the error.
    fn on_console_submitted(&mut self, ev: Event) {
        let Some(tab) = ev.tab().and_then(|id| self.tabs.find(id)) else {
            return;
        };
        if !tab.runs.accept(&ev) {
            return;
        }
        let (Event::Submitted { outcome, .. }, TabKind::Console(c)) = (ev, &mut tab.kind) else {
            return;
        };
        // Reported even when the result tab was closed or replaced meanwhile.
        let report = console::apply_submitted(c, outcome);
        let note = match &report.refresh {
            Some((_, _, connection)) => self.sessions.submit_note(connection),
            None => "",
        };
        match report.status {
            Ok(text) => {
                let text = format!("{text}{note}");
                tab.summary = Some(text.clone());
                // A run in progress keeps going: the result refreshes on the next run.
                if let Some((id, sql, connection)) = report.refresh {
                    // MongoDB: re-run in the database the rows came from.
                    let nosql = self.sessions.get(&connection).filter(|s| s.is_nosql());
                    if let (None, Some(session)) = (tab.runs.script, nosql) {
                        let database = c.results.iter().find(|r| r.id == id).and_then(|r| match &r
                            .source
                        {
                            Some(crate::engine::nosql::Source::Collection { coll, .. }) => {
                                Some(coll.db.clone())
                            }
                            // Redis: the database the key was read in.
                            Some(crate::engine::nosql::Source::Key { db, .. }) => {
                                Some(db.to_string())
                            }
                            None => None,
                        });
                        let scope = match database {
                            Some(database) => crate::engine::nosql::NoSqlScope { database },
                            None => c.scope.clone().unwrap_or_else(|| {
                                crate::engine::nosql::NoSqlScope::initial(&session.config)
                            }),
                        };
                        let run = self.worker.nosql_script(
                            tab.id,
                            session.conn.clone(),
                            sql,
                            None,
                            scope,
                            console::MAX_ROWS,
                        );
                        tab.runs.script = Some(run);
                        c.refreshing = Some(id);
                        c.running_since = Some(std::time::Instant::now());
                    } else if let (None, Some(runner)) =
                        (tab.runs.script, self.sessions.runner(&connection))
                    {
                        let run =
                            self.worker
                                .run_script(tab.id, runner, sql, Some(console::MAX_ROWS));
                        tab.runs.script = Some(run);
                        c.refreshing = Some(id);
                        c.running_since = Some(std::time::Instant::now());
                    }
                }
                self.success(text);
            }
            Err(e) => self.error(e),
        }
    }

    /// Outcomes of a console run: result tabs, log, history, summary.
    fn on_script(&mut self, ev: Event) {
        let Some(tab) = ev.tab().and_then(|id| self.tabs.find(id)) else {
            return;
        };
        if !tab.runs.accept(&ev) {
            return;
        }
        let TabKind::Console(c) = &mut tab.kind else {
            return;
        };
        let outcomes = match ev {
            Event::Script { outcomes, .. } => outcomes,
            Event::NoSqlScript {
                outcomes, scope, ..
            } => {
                // `use` / `SELECT` moved the console: the selector follows.
                c.apply_scope(scope);
                if let Some(id) = c.refreshing.take() {
                    tab.summary = Some(console::apply_nosql_refresh(
                        c,
                        id,
                        outcomes,
                        chrono::Local::now(),
                    ));
                    return;
                }
                let at = history_popup::now_secs();
                for o in outcomes.iter().filter(|o| !o.outcome.sql.trim().is_empty()) {
                    self.history.push(HistoryEntry {
                        sql: o.outcome.sql.clone(),
                        connection: tab.connection.clone(),
                        at,
                        duration_ms: o.outcome.elapsed_ms as u64,
                        ok: o.outcome.result.is_ok(),
                    });
                }
                self.history_saver.save(self.history.clone());
                tab.summary = Some(console::apply_nosql_outcomes(
                    c,
                    &tab.connection,
                    outcomes,
                    chrono::Local::now(),
                ));
                return;
            }
            _ => return,
        };
        if let Some(id) = c.refreshing.take() {
            tab.summary = Some(console::apply_refresh(
                c,
                id,
                outcomes,
                chrono::Local::now(),
            ));
            return;
        }
        let at = history_popup::now_secs();
        let tsql = self
            .sessions
            .get(&tab.connection)
            .is_some_and(|s| s.config.db_type.runs_tsql_batches());
        for o in history_outcomes(&outcomes, tsql) {
            self.history.push(HistoryEntry {
                sql: o.sql.clone(),
                connection: tab.connection.clone(),
                at,
                duration_ms: o.elapsed_ms as u64,
                ok: o.result.is_ok(),
            });
        }
        self.history_saver.save(self.history.clone());
        tab.summary = Some(console::apply_outcomes(
            c,
            &tab.connection,
            outcomes,
            chrono::Local::now(),
        ));
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        let pressed = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
        if pressed(&NEW_CONSOLE) {
            match self.current_connection() {
                Some(c) => {
                    self.new_console(&c, String::new());
                }
                None => self.error("Ouvrez d'abord une connexion"),
            }
        }
        if pressed(&CLOSE_TAB) {
            self.request_close(self.tabs.active);
        }
        if pressed(&SAVE_CONSOLES) {
            match self.save_consoles() {
                Ok(()) => self.info("Consoles enregistrées"),
                Err(e) => self.error(format!("Enregistrement des consoles impossible : {e}")),
            }
        }
        if pressed(&TABLE_SEARCH) {
            self.table_search();
        }
        if pressed(&REFRESH) {
            // A data tab reloads its page, other tabs their connection.
            use crate::gui::tabs::keys::KeyAction;
            use crate::gui::tabs::TabAction;
            let active = self.tabs.active_mut().map(|t| match &mut t.kind {
                TabKind::Data(d) => Err((t.id, Some(TabAction::Data(d.navigate(Nav::Refresh))))),
                TabKind::Documents(d) => {
                    Err((t.id, d.navigate(Nav::Refresh).map(TabAction::Documents)))
                }
                TabKind::Keys(_) => {
                    Err((t.id, Some(TabAction::Keys(KeyAction::Scan { more: false }))))
                }
                _ => Ok(t.connection.clone()),
            });
            match active {
                Some(Ok(c)) => self.refresh(&c),
                Some(Err((id, Some(action)))) => self.tab_action(id, action),
                Some(Err((_, None))) | None => {}
            }
        }
    }
}

/// The outcomes of a console run that go to the history: those with SQL.
/// On SQL Server (`tsql`), the result sets of one batch share its SQL when
/// they could not be matched with their SELECTs: it is logged once.
fn history_outcomes(
    outcomes: &[StatementOutcome],
    tsql: bool,
) -> impl Iterator<Item = &StatementOutcome> {
    let mut previous: Option<&str> = None;
    outcomes
        .iter()
        .filter(|o| !o.sql.trim().is_empty())
        .filter(move |o| !(tsql && previous.replace(&o.sql) == Some(o.sql.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(sql: &str) -> StatementOutcome {
        StatementOutcome {
            sql: sql.into(),
            result: Err(String::new()),
            elapsed_ms: 0,
        }
    }

    #[test]
    fn history_logs_a_batch_once_only_on_sql_server() {
        let outcomes = [
            outcome("SELECT 1"),
            outcome("SELECT 1"),
            outcome(" "),
            outcome("SELECT 2"),
        ];
        let sqls = |tsql| {
            history_outcomes(&outcomes, tsql)
                .map(|o| o.sql.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(sqls(false), ["SELECT 1", "SELECT 1", "SELECT 2"]);
        assert_eq!(sqls(true), ["SELECT 1", "SELECT 2"]);
    }
}
