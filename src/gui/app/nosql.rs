//! MongoDB and Redis in the app: the « Documents » and « Clés » tabs, the
//! NoSQL consoles' runs and submits, the administration tasks and their
//! outcomes. Nothing here goes through the SQL paths (`ops::query`,
//! `ops::rows`, `Session::runner`).

use crate::engine::nosql::mongo::{self, CollectionRef};
use crate::engine::nosql::redis::{self, KeyEdit};
use crate::engine::nosql::{self, NoSqlScope};
use crate::engine::ops::rows::RowChanges;
use crate::gui::dialogs::indexes::{IndexAnswer, IndexDialog};
use crate::gui::dialogs::json_editor::JsonEditor;
use crate::gui::dialogs::nosql::{ConfirmTask, NewKey, Prompt, PromptKind};
use crate::gui::dialogs::transfer::{BatchKind, NoSqlBatch};
use crate::gui::dialogs::Dialog;
use crate::gui::sessions::{self, KeySet};
use crate::gui::tabs::console::{self, RunKind};
use crate::gui::tabs::documents::{without_id, DocAction, DocMode, DocumentsTab};
use crate::gui::tabs::keys::{KeyAction, KeysTab, VALUE_PAGE};
use crate::gui::tabs::{TabAction, TabId, TabKind};
use crate::gui::worker::nosql::{NoSqlTask, Refresh};
use crate::gui::worker::Event;

use super::App;

/// The range of a console run: the unit at the cursor (`AtCursor`), or all
/// of `text` (a selection or F5). A text without units (or that the engine
/// can't split) runs whole: the engine reports what is wrong with it.
pub fn run_range(
    db: &crate::engine::models::DatabaseType,
    kind: RunKind,
) -> (String, Option<std::ops::Range<usize>>) {
    match kind {
        RunKind::Script(text) => (text, None),
        RunKind::AtCursor { text, cursor } => {
            let range = nosql::unit_at(db, &text, cursor);
            (text, range)
        }
    }
}

/// The confirmation of a destructive task.
fn confirm(connection: &str, title: &str, text: String, button: &str, task: NoSqlTask) -> Dialog {
    Dialog::ConfirmTask(ConfirmTask {
        connection: connection.to_string(),
        title: title.to_string(),
        text,
        button: button.to_string(),
        task,
    })
}

/// « Supprimer la collection… ».
pub fn confirm_drop_collection(connection: &str, coll: CollectionRef) -> Dialog {
    confirm(
        connection,
        "Supprimer la collection",
        format!("La collection {coll}, ses documents et ses index seront supprimés."),
        "Supprimer",
        NoSqlTask::DropCollection(coll),
    )
}

/// « Vider la collection… » (`deleteMany({})`).
pub fn confirm_empty_collection(connection: &str, coll: CollectionRef) -> Dialog {
    confirm(
        connection,
        "Vider la collection",
        format!("Tous les documents de {coll} seront supprimés (les index restent)."),
        "Vider",
        NoSqlTask::EmptyCollection(coll),
    )
}

/// « Supprimer la base… » (MongoDB).
pub fn confirm_drop_database(connection: &str, db: String) -> Dialog {
    confirm(
        connection,
        "Supprimer la base",
        format!("La base {db} et toutes ses collections seront supprimées."),
        "Supprimer",
        NoSqlTask::DropDatabase(db),
    )
}

/// « Vider la base… » (Redis `FLUSHDB`).
pub fn confirm_flush(connection: &str, db: u8) -> Dialog {
    confirm(
        connection,
        "Vider la base",
        format!("Toutes les clés de db{db} seront supprimées (FLUSHDB)."),
        "Vider",
        NoSqlTask::FlushDb(db),
    )
}

/// « Supprimer » a Redis key.
pub fn confirm_delete_key(connection: &str, db: u8, key: Vec<u8>) -> Dialog {
    confirm(
        connection,
        "Supprimer la clé",
        format!(
            "La clé {} de db{db} sera supprimée.",
            redis::display_bytes(&key)
        ),
        "Supprimer",
        NoSqlTask::EditKey {
            db,
            key,
            edits: vec![KeyEdit::Delete],
        },
    )
}

