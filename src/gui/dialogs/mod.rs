//! Modal dialogs (connection form, confirmations, …).

pub mod connection;
pub mod folder;
pub mod indexes;
pub mod json_editor;
pub mod nosql;
pub mod structure;
pub mod transfer;
pub mod update;

use super::app::App;
use super::history_popup::HistoryPopup;
use super::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use super::table_search::TableSearch;
use super::tabs::console::RunKind;
use super::tabs::{CloseRisk, TabId, TabKind};
use super::worker::Event;
use crate::engine::db::entra_interactive;
use crate::engine::nosql::Warning;
use crate::engine::ops::transfer::qualified;
use connection::{ConnectionForm, FormAction};
use folder::FolderPrompt;
use indexes::{IndexAnswer, IndexDialog};
use json_editor::JsonEditor;
use structure::{StructureAction, StructureDialog};
use transfer::{BatchDialog, BatchStart, NoSqlBatch, NoSqlStart};
use update::UpdateAction;

pub enum Dialog {
    Connection(Box<ConnectionForm>),
    /// Delete the connection with this name.
    ConfirmDeleteConnection(String),
    /// Query history (Ctrl+Alt+E) of the active console.
    History(HistoryPopup),
    /// Table search (Ctrl+N) over every open connection.
    TableSearch(TableSearch),
    /// "Abandonner les modifications ?" before data tab `TabId` changes page.
    DiscardEdits(TabId),
    /// Close tab `id`, which would lose `risk`.
    CloseTab {
        id: TabId,
        risk: CloseRisk,
    },
    /// Run console `id` again although its unpinned results (replaced by
    /// the run) have pending edits.
    ConfirmRerun {
        id: TabId,
        kind: RunKind,
    },
    /// Quit although `edits` tabs have pending edits and these
    /// `(connection, statements)` manual transactions are not committed.
    ConfirmQuit {
        edits: usize,
        transactions: Vec<(String, usize)>,
    },
    /// Leave the manual transaction mode of this connection, whose
    /// transaction holds statements: Commit or Rollback first.
    EndTx(String),
    /// Columns of one table of one connection.
    Structure(StructureDialog),
    /// Empty these (qualified) tables of `connection`.
    ConfirmTruncate {
        connection: String,
        tables: Vec<String>,
    },
    /// Batch export / import / truncate on one connection.
    Batch(BatchDialog),
    /// Name a new or renamed folder of the explorer.
    Folder(FolderPrompt),
    /// « Mise à jour »: follows the updater's state.
    Update,
    /// Run console `id` although `nosql::warnings` found destructive
    /// commands in what it runs.
    ConfirmNoSqlRun {
        id: TabId,
        kind: RunKind,
        warnings: Vec<Warning>,
    },
    /// A destructive MongoDB / Redis task (drop, flush, delete…).
    ConfirmTask(nosql::ConfirmTask),
    /// New collection, key rename, key expiration.
    Prompt(nosql::Prompt),
    /// « Nouvelle clé… » of a Redis database.
    NewKey(nosql::NewKey),
    /// A whole MongoDB document in shell syntax.
    JsonEditor(Box<JsonEditor>),
    /// « Index… » of a MongoDB collection.
    Indexes(Box<IndexDialog>),
    /// MongoDB / Redis export or import (JSON, JSON Lines, CSV).
    NoSqlBatch(Box<NoSqlBatch>),
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut dialog) = app.dialog.take() else {
        return;
    };
    let mut keep = true;
    // Each dialog sizes itself (`modal::ModalSize`).
    let modal =
        egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| dialog_ui(app, ui, &mut dialog));
    // Escape or a click outside cancels the dialog.
    let outside = modal.should_close();
    if let Some(answer) = settle(modal.inner, outside) {
        let effect = effect(&dialog, answer);
        keep = apply(app, ctx, &mut dialog, effect);
    }
    if let (false, Dialog::Connection(form)) = (keep, &mut dialog) {
        // Nobody waits for the test of a closed form any more: stop it (and
        // the browser sign-in it may wait for).
        if form.testing {
            app.worker.cancel_test();
        }
        // A sign-in only a test used (connection not saved, or saved with
        // another account) is forgotten, unless saved connections use its
        // keys too.
        for config in form.orphaned_tests(&app.config.connections) {
            entra_interactive::forget_unless_used(&config, &app.config.connections);
        }
    }
    // A dialog may open another one in its place (batch → confirmation).
    if keep && app.dialog.is_none() {
        app.dialog = Some(dialog);
    }
}

