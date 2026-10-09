//! Worker events (connections, metadata, query outcomes) and the
//! application-wide keyboard shortcuts.

use egui::KeyboardShortcut;

use crate::engine::db::utils::display_qualified;
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
        self.sessions.connecting.remove(&name);
        self.sessions.errors.remove(&name);
        self.sessions.open.insert(name.clone(), session);
        self.success(format!("Connecté à {name}"));
        if self.config.last_connection.as_deref() != Some(name.as_str()) {
            self.config.last_connection = Some(name.clone());
            self.save_config();
        }

        let restored =
            tabs::take_consoles(&mut self.pending_consoles, &name, !self.unbound_restored);
        self.unbound_restored = true;
        for q in restored {
            self.tabs.add(
                name.clone(),
                q.name,
                TabKind::Console(ConsoleTab::new(q.query, q.cursor_position)),
            );
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
                let Some(config) = self
                    .config
                    .connections
                    .iter()
                    .find(|c| c.name == name)
                    .cloned()
                else {
                    // Deleted or renamed while connecting.
                    self.sessions.connecting.remove(&name);
                    return;
                };
                self.on_connected(Session::new(config, conn, schemas));
            }
            Event::ConnectFailed { name, error } => {
                self.sessions.connecting.remove(&name);
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
            ev @ Event::Script { .. } => self.on_script(ev),
            ev @ Event::Submitted { .. } if self.is_console(&ev) => self.on_console_submitted(ev),
            ev @ (Event::Page { .. }
            | Event::Count { .. }
            | Event::Submitted { .. }
            | Event::Ddl { .. }) => {
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
                    self.data_action(id, action);
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
        match report.status {
            Ok(text) => {
                tab.summary = Some(text.clone());
                // A run in progress keeps going: the result refreshes on the next run.
                if let Some((id, sql, connection)) = report.refresh {
                    if let (None, Some(conn)) = (tab.runs.script, self.sessions.conn(&connection)) {
                        let run =
                            self.worker
                                .run_script(tab.id, conn, sql, Some(console::MAX_ROWS));
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
        let (Event::Script { outcomes, .. }, TabKind::Console(c)) = (ev, &mut tab.kind) else {
            return;
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
        for o in outcomes.iter().filter(|o| !o.sql.trim().is_empty()) {
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
            let active = self.tabs.active_mut().map(|t| match &mut t.kind {
                TabKind::Data(d) => Err((t.id, d.navigate(Nav::Refresh))),
                _ => Ok(t.connection.clone()),
            });
            match active {
                Some(Ok(c)) => self.refresh(&c),
                Some(Err((id, action))) => self.data_action(id, action),
                None => {}
            }
        }
    }
}