/// « Renommer… » a Redis key.
pub fn rename_prompt(connection: &str, db: u8, key: Vec<u8>) -> Dialog {
    let text = redis::display_bytes(&key);
    Dialog::Prompt(Prompt::new(
        connection,
        PromptKind::RenameKey { db, key },
        text,
    ))
}

/// « Expiration… » of a Redis key.
pub fn expire_prompt(connection: &str, db: u8, key: Vec<u8>, ttl: Option<i64>) -> Dialog {
    let text = ttl.map(|t| t.to_string()).unwrap_or_default();
    Dialog::Prompt(Prompt::new(
        connection,
        PromptKind::Expire { db, key },
        text,
    ))
}

/// The primary button of a NoSQL form was clicked: do what it asks;
/// whether the form stays open (refused: it shows why).
pub fn submit_form(app: &mut App, dialog: &mut Dialog) -> bool {
    match dialog {
        Dialog::Prompt(p) => match p.task() {
            Ok(task) => {
                app.nosql_task(&p.connection, task);
                false
            }
            Err(e) => {
                p.error = Some(e);
                true
            }
        },
        Dialog::NewKey(k) => match k.task() {
            Ok(task) => {
                app.nosql_task(&k.connection, task);
                false
            }
            Err(e) => {
                k.error = Some(e);
                true
            }
        },
        Dialog::JsonEditor(e) => match e.change() {
            Ok(change) => {
                let (connection, tab, coll) = (e.connection.clone(), e.tab, e.coll.clone());
                !app.write_documents(tab, &connection, coll, vec![change])
            }
            Err(msg) => {
                e.refused = Some(msg);
                true
            }
        },
        _ => false,
    }
}

/// A request of the index dialog; whether it stays open (always).
pub fn index_answer(app: &mut App, d: &mut IndexDialog, answer: IndexAnswer) -> bool {
    match answer {
        IndexAnswer::AskDrop(name) => d.dropping = Some(name),
        IndexAnswer::KeepIndex => d.dropping = None,
        IndexAnswer::Drop(name) => {
            d.dropping = None;
            d.busy = true;
            d.error = None;
            app.nosql_task(
                &d.connection.clone(),
                NoSqlTask::DropIndex {
                    coll: d.coll.clone(),
                    name,
                },
            );
        }
        IndexAnswer::Create => match d.new_index() {
            Ok((keys, options)) => {
                d.busy = true;
                d.error = None;
                app.nosql_task(
                    &d.connection.clone(),
                    NoSqlTask::CreateIndex {
                        coll: d.coll.clone(),
                        keys,
                        options,
                    },
                );
            }
            Err(e) => d.error = Some(e),
        },
    }
    true
}

impl App {
    /// Apply what tab `id` asked for after an outcome or a confirmation.
    pub fn tab_action(&mut self, id: TabId, action: TabAction) {
        match action {
            TabAction::Data(a) => self.data_action(id, a),
            TabAction::Documents(a) => self.documents_action(id, a),
            TabAction::Keys(a) => self.keys_action(id, a),
        }
    }

    /// Activate the « Documents » tab of `coll`, opening it if needed.
    pub fn open_documents(&mut self, connection: &str, coll: CollectionRef, mode: DocMode) {
        match self.tabs.find_documents(connection, &coll) {
            Some(i) => {
                self.tabs.active = i;
                if let Some(TabKind::Documents(d)) = self.tabs.list.get_mut(i).map(|t| &mut t.kind)
                {
                    d.mode = mode;
                }
            }
            None => {
                let title = coll.coll.clone();
                // It asks for its first page itself (`needs_load`).
                self.tabs.add(
                    connection.to_string(),
                    title,
                    TabKind::Documents(Box::new(DocumentsTab::new(coll, mode))),
                );
            }
        }
    }