/// Buttons of the standard confirmations.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    /// The (destructive or not) confirmed action.
    Confirm,
    /// "Submit" the pending edits first.
    Submit,
    /// Drop the pending edits.
    Drop,
    Commit,
    Rollback,
    Connection(FormAction),
    Structure(StructureAction),
    /// Insert this history entry into the active console.
    Insert(usize),
    /// Open the data of this table of the table search.
    Open(usize),
    /// Start the batch.
    StartBatch,
    /// Create / rename the folder of the prompt.
    Folder,
    /// A button of the update modal.
    Update(UpdateAction),
    /// The primary button of a NoSQL form (prompt, new key, JSON editor,
    /// export / import).
    FormOk,
    /// What the index dialog asked for.
    Index(IndexAnswer),
}

/// The answer a standard dialog got this frame: `Some(None)` to cancel it
/// (Annuler, ×, Escape, or a click outside: `outside`), `Some(Some(_))`
/// for a button, `None` to keep it open. A button clicked in the frame
/// wins over a click outside.
fn settle(response: ModalResponse<Answer>, outside: bool) -> Option<Option<Answer>> {
    match response {
        ModalResponse::Button(a) => Some(Some(a)),
        ModalResponse::Cancel => Some(None),
        ModalResponse::None => outside.then_some(None),
    }
}

/// Draw `dialog`, a standard modal.
fn dialog_ui(app: &mut App, ui: &mut egui::Ui, dialog: &mut Dialog) -> ModalResponse<Answer> {
    let small = |title| ModalSpec::new(title, ModalSize::Small);
    match dialog {
        Dialog::ConfirmDeleteConnection(name) => {
            let tabs = app
                .tabs
                .list
                .iter()
                .filter(|t| &t.connection == name)
                .count();
            let pending = app.tabs.with_pending_edits(Some(name));
            let warning = (pending > 0).then(|| {
                format!(
                    "{pending} onglet(s) ont des modifications non envoyées : \
                     elles seront perdues."
                )
            });
            modal::show(
                ui,
                small("Supprimer la connexion"),
                |ui| {
                    ui.label(format!(
                        "Supprimer « {name} » ? Cette action est définitive."
                    ));
                    if tabs > 0 {
                        ui.label(format!(
                            "Ses {tabs} onglet(s) seront fermés et ses consoles oubliées."
                        ));
                    }
                },
                warning.as_deref().map(|w| (MessageKind::Error, w)),
                &[ModalButton::danger("Supprimer", Answer::Confirm)],
            )
        }
        Dialog::DiscardEdits(_) => discard_ui(
            ui,
            "Les modifications en attente seront perdues.",
            "Appliquer, puis continuer si tout a réussi",
        ),
        Dialog::CloseTab {
            risk: CloseRisk::Console { text, edits },
            ..
        } => {
            let mut lines = vec![];
            if *text {
                lines.push("Son contenu sera perdu.");
            }
            if *edits {
                lines.push(
                    "Des résultats ont des modifications non envoyées : elles seront perdues.",
                );
            }
            modal::show(
                ui,
                small("Fermer la console ?"),
                |ui| {
                    for line in lines {
                        ui.label(line);
                    }
                },
                None,
                &[ModalButton::danger("Fermer", Answer::Drop)],
            )
        }
        Dialog::CloseTab { .. } => discard_ui(
            ui,
            "Les modifications en attente de cet onglet seront perdues.",
            "Appliquer, puis fermer si tout a réussi",
        ),
        Dialog::ConfirmRerun { .. } => modal::confirm(
            ui,
            "Abandonner les modifications ?",
            "L'exécution remplace les résultats non épinglés : \
             leurs modifications en attente seront perdues.",
            &[],
            ModalButton::danger("Abandonner", Answer::Confirm),
        ),
        Dialog::ConfirmQuit {
            edits,
            transactions,
        } => {
            let mut items = vec![];
            if *edits > 0 {
                items.push(format!(
                    "{edits} onglet(s) ont des modifications non envoyées."
                ));
            }
            items.extend(transactions.iter().map(|(name, n)| {
                format!(
                    "{name} : transaction non validée ({n} instruction(s)), \
                     elle sera annulée."
                )
            }));
            modal::confirm(
                ui,
                "Quitter ?",
                "En quittant maintenant :",
                &items,
                ModalButton::danger("Quitter quand même", Answer::Confirm),
            )
        }
        Dialog::EndTx(name) => {
            let n = app.sessions.get(name).map_or(0, |s| s.pending_statements());
            modal::show(
                ui,
                small("Terminer la transaction ?"),
                |ui| {
                    ui.label(format!(
                        "La transaction de {name} contient {n} instruction(s) non validée(s)."
                    ));
                },
                None,
                &[
                    ModalButton::danger("Rollback", Answer::Rollback),
                    ModalButton::primary("Commit", Answer::Commit),
                ],
            )
        }
        Dialog::ConfirmTruncate { connection, tables } => {
            transfer::confirm_truncate_ui(ui, connection, tables).map(|()| Answer::Confirm)
        }
        Dialog::History(popup) => popup.ui(ui, &app.history).map(Answer::Insert),
        Dialog::TableSearch(search) => {
            let color_of = |name: &str| app.color_of(name);
            search.ui(ui, &color_of).map(Answer::Open)
        }
        Dialog::Batch(b) => transfer::batch_ui(ui, b).map(|()| Answer::StartBatch),
        Dialog::Connection(form) => {
            let sign_in = form.testing_name().and_then(|n| app.sign_ins.url_of(n));
            form.ui(ui, sign_in).map(Answer::Connection)
        }
        Dialog::Structure(d) => structure::ui(app, ui, d).map(Answer::Structure),
        Dialog::Folder(prompt) => prompt.ui(ui).map(|()| Answer::Folder),
        Dialog::Update => update::ui(ui, &app.updater.state, crate::updater::current_version())
            .map(Answer::Update),
        Dialog::ConfirmNoSqlRun { warnings, .. } => {
            nosql::warnings_ui(ui, warnings).map(|()| Answer::Confirm)
        }
        Dialog::ConfirmTask(c) => c.ui(ui).map(|()| Answer::Confirm),
        Dialog::Prompt(p) => p.ui(ui).map(|()| Answer::FormOk),
        Dialog::NewKey(k) => k.ui(ui).map(|()| Answer::FormOk),
        Dialog::JsonEditor(e) => e.ui(ui).map(|()| Answer::FormOk),
        Dialog::Indexes(d) => d.ui(ui).map(Answer::Index),
        Dialog::NoSqlBatch(b) => b.ui(ui).map(|()| Answer::FormOk),
    }
}