    /// Activate the « Clés » tab of Redis database `db` (opening it if
    /// needed), showing `key` when given.
    pub fn open_keys(&mut self, connection: &str, db: u8, key: Option<Vec<u8>>) {
        let id = match self.tabs.find_keys(connection, db) {
            Some(i) => {
                self.tabs.active = i;
                self.tabs.list[i].id
            }
            None => {
                let id = self.tabs.add(
                    connection.to_string(),
                    format!("db{db}"),
                    TabKind::Keys(Box::new(KeysTab::new(db, None))),
                );
                id
            }
        };
        let Some(key) = key else {
            return;
        };
        let action = self.tabs.find(id).and_then(|t| match &mut t.kind {
            TabKind::Keys(k) => k.open(key),
            _ => None,
        });
        if let Some(a) = action {
            self.keys_action(id, a);
        }
    }

    /// Run an administration task on `connection`.
    pub fn nosql_task(&mut self, connection: &str, task: NoSqlTask) {
        match self.sessions.conn(connection) {
            Some(conn) => {
                self.info("Opération en cours…");
                self.worker.nosql_task(connection.to_string(), conn, task);
            }
            None => self.not_connected(connection),
        }
    }

    /// Write `changes` to `coll` for « Documents » tab `tab`; whether they
    /// were sent.
    pub fn write_documents(
        &mut self,
        tab: TabId,
        connection: &str,
        coll: CollectionRef,
        changes: Vec<mongo::DocChange>,
    ) -> bool {
        let Some(conn) = self.sessions.conn(connection) else {
            self.not_connected(connection);
            return false;
        };
        let Some(t) = self.tabs.find(tab) else {
            self.error("L'onglet des documents a été fermé");
            return false;
        };
        if t.runs.submit.is_some() {
            self.error("Un envoi est déjà en cours");
            return false;
        }
        t.runs.submit = Some(self.worker.apply_doc_changes(tab, conn, coll, changes));
        t.summary = Some("Envoi…".into());
        true
    }

    /// Apply what « Documents » tab `id` asked for.
    pub fn documents_action(&mut self, id: TabId, action: DocAction) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Documents(d) = &mut tab.kind else {
            return;
        };
        let connection = tab.connection.clone();
        let conn = self.sessions.conn(&connection);
        match action {
            DocAction::Load { count } => {
                let Some(conn) = conn else {
                    d.needs_load = true;
                    return;
                };
                d.needs_load = false;
                d.loading = true;
                tab.runs.page = Some(self.worker.find_page(id, conn.clone(), d.query.clone()));
                if count {
                    d.total = None;
                    d.count_error = None;
                    tab.runs.count = Some(self.worker.doc_count(id, conn, d.query.clone()));
                }
            }
            DocAction::Submit => {
                if tab.runs.submit.is_some() || d.grid.edits.is_empty() {
                    d.after_submit = None;
                    d.close_after_submit = false;
                    return;
                }
                let Some(conn) = conn else {
                    d.after_submit = None;
                    d.close_after_submit = false;
                    self.not_connected(&connection);
                    return;
                };
                let Some(writes) = d.writes() else {
                    return;
                };
                match writes {
                    Ok(writes) => {
                        d.error = None;
                        let coll = d.coll.clone();
                        tab.runs.submit =
                            Some(self.worker.apply_doc_changes(id, conn, coll, writes));
                        tab.summary = Some("Submit…".into());
                    }
                    Err(e) => {
                        d.after_submit = None;
                        d.close_after_submit = false;
                        d.error = Some(format!("Submit impossible : {e}"));
                        self.error(format!("Submit impossible : {e}"));
                    }
                }
            }
            DocAction::Confirm => self.dialog = Some(Dialog::DiscardEdits(id)),
            DocAction::Applied(n) => {
                let close = std::mem::take(&mut d.close_after_submit);
                self.success(format!("{n} modification(s) appliquée(s)"));
                if close {
                    self.close_tab_id(id);
                } else {
                    self.documents_action(id, DocAction::Load { count: true });
                }
            }
            DocAction::SubmitFailed(e) => self.error(format!(
                "Submit interrompu (les écritures précédentes restent) : {e}"
            )),
            DocAction::Aggregate => {
                let Some(conn) = conn else {
                    self.not_connected(&connection);
                    return;
                };
                d.pipeline_error = None;
                let (coll, pipeline) = (d.coll.clone(), d.pipeline.clone());
                let max = crate::gui::tabs::documents::MAX_AGGREGATE_ROWS;
                tab.runs.agg = Some(self.worker.aggregate(id, conn, coll, pipeline, max));
            }
            DocAction::Edit(row) => {
                let coll = d.coll.clone();
                let editor = match row {
                    Some(i) => match d.doc(crate::gui::grid::changes::RowRef::Base(i)) {
                        Some(doc) => JsonEditor::edit(&connection, id, coll, doc),
                        None => return,
                    },
                    None => JsonEditor::insert(&connection, id, coll, &bson::Document::new()),
                };
                self.dialog = Some(Dialog::JsonEditor(Box::new(editor)));
            }
            DocAction::Duplicate(i) => {
                let Some(doc) = d.doc(crate::gui::grid::changes::RowRef::Base(i)) else {
                    return;
                };
                let editor = JsonEditor::insert(&connection, id, d.coll.clone(), &without_id(doc));
                self.dialog = Some(Dialog::JsonEditor(Box::new(editor)));
            }
            DocAction::Export => {
                let (coll, filter) = (d.coll.clone(), d.query.filter.clone());
                let Some(session) = self.sessions.get(&connection) else {
                    self.not_connected(&connection);
                    return;
                };
                let mut b = NoSqlBatch::mongo(
                    &connection,
                    BatchKind::Export,
                    &session.schemas,
                    Some(&coll),
                );
                b.filter = filter;
                self.dialog = Some(Dialog::NoSqlBatch(Box::new(b)));
            }
        }
    }

    /// Apply what « Clés » tab `id` asked for.
    pub fn keys_action(&mut self, id: TabId, action: KeyAction) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Keys(k) = &mut tab.kind else {
            return;
        };
        let connection = tab.connection.clone();
        let conn = self.sessions.conn(&connection);
        let db = k.db;
        // Writes to the open key: one at a time.
        let write = |k: &KeysTab, edits: Vec<KeyEdit>| Some((k.open_key.clone()?, edits));
        let edits = match action {
            KeyAction::Scan { more } => {
                let Some(conn) = conn else {
                    k.needs_scan = true;
                    return;
                };
                k.appending = more;
                let cursor = if more { k.cursor } else { 0 };
                let (pattern, kind) = (k.applied_pattern.clone(), k.kind.clone());
                tab.runs.page = Some(self.worker.scan_keys(id, conn, db, pattern, kind, cursor));
                None
            }
            KeyAction::Read { from } => {
                let (Some(conn), Some(key)) = (conn, k.open_key.clone()) else {
                    return;
                };
                k.reading_from(from);
                tab.runs.value = Some(self.worker.read_key(id, conn, db, key, from, VALUE_PAGE));
                None
            }
            KeyAction::Confirm => {
                self.dialog = Some(Dialog::DiscardEdits(id));
                None
            }
            KeyAction::SubmitValue => {
                k.commit_editor();
                let Some(edits) = k.edits() else {
                    return;
                };
                match edits {
                    Ok(edits) => write(k, edits),
                    Err(e) => {
                        k.value_error = Some(format!("Modification impossible : {e}"));
                        None
                    }
                }
            }
            KeyAction::SaveString => match k.string_value() {
                Ok(bytes) => write(k, vec![KeyEdit::SetString(bytes)]),
                Err(e) => {
                    k.value_error = Some(e);
                    None
                }
            },
            KeyAction::SetTtl(ttl) => write(k, vec![KeyEdit::Expire(ttl)]),
            KeyAction::Rename => {
                if let Some(key) = k.open_key.clone() {
                    self.dialog = Some(rename_prompt(&connection, db, key));
                }
                None
            }
            KeyAction::Delete => {
                if let Some(key) = k.open_key.clone() {
                    self.dialog = Some(confirm_delete_key(&connection, db, key));
                }
                None
            }
            KeyAction::NewKey => {
                self.dialog = Some(Dialog::NewKey(NewKey::new(&connection, db)));
                None
            }
            KeyAction::Applied(n) => {
                let from = k.value_from;
                let close = std::mem::take(&mut k.close_after_submit);
                self.success(format!("{n} modification(s) appliquée(s)"));
                if close {
                    self.close_tab_id(id);
                    return;
                }
                self.keys_action(id, KeyAction::Read { from });
                // The TTL and size of the listed keys may have changed.
                self.reload_explorer_keys(&connection, db);
                None
            }
            KeyAction::SubmitFailed(e) => {
                self.error(format!("Modification interrompue : {e}"));
                None
            }
        };
        let Some((key, edits)) = edits else {
            return;
        };
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        if tab.runs.submit.is_some() {
            return;
        }
        let Some(conn) = self.sessions.conn(&connection) else {
            self.not_connected(&connection);
            return;
        };
        tab.runs.submit = Some(self.worker.apply_key_edits(id, conn, db, key, edits));
        tab.summary = Some("Envoi…".into());
    }

    /// List the keys of Redis database `db` again when the explorer shows
    /// them.
    pub fn reload_explorer_keys(&mut self, connection: &str, db: u8) {
        if let Some(s) = self.sessions.get_mut(connection) {
            if s.redis_keys.contains_key(&db) || s.keys_loading.contains_key(&db) {
                s.load_keys(&mut self.worker, connection, db);
            }
        }
    }

    /// Run console `id` on its MongoDB / Redis connection, after a
    /// confirmation of its destructive commands unless `confirmed`.
    pub fn run_nosql_console(&mut self, id: TabId, kind: RunKind, confirmed: bool) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Console(c) = &mut tab.kind else {
            return;
        };
        let Some(session) = self.sessions.get(&tab.connection) else {
            let connection = tab.connection.clone();
            self.not_connected(&connection);
            return;
        };
        let db = session.config.db_type.clone();
        let (text, range) = run_range(&db, kind.clone());
        if !confirmed {
            let warnings = nosql::warnings(&db, &text, range.clone());
            if !warnings.is_empty() {
                self.dialog = Some(Dialog::ConfirmNoSqlRun { id, kind, warnings });
                return;
            }
        }
        let scope = c
            .scope
            .clone()
            .unwrap_or_else(|| NoSqlScope::initial(&session.config));
        let conn = session.conn.clone();
        let run = self
            .worker
            .nosql_script(tab.id, conn, text, range, scope, console::MAX_ROWS);
        tab.runs.script = Some(run);
        c.refreshing = None;
        c.running_since = Some(std::time::Instant::now());
        tab.summary = Some("Exécution…".into());
    }

    /// « Exécuter quand même »: run without asking again.
    pub fn run_console_confirmed(&mut self, id: TabId, kind: RunKind) {
        self.run_nosql_console(id, kind, true);
    }

    /// Submit the pending edits of NoSQL result tab `result_index` of
    /// console `index` (MongoDB documents).
    pub(super) fn submit_nosql_result(
        &mut self,
        index: usize,
        result_index: usize,
        changes: RowChanges,
    ) {
        let Some(tab) = self.tabs.list.get_mut(index) else {
            return;
        };
        let TabKind::Console(c) = &mut tab.kind else {
            return;
        };
        if tab.runs.submit.is_some() {
            return;
        }
        let Some(r) = c.results.get_mut(result_index) else {
            return;
        };
        // Documents of a collection, or the value of a Redis key.
        let writes = match (r.doc_writes(&changes), r.key_writes(&changes)) {
            (Some((coll, writes)), _) => writes.map(|w| ConsoleWrites::Docs(coll, w)),
            (None, Some((db, key, edits))) => edits.map(|e| ConsoleWrites::Key(db, key, e)),
            (None, None) => return,
        };
        let writes = match writes {
            Ok(w) => w,
            Err(e) => {
                r.error = Some(e.clone());
                self.error(format!("Submit impossible : {e}"));
                return;
            }
        };
        let open = |name: &str| self.sessions.open.contains_key(name);
        let conn = match console::result_connection(r, open) {
            Ok(name) => self.sessions.open[name].conn.clone(),
            Err(e) => {
                self.error(e);
                return;
            }
        };
        r.error = None;
        c.submitting = Some(r.id);
        tab.runs.submit = Some(match writes {
            ConsoleWrites::Docs(coll, w) => self.worker.apply_doc_changes(tab.id, conn, coll, w),
            ConsoleWrites::Key(db, key, e) => self.worker.apply_key_edits(tab.id, conn, db, key, e),
        });
        tab.summary = Some("Submit…".into());
    }

    /// Outcomes of the NoSQL work not addressed to a tab.
    pub(super) fn on_nosql_event(&mut self, ev: Event) {
        match ev {
            Event::Keys {
                name,
                db,
                run,
                outcome,
            } => {
                if let Some(s) = self.sessions.get_mut(&name) {
                    let outcome = outcome.map(|page| KeySet {
                        more: page.cursor != 0,
                        keys: page.keys,
                    });
                    if sessions::accept_keys(
                        &mut s.redis_keys,
                        &mut s.keys_loading,
                        db,
                        run,
                        outcome,
                    ) {
                        s.refresh_key_names();
                    }
                }
            }
            Event::Indexes {
                name,
                coll,
                run,
                outcome,
            } => {
                if let Some(Dialog::Indexes(d)) = &mut self.dialog {
                    if d.connection == name && d.coll == coll && d.run == Some(run) {
                        d.run = None;
                        d.indexes = Some(outcome);
                    }
                }
            }
            Event::NoSqlDone {
                name,
                refresh,
                outcome,
            } => self.on_task_done(name, refresh, outcome),
            _ => {}
        }
    }

    /// Open « Index… » of `coll` and list its indexes.
    pub fn open_indexes(&mut self, connection: &str, coll: CollectionRef) {
        let mut d = IndexDialog::new(connection, coll);
        self.load_indexes(&mut d);
        self.dialog = Some(Dialog::Indexes(Box::new(d)));
    }

    fn load_indexes(&mut self, d: &mut IndexDialog) {
        match self.sessions.conn(&d.connection) {
            Some(conn) => {
                d.indexes = None;
                d.run = Some(
                    self.worker
                        .indexes(d.connection.clone(), conn, d.coll.clone()),
                );
            }
            None => d.indexes = Some(Err(crate::gui::status::not_connected(&d.connection))),
        }
    }

    fn on_task_done(&mut self, name: String, refresh: Refresh, outcome: Result<String, String>) {
        let ok = outcome.is_ok();
        match outcome {
            Ok(text) => self.success(text),
            Err(e) => self.error(format!("{name} : {e}")),
        }
        match refresh {
            Refresh::Schemas => {
                if ok {
                    self.refresh(&name);
                }
            }
            Refresh::Collection(coll) => {
                if let Some(s) = self.sessions.get_mut(&name) {
                    s.details.remove(&coll.key());
                }
                self.reload_documents(&name, &coll);
            }
            Refresh::Indexes(coll) => {
                if let Some(s) = self.sessions.get_mut(&name) {
                    s.details.remove(&coll.key());
                }
                if let Some(Dialog::Indexes(mut d)) = self.dialog.take() {
                    if d.connection == name && d.coll == coll {
                        d.busy = false;
                        if ok {
                            d.created();
                            self.load_indexes(&mut d);
                        }
                    }
                    self.dialog = Some(Dialog::Indexes(d));
                }
            }
            Refresh::RedisDb(db) => {
                self.reload_explorer_keys(&name, db);
                let ids: Vec<TabId> = self
                    .tabs
                    .list
                    .iter()
                    .filter(|t| t.connection == name)
                    .filter(|t| matches!(&t.kind, TabKind::Keys(k) if k.db == db))
                    .map(|t| t.id)
                    .collect();
                for id in ids {
                    self.keys_action(id, KeyAction::Scan { more: false });
                    let from = match self.tabs.find(id).map(|t| &t.kind) {
                        Some(TabKind::Keys(k)) if k.open_key.is_some() => Some(k.value_from),
                        _ => None,
                    };
                    if let Some(from) = from {
                        self.keys_action(id, KeyAction::Read { from });
                    }
                }
            }
        }
    }

    /// Reload the « Documents » tabs of `coll` without pending edits.
    pub fn reload_documents(&mut self, connection: &str, coll: &CollectionRef) {
        let ids: Vec<TabId> = self
            .tabs
            .list
            .iter()
            .filter(|t| t.connection == connection)
            .filter(|t| {
                matches!(&t.kind, TabKind::Documents(d) if &d.coll == coll && d.grid.edits.is_empty())
            })
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.documents_action(id, DocAction::Load { count: true });
        }
    }
}