/// "Abandonner les modifications ?": Submit them first, drop them, or
/// cancel.
fn discard_ui(ui: &mut egui::Ui, text: &str, submit_hint: &str) -> ModalResponse<Answer> {
    modal::show(
        ui,
        ModalSpec::new("Abandonner les modifications ?", ModalSize::Small),
        |ui| {
            ui.label(text);
        },
        None,
        &[
            ModalButton::danger("Abandonner", Answer::Drop),
            ModalButton::primary("Submit", Answer::Submit).hint(submit_hint),
        ],
    )
}

/// What answering a standard dialog does.
#[derive(Debug, PartialEq)]
enum Effect {
    Nothing,
    DeleteConnection(String),
    AnswerDiscard(TabId, Discard),
    SubmitAndClose(TabId),
    CloseTab(TabId),
    Rerun(TabId, RunKind),
    Quit,
    /// The quit (maybe "Redémarrer maintenant") is cancelled.
    CancelRestart,
    EndTx {
        connection: String,
        commit: bool,
    },
    Truncate {
        connection: String,
        tables: Vec<String>,
    },
    /// A button of the connection form (the form is the dialog).
    Connection(FormAction),
    /// A button of the structure dialog (the dialog is its own).
    Structure(StructureAction),
    /// Insert this history entry into the active console.
    InsertHistory(usize),
    OpenTable {
        connection: String,
        schema: String,
        table: String,
    },
    Batch(BatchStart),
    /// The folder prompt's button (the prompt is the dialog).
    Folder,
    /// The update modal's button, `None` when it was closed.
    Update(Option<UpdateAction>),
    /// Run console `id` (dangerous NoSQL commands confirmed).
    RunNoSql(TabId, RunKind),
    /// Run a confirmed NoSQL task.
    Task {
        connection: String,
        task: crate::gui::worker::nosql::NoSqlTask,
    },
    /// The primary button of a NoSQL form (the form is the dialog).
    Form,
    /// A request of the index dialog (the dialog is its own).
    Index(IndexAnswer),
    /// Start a NoSQL export / import.
    NoSqlBatch(NoSqlStart),
}