/// The writes of a NoSQL console result's Submit.
enum ConsoleWrites {
    Docs(
        crate::engine::nosql::mongo::CollectionRef,
        Vec<crate::engine::nosql::mongo::DocChange>,
    ),
    Key(u8, Vec<u8>, Vec<crate::engine::nosql::redis::KeyEdit>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::DatabaseType;

    #[test]
    fn opening_a_table_s_data_never_uses_the_sql_tab_for_nosql() {
        use super::super::{data_route, DataRoute};
        assert_eq!(
            data_route(Some(&DatabaseType::MongoDB), "\"shop\".\"fs.chunks\""),
            DataRoute::Documents(CollectionRef {
                db: "shop".into(),
                coll: "fs.chunks".into()
            })
        );
        assert_eq!(
            data_route(Some(&DatabaseType::MongoDB), "users"),
            DataRoute::Unknown
        );
        assert_eq!(
            data_route(Some(&DatabaseType::Redis), "x"),
            DataRoute::NoTables
        );
        assert_eq!(
            data_route(Some(&DatabaseType::Postgres), "\"public\".\"t\""),
            DataRoute::Sql
        );
        assert_eq!(data_route(None, "t"), DataRoute::Sql);
    }

    #[test]
    fn a_selection_or_f5_runs_whole() {
        let (text, range) = run_range(
            &DatabaseType::MongoDB,
            RunKind::Script("db.a.find()".into()),
        );
        assert_eq!((text.as_str(), range), ("db.a.find()", None));
        // At the cursor: the engine's unit, whatever it finds.
        let (text, range) = run_range(
            &DatabaseType::Redis,
            RunKind::AtCursor {
                text: "GET a\nGET b".into(),
                cursor: 8,
            },
        );
        assert_eq!(text, "GET a\nGET b");
        assert_eq!(range, nosql::unit_at(&DatabaseType::Redis, &text, 8));
    }

    #[test]
    fn destructive_tasks_are_confirmed_with_their_target() {
        let coll = CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        };
        let Dialog::ConfirmTask(c) = confirm_drop_collection("m", coll.clone()) else {
            panic!("a confirmation")
        };
        assert!(c.text.contains("shop.users"));
        assert_eq!(c.task, NoSqlTask::DropCollection(coll.clone()));
        let Dialog::ConfirmTask(c) = confirm_empty_collection("m", coll.clone()) else {
            panic!("a confirmation")
        };
        assert_eq!(c.task, NoSqlTask::EmptyCollection(coll));
        let Dialog::ConfirmTask(c) = confirm_flush("r", 4) else {
            panic!("a confirmation")
        };
        assert!(c.text.contains("FLUSHDB"));
        let Dialog::ConfirmTask(c) = confirm_delete_key("r", 0, b"user:1".to_vec()) else {
            panic!("a confirmation")
        };
        assert!(c.text.contains("user:1"));
        let Dialog::Prompt(p) = expire_prompt("r", 0, b"k".to_vec(), Some(30)) else {
            panic!("a prompt")
        };
        assert_eq!(p.text, "30");
    }
}