/// The effect of `answer` (`None`: cancelled) to `dialog`.
fn effect(dialog: &Dialog, answer: Option<Answer>) -> Effect {
    use Answer::*;
    match (dialog, answer) {
        (Dialog::ConfirmDeleteConnection(name), Some(Confirm)) => {
            Effect::DeleteConnection(name.clone())
        }
        (Dialog::DiscardEdits(id), answer) => Effect::AnswerDiscard(
            *id,
            match answer {
                Some(Submit) => Discard::Submit,
                Some(Drop) => Discard::Drop,
                _ => Discard::Cancel,
            },
        ),
        (Dialog::CloseTab { id, .. }, Some(Submit)) => Effect::SubmitAndClose(*id),
        (Dialog::CloseTab { id, .. }, Some(Drop)) => Effect::CloseTab(*id),
        (Dialog::ConfirmRerun { id, kind }, Some(Confirm)) => Effect::Rerun(*id, kind.clone()),
        (Dialog::ConfirmQuit { .. }, Some(Confirm)) => Effect::Quit,
        (Dialog::ConfirmQuit { .. }, None) => Effect::CancelRestart,
        (Dialog::EndTx(name), Some(a @ (Commit | Rollback))) => Effect::EndTx {
            connection: name.clone(),
            commit: a == Commit,
        },
        (Dialog::ConfirmTruncate { connection, tables }, Some(Confirm)) => Effect::Truncate {
            connection: connection.clone(),
            tables: tables.clone(),
        },
        (Dialog::Connection(_), Some(Answer::Connection(a))) => Effect::Connection(a),
        (Dialog::Structure(_), Some(Answer::Structure(a))) => Effect::Structure(a),
        (Dialog::History(_), Some(Insert(i))) => Effect::InsertHistory(i),
        (Dialog::TableSearch(search), Some(Open(i))) => match search.table(i) {
            Some((connection, schema, table)) => Effect::OpenTable {
                connection: connection.clone(),
                schema: schema.clone(),
                table: table.clone(),
            },
            None => Effect::Nothing,
        },
        (Dialog::Batch(b), Some(StartBatch)) => b.start().map_or(Effect::Nothing, Effect::Batch),
        (Dialog::Folder(_), Some(Answer::Folder)) => Effect::Folder,
        (Dialog::Update, Some(Answer::Update(a))) => Effect::Update(Some(a)),
        (Dialog::Update, None) => Effect::Update(None),
        (Dialog::ConfirmNoSqlRun { id, kind, .. }, Some(Confirm)) => {
            Effect::RunNoSql(*id, kind.clone())
        }
        (Dialog::ConfirmTask(c), Some(Confirm)) => Effect::Task {
            connection: c.connection.clone(),
            task: c.task.clone(),
        },
        (Dialog::NoSqlBatch(b), Some(FormOk)) => {
            b.start().map_or(Effect::Nothing, Effect::NoSqlBatch)
        }
        (Dialog::Prompt(_) | Dialog::NewKey(_) | Dialog::JsonEditor(_), Some(FormOk)) => {
            Effect::Form
        }
        (Dialog::Indexes(_), Some(Index(a))) => Effect::Index(a),
        _ => Effect::Nothing,
    }
}

/// Do `effect`; whether `dialog` stays open (only the forms' buttons keep
/// it open: a test, or a save refused by the form's checks).
fn apply(app: &mut App, ctx: &egui::Context, dialog: &mut Dialog, effect: Effect) -> bool {
    match effect {
        Effect::Connection(action) => {
            if let Dialog::Connection(form) = dialog {
                return form.act(app, action);
            }
        }
        Effect::Structure(action) => {
            if let Dialog::Structure(d) = dialog {
                return structure::act(app, d, action);
            }
        }
        Effect::Folder => {
            if let Dialog::Folder(prompt) = dialog {
                return super::explorer::apply_folder_prompt(app, prompt);
            }
        }
        Effect::Update(action) => return update::act(app, ctx, action),
        Effect::Form => return super::app::nosql::submit_form(app, dialog),
        Effect::Index(answer) => {
            if let Dialog::Indexes(d) = dialog {
                return super::app::nosql::index_answer(app, d, answer);
            }
        }
        Effect::RunNoSql(id, kind) => app.run_console_confirmed(id, kind),
        Effect::Task { connection, task } => app.nosql_task(&connection, task),
        Effect::NoSqlBatch(start) => transfer::start_nosql(app, start),
        Effect::Nothing => {}
        Effect::DeleteConnection(name) => app.delete_connection(&name),
        Effect::AnswerDiscard(id, answer) => answer_discard(app, id, answer),
        Effect::SubmitAndClose(id) => app.submit_and_close(id),
        Effect::CloseTab(id) => app.close_tab_id(id),
        Effect::Rerun(id, kind) => app.run_console(id, kind),
        Effect::Quit => {
            app.quit_confirmed = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        Effect::CancelRestart => app.updater.cancel_restart(),
        Effect::EndTx { connection, commit } => app.end_tx_then_auto(&connection, commit),
        Effect::Truncate { connection, tables } => transfer::truncate(app, &connection, tables),
        Effect::InsertHistory(index) => {
            let sql = app.history.get(index).map(|e| e.sql.clone());
            if let (Some(sql), Some(tab)) = (sql, app.tabs.active_mut()) {
                if let TabKind::Console(c) = &mut tab.kind {
                    c.insert(ctx, tab.id, &sql);
                }
            }
        }
        Effect::OpenTable {
            connection,
            schema,
            table,
        } => {
            let quotes = app.sessions.get(&connection).map(|s| s.quotes);
            if let Some(quotes) = quotes {
                app.open_data(&connection, &qualified(&schema, &table, quotes), &table);
            }
        }
        // A truncate opens its confirmation in the batch's place.
        Effect::Batch(start) => transfer::start_batch(app, start),
    }
    false
}

/// Answers to "Abandonner les modifications ?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Discard {
    Submit,
    Drop,
    Cancel,
}

fn answer_discard(app: &mut App, id: TabId, answer: Discard) {
    // The NoSQL tabs ask the same question.
    let nosql = app.tabs.find(id).and_then(|t| match &mut t.kind {
        TabKind::Documents(d) => Some(super::tabs::TabAction::Documents(match answer {
            Discard::Submit => d.submit_and_continue(),
            Discard::Drop => d.discard_and_continue()?,
            Discard::Cancel => {
                d.confirm = None;
                return None;
            }
        })),
        TabKind::Keys(k) => match answer {
            // Value edits are submitted by their own buttons: « Submit »
            // keeps the key open and applies them.
            Discard::Submit => {
                k.confirm = None;
                k.submit_pending().map(super::tabs::TabAction::Keys)
            }
            Discard::Drop => k.discard_and_continue().map(super::tabs::TabAction::Keys),
            Discard::Cancel => {
                k.confirm = None;
                None
            }
        },
        _ => None,
    });
    if let Some(action) = nosql {
        app.tab_action(id, action);
        return;
    }
    let action = app.tabs.find(id).and_then(|t| match &mut t.kind {
        TabKind::Data(d) => match answer {
            Discard::Submit => Some(d.submit_and_continue()),
            Discard::Drop => d.discard_and_continue(),
            Discard::Cancel => {
                d.confirm = None;
                None
            }
        },
        _ => None,
    });
    if let Some(action) = action {
        app.data_action(id, action);
    }
}

/// Outcomes addressed to an open dialog.
pub fn handle_event(app: &mut App, ev: Event) {
    if let Event::TestFinished(r) = ev {
        if let Some(Dialog::Connection(form)) = &mut app.dialog {
            form.testing = false;
            form.test_result = Some(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_outside_cancels_unless_a_button_was_clicked() {
        assert_eq!(settle(ModalResponse::None, false), None);
        assert_eq!(settle(ModalResponse::None, true), Some(None));
        assert_eq!(settle(ModalResponse::Cancel, false), Some(None));
        assert_eq!(
            settle(ModalResponse::Button(Answer::Drop), true),
            Some(Some(Answer::Drop))
        );
    }

    #[test]
    fn cancelling_keeps_the_side_effects_of_each_confirmation() {
        // The data tab stops waiting for an answer.
        assert_eq!(
            effect(&Dialog::DiscardEdits(7), None),
            Effect::AnswerDiscard(7, Discard::Cancel)
        );
        // "Redémarrer maintenant" is cancelled with the quit.
        let quit = Dialog::ConfirmQuit {
            edits: 1,
            transactions: vec![],
        };
        assert_eq!(effect(&quit, None), Effect::CancelRestart);
        for dialog in [
            Dialog::ConfirmDeleteConnection("prod".into()),
            Dialog::CloseTab {
                id: 3,
                risk: CloseRisk::DataEdits,
            },
            Dialog::ConfirmRerun {
                id: 3,
                kind: RunKind::Script("select 1".into()),
            },
            Dialog::EndTx("prod".into()),
            Dialog::ConfirmTruncate {
                connection: "prod".into(),
                tables: vec!["t".into()],
            },
        ] {
            assert_eq!(effect(&dialog, None), Effect::Nothing);
        }
    }

    #[test]
    fn each_button_has_its_effect() {
        assert_eq!(
            effect(
                &Dialog::ConfirmDeleteConnection("prod".into()),
                Some(Answer::Confirm)
            ),
            Effect::DeleteConnection("prod".into())
        );
        assert_eq!(
            effect(&Dialog::DiscardEdits(7), Some(Answer::Submit)),
            Effect::AnswerDiscard(7, Discard::Submit)
        );
        assert_eq!(
            effect(&Dialog::DiscardEdits(7), Some(Answer::Drop)),
            Effect::AnswerDiscard(7, Discard::Drop)
        );
        let close = |risk| Dialog::CloseTab { id: 4, risk };
        assert_eq!(
            effect(&close(CloseRisk::DataEdits), Some(Answer::Submit)),
            Effect::SubmitAndClose(4)
        );
        assert_eq!(
            effect(
                &close(CloseRisk::Console {
                    text: true,
                    edits: false
                }),
                Some(Answer::Drop)
            ),
            Effect::CloseTab(4)
        );
        let kind = RunKind::AtCursor {
            text: "select 1".into(),
            cursor: 2,
        };
        assert_eq!(
            effect(
                &Dialog::ConfirmRerun {
                    id: 5,
                    kind: kind.clone()
                },
                Some(Answer::Confirm)
            ),
            Effect::Rerun(5, kind)
        );
        let quit = Dialog::ConfirmQuit {
            edits: 0,
            transactions: vec![("prod".into(), 2)],
        };
        assert_eq!(effect(&quit, Some(Answer::Confirm)), Effect::Quit);
        for (answer, commit) in [(Answer::Commit, true), (Answer::Rollback, false)] {
            assert_eq!(
                effect(&Dialog::EndTx("prod".into()), Some(answer)),
                Effect::EndTx {
                    connection: "prod".into(),
                    commit
                }
            );
        }
        let truncate = Dialog::ConfirmTruncate {
            connection: "prod".into(),
            tables: vec!["a".into(), "b".into()],
        };
        assert_eq!(
            effect(&truncate, Some(Answer::Confirm)),
            Effect::Truncate {
                connection: "prod".into(),
                tables: vec!["a".into(), "b".into()]
            }
        );
    }

    fn connection_form() -> Dialog {
        Dialog::Connection(Box::new(ConnectionForm::new(None)))
    }

    fn structure() -> Dialog {
        Dialog::Structure(StructureDialog {
            connection: "prod".into(),
            table: "t".into(),
            title: "t".into(),
            edit: None,
            applying: false,
            error: None,
            goto: None,
        })
    }

    #[test]
    fn nosql_confirmations_run_only_when_confirmed() {
        let kind = RunKind::Script("db.users.drop()".into());
        let run = Dialog::ConfirmNoSqlRun {
            id: 4,
            kind: kind.clone(),
            warnings: vec![Warning {
                unit: "db.users.drop()".into(),
                reason: "Supprime la collection shop.users".into(),
            }],
        };
        assert_eq!(effect(&run, None), Effect::Nothing);
        assert_eq!(
            effect(&run, Some(Answer::Confirm)),
            Effect::RunNoSql(4, kind)
        );
        let task = crate::gui::worker::nosql::NoSqlTask::FlushDb(3);
        let confirm = Dialog::ConfirmTask(nosql::ConfirmTask {
            connection: "r".into(),
            title: "Vider la base".into(),
            text: "Toutes les clés de db3 seront supprimées.".into(),
            button: "Vider".into(),
            task: task.clone(),
        });
        assert_eq!(effect(&confirm, None), Effect::Nothing);
        assert_eq!(
            effect(&confirm, Some(Answer::Confirm)),
            Effect::Task {
                connection: "r".into(),
                task
            }
        );
        let prompt = Dialog::Prompt(nosql::Prompt::new(
            "m",
            nosql::PromptKind::NewCollection { db: "shop".into() },
            String::new(),
        ));
        assert_eq!(effect(&prompt, Some(Answer::FormOk)), Effect::Form);
        assert_eq!(effect(&prompt, None), Effect::Nothing);
        let indexes = Dialog::Indexes(Box::new(IndexDialog::new(
            "m",
            crate::engine::nosql::mongo::CollectionRef {
                db: "shop".into(),
                coll: "users".into(),
            },
        )));
        assert_eq!(
            effect(&indexes, Some(Answer::Index(IndexAnswer::Create))),
            Effect::Index(IndexAnswer::Create)
        );
        // An export not ready starts nothing.
        let batch = Dialog::NoSqlBatch(Box::new(NoSqlBatch::redis(
            "r",
            transfer::BatchKind::Export,
            0,
        )));
        assert_eq!(effect(&batch, Some(Answer::FormOk)), Effect::Nothing);
    }

    #[test]
    fn update_modal_buttons_and_closing() {
        assert_eq!(
            effect(&Dialog::Update, Some(Answer::Update(UpdateAction::Install))),
            Effect::Update(Some(UpdateAction::Install))
        );
        // Plus tard / Fermer / ×: the updater puts the update off.
        assert_eq!(effect(&Dialog::Update, None), Effect::Update(None));
    }

    #[test]
    fn form_buttons_reach_their_form() {
        for action in [FormAction::Save, FormAction::Test, FormAction::CancelTest] {
            assert_eq!(
                effect(&connection_form(), Some(Answer::Connection(action))),
                Effect::Connection(action)
            );
        }
        for action in [StructureAction::Apply, StructureAction::CloseEdit] {
            assert_eq!(
                effect(&structure(), Some(Answer::Structure(action))),
                Effect::Structure(action)
            );
        }
    }

    #[test]
    fn cancelling_a_form_just_closes_it() {
        // Annuler, ×, Escape or a click outside: the dialog closes (`show`
        // then stops the connection test and forgets its orphaned sign-ins).
        for response in [ModalResponse::Cancel, ModalResponse::None] {
            assert_eq!(settle(response, true), Some(None));
        }
        assert_eq!(settle(ModalResponse::Cancel, false), Some(None));
        assert_eq!(effect(&connection_form(), None), Effect::Nothing);
        assert_eq!(effect(&structure(), None), Effect::Nothing);
    }

    fn table_search() -> Dialog {
        Dialog::TableSearch(TableSearch::new(vec![(
            "prod".into(),
            "public".into(),
            "orders".into(),
        )]))
    }

    fn batch(dir: Option<&str>) -> Dialog {
        let schemas = [crate::engine::models::SchemaInfo {
            name: "main".into(),
            tables: vec!["a".into()],
            expanded: true,
        }];
        let mut b = BatchDialog::new("prod", transfer::BatchKind::Export, &schemas);
        b.set_all(true);
        b.dir = dir.map(Into::into);
        Dialog::Batch(b)
    }

    #[test]
    fn list_dialogs_act_on_their_selection() {
        let history = Dialog::History(HistoryPopup::new(String::new()));
        assert_eq!(
            effect(&history, Some(Answer::Insert(4))),
            Effect::InsertHistory(4)
        );
        assert_eq!(
            effect(&table_search(), Some(Answer::Open(0))),
            Effect::OpenTable {
                connection: "prod".into(),
                schema: "public".into(),
                table: "orders".into()
            }
        );
        assert_eq!(
            effect(&table_search(), Some(Answer::Open(3))),
            Effect::Nothing,
            "gone"
        );
        let Effect::Batch(start) = effect(&batch(Some("dump")), Some(Answer::StartBatch)) else {
            panic!("the batch starts");
        };
        assert_eq!(start.tables, vec![("main".to_string(), "a".to_string())]);
        assert_eq!(
            effect(&batch(None), Some(Answer::StartBatch)),
            Effect::Nothing,
            "not ready"
        );
        // Cancelling them does nothing.
        for dialog in [history, table_search(), batch(Some("dump"))] {
            assert_eq!(effect(&dialog, None), Effect::Nothing);
        }
    }
}
