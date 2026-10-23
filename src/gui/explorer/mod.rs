//! Database explorer (left panel): connection → schemas → tables → columns /
//! keys / indexes, loaded lazily. Opening a connection node connects it;
//! opening a table node fetches its details once.
//!
//! The tree is flattened into a list of one-line rows, rebuilt only when the
//! filter, the sessions or an open state change, and drawn with
//! `ScrollArea::show_rows`: only the visible rows are laid out, so thousands
//! of tables cost nothing while idle or scrolling.

pub mod tree;

use egui::collapsing_header::paint_default_icon;
use egui::{DragAndDrop, Id, Label, Response, RichText, Sense, Stroke, StrokeKind};
use egui_phosphor::regular as icon;

use crate::engine::config::folders::{self, SEP};
use crate::engine::config::AppConfig;
use crate::engine::models::{
    AzureAuthMethod, ConnectionConfig, DatabaseType, Family, TableDetails,
};
use crate::engine::nosql::mongo::CollectionRef;
use crate::engine::nosql::redis::{self, KeyInfo};
use crate::engine::ops::transfer::qualified;

use super::app::{nosql, App};
use super::dialogs::folder::{FolderGoal, FolderPrompt};
use super::dialogs::transfer::{self, BatchKind};
use super::dialogs::{self, connection::ConnectionForm, Dialog};
use super::fk;
use super::rows::fixed_row;
use super::sessions::{redis_db, Session, Sessions};
use super::status::one_line;
use super::tabs::documents::DocMode;
use super::tabs::keys::{ttl_label, type_icon, type_label};
use super::theme::{self, ERROR};
use tree::{
    can_drop, conn_views, connection_id, flatten, folder_id, group_error, group_id, has_pk,
    inputs_key, key_group_id, schema_id, show_hidden_id, table_id, DragItem, Group, KeyRow, Row,
    RowKind, TableAt, Tree,
};

/// What the user asked for while the tree was drawn (applied afterwards, so
/// drawing only needs shared borrows).
enum Action {
    /// A new connection in this folder (`""`: the root).
    NewConnection(String),
    /// Name a new folder in this one (`""`: the root).
    NewFolder(String),
    RenameFolder(String),
    /// Delete this folder; its content moves up one level.
    DeleteFolder(String),
    /// Move connection `name` into `folder` (`""`: the root).
    MoveConnection {
        name: String,
        folder: String,
    },
    /// Move connection `name` into a folder to name.
    MoveToNewFolder(String),
    /// `item` was dropped into folder `target` (`""`: the root).
    Drop {
        item: DragItem,
        target: String,
    },
    SetSchemaHidden {
        connection: String,
        schema: String,
        hidden: bool,
    },
    RefreshAll,
    /// Open or close a node.
    SetOpen(Id, bool),
    Connect(String),
    Disconnect(String),
    Refresh(String),
    Edit(String),
    Delete(String),
    /// Forget the Entra ID sign-in of this connection.
    ForgetSignIn(String),
    NewConsole {
        connection: String,
        query: String,
    },
    LoadDetails {
        connection: String,
        table: String,
    },
    OpenData {
        connection: String,
        table: String,
        title: String,
    },
    Ddl {
        connection: String,
        table: String,
        title: String,
    },
    Structure {
        connection: String,
        table: String,
        title: String,
    },
    ImportCsv {
        connection: String,
        table: String,
    },
    Truncate {
        connection: String,
        table: String,
    },
    /// Batch dialog; `only` = `(schema, table)` ticked alone.
    Batch {
        connection: String,
        kind: BatchKind,
        only: Option<(String, String)>,
    },
    Copy(String),
    /// List the keys of a Redis database.
    LoadKeys {
        connection: String,
        db: u8,
    },
    /// MongoDB: a collection in this database.
    NewCollection {
        connection: String,
        db: String,
    },
    DropDatabase {
        connection: String,
        db: String,
    },
    OpenDocuments {
        connection: String,
        coll: CollectionRef,
        mode: DocMode,
    },
    Indexes {
        connection: String,
        coll: CollectionRef,
    },
    /// Export / import of one collection.
    CollectionTransfer {
        connection: String,
        coll: CollectionRef,
        kind: BatchKind,
    },
    EmptyCollection {
        connection: String,
        coll: CollectionRef,
    },
    DropCollection {
        connection: String,
        coll: CollectionRef,
    },
    /// Redis: the « Clés » tab of a database, on `key` when given.
    OpenKeys {
        connection: String,
        db: u8,
        key: Option<Vec<u8>>,
    },
    NewKey {
        connection: String,
        db: u8,
    },
    /// Export / import of a Redis database.
    RedisTransfer {
        connection: String,
        db: u8,
        kind: BatchKind,
    },
    FlushDb {
        connection: String,
        db: u8,
    },
    RenameKey {
        connection: String,
        db: u8,
        key: Vec<u8>,
    },
    ExpireKey {
        connection: String,
        db: u8,
        key: Vec<u8>,
        ttl: Option<i64>,
    },
    DeleteKey {
        connection: String,
        db: u8,
        key: Vec<u8>,
    },
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    let top = ui.cursor().top();
    ui.horizontal(|ui| {
        ui.strong("Explorateur");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(icon::PLUS)
                .on_hover_text("Nouvelle connexion")
                .clicked()
            {
                actions.push(Action::NewConnection(String::new()));
            }
            if ui
                .small_button(icon::FOLDER_PLUS)
                .on_hover_text("Nouveau dossier")
                .clicked()
            {
                actions.push(Action::NewFolder(String::new()));
            }
            if ui
                .add_enabled(
                    !app.sessions.open.is_empty(),
                    egui::Button::new(icon::ARROW_CLOCKWISE).small(),
                )
                .on_hover_text("Rafraîchir les connexions ouvertes")
                .clicked()
            {
                actions.push(Action::RefreshAll);
            }
        });
    });
    ui.add(
        egui::TextEdit::singleline(&mut app.explorer_filter)
            .hint_text(format!("{} Filtrer les tables", icon::MAGNIFYING_GLASS))
            .desired_width(f32::INFINITY),
    );
    let header = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=ui.cursor().top());
    if let Some(item) = DragAndDrop::payload::<DragItem>(ui.ctx()) {
        dragging_ui(ui, header, &app.config.connections, &item, &mut actions);
    }
    ui.separator();
    if app.config.connections.is_empty() {
        ui.label(RichText::new("Aucune connexion. Cliquez sur + pour en ajouter une.").weak());
    }
    tree_ui(
        ui,
        &mut app.explorer_tree,
        app.explorer_filter.trim(),
        &app.config,
        &app.sessions,
        &mut actions,
    );
    for action in actions {
        apply(app, ui.ctx(), action);
    }
}

/// The scrolling tree: only its visible rows are laid out.
fn tree_ui(
    ui: &mut egui::Ui,
    tree: &mut Tree,
    filter: &str,
    config: &AppConfig,
    sessions: &Sessions,
    actions: &mut Vec<Action>,
) {
    let connections = &config.connections;
    let show_hidden: Vec<bool> = connections
        .iter()
        .map(|c| tree.is_open(show_hidden_id(&c.name)).unwrap_or(false))
        .collect();
    let key = inputs_key(filter, config, sessions, tree.generation);
    let flat = tree.rows(key, |open| {
        flatten(
            &folders::tree(config),
            &conn_views(connections, sessions),
            filter,
            |id| open.get(&id).copied(),
        )
    });
    for (conn, table) in &flat.needs_details {
        let Some(name) = connections.get(*conn).map(|c| &c.name) else {
            continue;
        };
        if sessions
            .get(name)
            .is_some_and(|s| !s.details.contains_key(table) && !s.loading.contains_key(table))
        {
            actions.push(Action::LoadDetails {
                connection: name.clone(),
                table: table.clone(),
            });
        }
    }
    for (conn, db) in &flat.needs_keys {
        let Some(name) = connections.get(*conn).map(|c| &c.name) else {
            continue;
        };
        if sessions
            .get(name)
            .is_some_and(|s| !s.redis_keys.contains_key(db) && !s.keys_loading.contains_key(db))
        {
            actions.push(Action::LoadKeys {
                connection: name.clone(),
                db: *db,
            });
        }
    }
    let cx = Cx {
        connections,
        sessions,
        key_rows: &flat.key_rows,
        row_height: ui.spacing().interact_size.y,
        folders: &flat.folders,
        all_folders: &flat.all_folders,
        show_hidden: &show_hidden,
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, cx.row_height, flat.rows.len(), |ui, range| {
            for row in &flat.rows[range] {
                row_ui(ui, &cx, row, actions);
            }
            if DragAndDrop::has_payload_of_type::<DragItem>(ui.ctx()) {
                drag_scroll(ui, cx.row_height);
            }
        });
}

/// Scroll speed while dragging by an edge of the tree, in points / second.
const DRAG_SCROLL_SPEED: f32 = 600.0;

/// While dragging, scroll the tree when the pointer is within one row of
/// its top or bottom edge (`ui`: the scroll area's content).
fn drag_scroll(ui: &mut egui::Ui, edge: f32) {
    let Some(pointer) = ui.ctx().pointer_latest_pos() else {
        return;
    };
    let view = ui.clip_rect();
    let direction = edge_direction(view.y_range(), pointer.y, edge);
    if direction == 0.0 || !view.x_range().contains(pointer.x) {
        return;
    }
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    ui.scroll_with_delta(egui::vec2(0.0, direction * DRAG_SCROLL_SPEED * dt));
    ui.ctx().request_repaint();
}

/// `1.0` (scroll up) with `y` within `edge` of the top of `view`, `-1.0`
/// (down) within `edge` of its bottom, else `0.0` (also outside it).
fn edge_direction(view: egui::Rangef, y: f32, edge: f32) -> f32 {
    if !view.contains(y) {
        0.0
    } else if y < view.min + edge {
        1.0
    } else if y > view.max - edge {
        -1.0
    } else {
        0.0
    }
}

/// While an item is dragged: what it is, by the pointer, and the root as a
/// drop target over the explorer's `header` (title and filter: covering
/// them leaves the rows where they are).
fn dragging_ui(
    ui: &mut egui::Ui,
    header: egui::Rect,
    connections: &[ConnectionConfig],
    item: &DragItem,
    actions: &mut Vec<Action>,
) {
    let text = match item {
        DragItem::Connection(name) => format!("{} {name}", icon::DATABASE),
        DragItem::Folder(path) => format!("{} {}", icon::FOLDER, folders::name(path)),
    };
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), Id::new("explorer_drag"), |ui| {
        ui.add(Label::new(text).extend())
    });
    if !can_drop(item, "", |name| folder_of(connections, name)) {
        return;
    }
    let visuals = ui.visuals();
    let painter = ui.painter();
    painter.rect(
        header,
        4.0,
        visuals.panel_fill,
        visuals.widgets.noninteractive.bg_stroke,
        StrokeKind::Inside,
    );
    painter.text(
        header.center(),
        egui::Align2::CENTER_CENTER,
        format!(
            "{} Déposer ici pour ranger à la racine",
            icon::ARROW_LINE_UP
        ),
        egui::TextStyle::Body.resolve(ui.style()),
        visuals.weak_text_color(),
    );
    drop_target(ui, header, connections, "", actions);
}

/// The folder of connection `name` (`""`: the root).
fn folder_of(connections: &[ConnectionConfig], name: &str) -> String {
    connections
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| c.folder.as_deref())
        .and_then(|f| folders::normalize(f).ok())
        .unwrap_or_default()
}

/// Highlight `rect` while the dragged item can be dropped into folder
/// `target` (`""`: the root) and the pointer is over it; drop it there
/// when released.
fn drop_target(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    connections: &[ConnectionConfig],
    target: &str,
    actions: &mut Vec<Action>,
) {
    let ctx = ui.ctx().clone();
    let Some(item) = DragAndDrop::payload::<DragItem>(&ctx) else {
        return;
    };
    if !ui.rect_contains_pointer(rect) {
        return;
    }
    if !can_drop(&item, target, |name| folder_of(connections, name)) {
        ctx.set_cursor_icon(egui::CursorIcon::NotAllowed);
        return;
    }
    let selection = ui.visuals().selection;
    ui.painter().rect(
        rect,
        3.0,
        selection.bg_fill.gamma_multiply(0.3),
        Stroke::new(1.5, selection.stroke.color),
        StrokeKind::Inside,
    );
    if ctx.input(|i| i.pointer.any_released()) {
        if let Some(item) = DragAndDrop::take_payload::<DragItem>(&ctx) {
            actions.push(Action::Drop {
                item: (*item).clone(),
                target: target.to_string(),
            });
        }
    }
}

/// Paths of every folder, depth first (the « Dossier » choices).
pub fn folder_paths(config: &AppConfig) -> Vec<String> {
    fn walk(node: &folders::FolderNode, out: &mut Vec<String>) {
        for f in &node.folders {
            out.push(f.path.clone());
            walk(f, out);
        }
    }
    let mut out = Vec::new();
    walk(&folders::tree(config), &mut out);
    out
}

/// The connection form for `config` (`None`: a new one), with the folders
/// to choose from and, when connected, its schemas.
fn connection_form(app: &App, config: Option<ConnectionConfig>) -> ConnectionForm {
    let schemas = config.as_ref().and_then(|c| {
        let s = app.sessions.get(&c.name)?;
        Some(s.schemas.iter().map(|s| s.name.clone()).collect())
    });
    ConnectionForm::new(config).with_context(folder_paths(&app.config), schemas)
}

/// Apply the folder prompt's name; whether the prompt stays open (refused:
/// it shows why).
pub fn apply_folder_prompt(app: &mut App, prompt: &mut FolderPrompt) -> bool {
    let before = folder_paths(&app.config);
    match prompt.apply(&mut app.config) {
        Ok(path) => {
            if let FolderGoal::Rename { from } = &prompt.goal {
                app.explorer_tree.folder_moved(&before, from, &path);
            }
            app.save_config();
            false
        }
        Err(e) => {
            prompt.error = Some(e);
            true
        }
    }
}

/// Hide or show `schema` of `connection`, in the config and its session.
fn set_schema_hidden(app: &mut App, connection: &str, schema: &str, hidden: bool) {
    app.config.set_schema_hidden(connection, schema, hidden);
    let list = app
        .config
        .connections
        .iter()
        .find(|c| c.name == connection)
        .map(|c| c.hidden_schemas.clone());
    if let (Some(list), Some(s)) = (list, app.sessions.get_mut(connection)) {
        s.config.hidden_schemas = list;
    }
    app.save_config();
}

/// Apply a config change of the explorer: saved, or its error reported.
fn change_config(app: &mut App, change: impl FnOnce(&mut AppConfig) -> Result<(), String>) {
    match change(&mut app.config) {
        Ok(()) => app.save_config(),
        Err(e) => app.error(e),
    }
}

fn apply(app: &mut App, ctx: &egui::Context, action: Action) {
    match action {
        Action::NewConnection(folder) => {
            let mut form = connection_form(app, None);
            form.folder = folder;
            app.dialog = Some(Dialog::Connection(Box::new(form)));
        }
        Action::NewFolder(parent) => {
            let goal = FolderGoal::Create { parent };
            app.dialog = Some(Dialog::Folder(FolderPrompt::new(goal)));
        }
        Action::RenameFolder(from) => {
            let goal = FolderGoal::Rename { from };
            app.dialog = Some(Dialog::Folder(FolderPrompt::new(goal)));
        }
        Action::DeleteFolder(path) => {
            let before = folder_paths(&app.config);
            match app.config.delete_folder(&path) {
                Ok(()) => {
                    app.explorer_tree.folder_deleted(&before, &path);
                    app.save_config();
                }
                Err(e) => app.error(e),
            }
        }
        Action::MoveConnection { name, folder } => {
            change_config(app, |c| c.move_connection(&name, Some(&folder)))
        }
        Action::MoveToNewFolder(connection) => {
            let goal = FolderGoal::MoveConnection { connection };
            app.dialog = Some(Dialog::Folder(FolderPrompt::new(goal)));
        }
        Action::Drop { item, target } => {
            let destination = item.destination(&target);
            match item {
                DragItem::Connection(name) => {
                    change_config(app, |c| c.move_connection(&name, Some(&destination)))
                }
                DragItem::Folder(path) => {
                    let before = folder_paths(&app.config);
                    match app.config.rename_folder(&path, &destination) {
                        Ok(()) => {
                            tree::keep_moved_folder(&mut app.config, &destination);
                            app.explorer_tree.folder_moved(&before, &path, &destination);
                            app.save_config();
                        }
                        Err(e) => app.error(e),
                    }
                }
            }
        }
        Action::SetSchemaHidden {
            connection,
            schema,
            hidden,
        } => set_schema_hidden(app, &connection, &schema, hidden),
        Action::RefreshAll => {
            let names: Vec<String> = app.sessions.open.keys().cloned().collect();
            for name in names {
                app.refresh(&name);
            }
        }
        Action::SetOpen(id, open) => app.explorer_tree.set_open(id, open),
        Action::Connect(name) => app.connect(&name),
        Action::Disconnect(name) => app.disconnect(&name),
        Action::Refresh(name) => app.refresh(&name),
        Action::Edit(name) => {
            if let Some(c) = app.config.connections.iter().find(|c| c.name == name) {
                let form = connection_form(app, Some(c.clone()));
                app.dialog = Some(Dialog::Connection(Box::new(form)));
            }
        }
        Action::Delete(name) => app.dialog = Some(Dialog::ConfirmDeleteConnection(name)),
        Action::ForgetSignIn(name) => app.forget_sign_in(&name),
        Action::NewConsole { connection, query } => {
            app.new_console(&connection, query);
        }
        Action::LoadDetails { connection, table } => {
            if let Some(s) = app.sessions.get_mut(&connection) {
                s.load_details(&mut app.worker, &connection, &table);
            }
        }
        Action::OpenData {
            connection,
            table,
            title,
        } => app.open_data(&connection, &table, &title),
        Action::Ddl {
            connection,
            table,
            title,
        } => app.open_ddl(&connection, &table, &title),
        Action::Structure {
            connection,
            table,
            title,
        } => dialogs::structure::open(app, &connection, &table, &title),
        Action::ImportCsv { connection, table } => transfer::open_import(app, &connection, &table),
        Action::Truncate { connection, table } => {
            transfer::open_truncate(app, &connection, vec![table])
        }
        Action::Batch {
            connection,
            kind,
            only,
        } => {
            let only = only.as_ref().map(|(s, t)| (s.as_str(), t.as_str()));
            transfer::open_batch(app, &connection, kind, only)
        }
        Action::Copy(text) => ctx.copy_text(text),
        Action::LoadKeys { connection, db } => {
            if let Some(s) = app.sessions.get_mut(&connection) {
                s.load_keys(&mut app.worker, &connection, db);
            }
        }
        Action::NewCollection { connection, db } => {
            let kind = dialogs::nosql::PromptKind::NewCollection { db };
            let prompt = dialogs::nosql::Prompt::new(&connection, kind, String::new());
            app.dialog = Some(Dialog::Prompt(prompt));
        }
        Action::DropDatabase { connection, db } => {
            app.dialog = Some(nosql::confirm_drop_database(&connection, db))
        }
        Action::OpenDocuments {
            connection,
            coll,
            mode,
        } => app.open_documents(&connection, coll, mode),
        Action::Indexes { connection, coll } => app.open_indexes(&connection, coll),
        Action::CollectionTransfer {
            connection,
            coll,
            kind,
        } => transfer::open_batch(app, &connection, kind, Some((&coll.db, &coll.coll))),
        Action::EmptyCollection { connection, coll } => {
            app.dialog = Some(nosql::confirm_empty_collection(&connection, coll))
        }
        Action::DropCollection { connection, coll } => {
            app.dialog = Some(nosql::confirm_drop_collection(&connection, coll))
        }
        Action::OpenKeys {
            connection,
            db,
            key,
        } => app.open_keys(&connection, db, key),
        Action::NewKey { connection, db } => {
            app.dialog = Some(Dialog::NewKey(dialogs::nosql::NewKey::new(&connection, db)))
        }
        Action::RedisTransfer {
            connection,
            db,
            kind,
        } => {
            let b = transfer::NoSqlBatch::redis(&connection, kind, db);
            app.dialog = Some(Dialog::NoSqlBatch(Box::new(b)));
        }
        Action::FlushDb { connection, db } => {
            app.dialog = Some(nosql::confirm_flush(&connection, db))
        }
        Action::RenameKey {
            connection,
            db,
            key,
        } => app.dialog = Some(nosql::rename_prompt(&connection, db, key)),
        Action::ExpireKey {
            connection,
            db,
            key,
            ttl,
        } => app.dialog = Some(nosql::expire_prompt(&connection, db, key, ttl)),
        Action::DeleteKey {
            connection,
            db,
            key,
        } => app.dialog = Some(nosql::confirm_delete_key(&connection, db, key)),
    }
}

/// What drawing a row reads.
struct Cx<'a> {
    connections: &'a [ConnectionConfig],
    sessions: &'a Sessions,
    /// `Flat::key_rows`.
    key_rows: &'a [KeyRow],
    row_height: f32,
    /// `Flat::folders`.
    folders: &'a [String],
    /// `Flat::all_folders`.
    all_folders: &'a [String],
    /// Per connection: whether « Afficher les schémas masqués » is on.
    show_hidden: &'a [bool],
}

/// A table row's names, resolved against the current sessions (`None` if
/// they changed since the rows were built: the row is left blank).
struct TableRef<'a> {
    connection: &'a str,
    session: &'a Session,
    schema: &'a str,
    table: &'a str,
}

/// A Redis database of a row, resolved against the current sessions.
struct RedisDbRef<'a> {
    connection: &'a str,
    schema: &'a str,
    index: u8,
    /// Its listed keys, when listed.
    keys: Option<&'a [KeyInfo]>,
    error: Option<&'a str>,
}

impl<'a> Cx<'a> {
    fn redis_db(&self, conn: usize, schema: usize) -> Option<RedisDbRef<'a>> {
        let connection = &self.connections.get(conn)?.name;
        let session = self.sessions.get(connection)?;
        let schema = &session.schemas.get(schema)?.name;
        let index = redis_db(schema)?;
        let listed = session.redis_keys.get(&index);
        Some(RedisDbRef {
            connection,
            schema,
            index,
            keys: listed.and_then(|k| k.as_ref().ok()).map(|k| &k.keys[..]),
            error: listed.and_then(|k| k.as_ref().err()).map(String::as_str),
        })
    }

    fn table(&self, at: TableAt) -> Option<TableRef<'a>> {
        let connection = &self.connections.get(at.conn)?.name;
        let session = self.sessions.get(connection)?;
        let schema = session.schemas.get(at.schema)?;
        Some(TableRef {
            connection,
            session,
            schema: &schema.name,
            table: schema.tables.get(at.table)?,
        })
    }
}

impl<'a> TableRef<'a> {
    /// Qualified, quoted name (key of the session's details).
    fn key(&self) -> String {
        qualified(self.schema, self.table, self.session.quotes)
    }

    fn details(&self) -> Option<&'a Result<TableDetails, String>> {
        self.session.details.get(&self.key())
    }
}

/// One row of the tree (see `fixed_row`).
fn row_ui(ui: &mut egui::Ui, cx: &Cx, row: &Row, actions: &mut Vec<Action>) {
    fixed_row(ui, cx.row_height, row.kind, |ui, rect| {
        row_contents(ui, rect, cx, row, actions)
    });
}

fn row_contents(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    cx: &Cx,
    row: &Row,
    actions: &mut Vec<Action>,
) {
    let indent = ui.spacing().indent;
    if ui.visuals().indent_has_left_vline {
        // The guide lines of the ancestors' bodies, joined across the spacing.
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let bottom = rect.bottom() + ui.spacing().item_spacing.y;
        for level in 0..row.depth {
            let x = rect.left() + (f32::from(level) + 0.5) * indent;
            ui.painter().vline(x, rect.top()..=bottom, stroke);
        }
    }
    ui.add_space(f32::from(row.depth) * indent);
    match row.kind {
        RowKind::Folder {
            folder,
            count,
            open,
        } => {
            if let Some(path) = cx.folders.get(folder) {
                folder_row(ui, path, count, open, actions);
                drop_target(ui, rect, cx.connections, path, actions);
            }
        }
        RowKind::Connection { conn, open } => {
            if let Some(config) = cx.connections.get(conn) {
                let show_hidden = cx.show_hidden.get(conn).copied().unwrap_or(false);
                connection_row(ui, cx, config, open, show_hidden, actions);
                // In a folder, the row takes drops for that folder.
                let parent = folder_of(cx.connections, &config.name);
                if !parent.is_empty() {
                    drop_target(ui, rect, cx.connections, &parent, actions);
                }
            }
        }
        RowKind::Note(text) => {
            ui.label(RichText::new(text).weak());
        }
        RowKind::ConnError { conn } => {
            let error = cx
                .connections
                .get(conn)
                .and_then(|c| cx.sessions.errors.get(&c.name));
            if let Some(e) = error {
                error_line(ui, e);
            }
        }
        RowKind::Schema {
            conn,
            schema,
            count,
            open,
            hidden,
        } => {
            let names = cx.connections.get(conn).and_then(|c| {
                let s = cx.sessions.get(&c.name)?.schemas.get(schema)?;
                Some((&c.name, &s.name))
            });
            if let Some((connection, schema)) = names {
                let session = cx.sessions.get(connection);
                let row = SchemaRow {
                    connection,
                    schema,
                    count,
                    open,
                    hidden,
                    family: session.map_or(Family::Sql, |s| s.config.db_type.family()),
                    keys: redis_db(schema).and_then(|db| session?.key_count(db)),
                };
                schema_row(ui, &row, actions);
            }
        }
        RowKind::Table { at, open } => {
            if let Some(t) = cx.table(at) {
                if t.session.config.db_type == DatabaseType::MongoDB {
                    collection_row(ui, &t, open, actions);
                } else {
                    table_row(ui, &t, open, actions);
                }
            }
        }
        RowKind::KeyGroup {
            conn,
            schema,
            node,
            open,
        } => {
            if let (Some(db), Some(n)) = (cx.redis_db(conn, schema), cx.key_rows.get(node)) {
                let toggled = toggle(ui, open).clicked();
                let glyph = if open {
                    icon::FOLDER_OPEN
                } else {
                    icon::FOLDER
                };
                let label = clickable(ui, format!("{glyph} {}", n.label));
                ui.label(RichText::new(n.count.to_string()).weak().small());
                if toggled || label.clicked() {
                    actions.push(Action::SetOpen(
                        key_group_id(db.connection, db.schema, &n.prefix),
                        !open,
                    ));
                }
                label.on_hover_text(&n.prefix);
            }
        }
        RowKind::Key { conn, schema, node } => {
            if let (Some(db), Some(n)) = (cx.redis_db(conn, schema), cx.key_rows.get(node)) {
                let info = n.key.and_then(|k| db.keys?.get(k));
                if let Some(info) = info {
                    key_row(ui, &db, n, info, actions);
                }
            }
        }
        RowKind::KeysError { conn, schema } => {
            let e = cx.redis_db(conn, schema).and_then(|db| db.error);
            if let Some(e) = e {
                error_line(ui, e);
            }
        }
        RowKind::KeysMore { conn, schema } => {
            if let Some(db) = cx.redis_db(conn, schema) {
                let text = format!(
                    "… {} premières clés : utiliser l'onglet Clés",
                    crate::gui::worker::nosql::EXPLORER_MAX_KEYS
                );
                if ui
                    .link(RichText::new(text).weak().small())
                    .on_hover_text("Parcourir toutes les clés (SCAN, motif, type)")
                    .clicked()
                {
                    actions.push(Action::OpenKeys {
                        connection: db.connection.to_string(),
                        db: db.index,
                        key: None,
                    });
                }
            }
        }
        RowKind::DetailsLoading => {
            ui.spinner();
            ui.label(RichText::new("Chargement…").weak());
        }
        RowKind::DetailsError { at } => {
            if let Some(Err(e)) = cx.table(at).and_then(|t| t.details()) {
                error_line(ui, e);
            }
        }
        RowKind::Group {
            at,
            group,
            count,
            open,
        } => {
            if let Some(t) = cx.table(at) {
                let id = group_id(t.connection, t.schema, t.table, group);
                let mongo = t.session.config.db_type == DatabaseType::MongoDB;
                let (glyph, title) = match group {
                    Group::Columns if mongo => (icon::COLUMNS, "Champs (échantillon)"),
                    Group::Columns => (icon::COLUMNS, "Colonnes"),
                    Group::Keys => (icon::KEY, "Clés"),
                    Group::Indexes => (icon::LIST_BULLETS, "Index"),
                };
                let toggled = toggle(ui, open).clicked();
                if toggled || clickable(ui, format!("{glyph} {title} ({count})")).clicked() {
                    actions.push(Action::SetOpen(id, !open));
                }
            }
        }
        RowKind::Item { at, group, index } => {
            if let Some(t) = cx.table(at) {
                let mongo = t.session.config.db_type == DatabaseType::MongoDB;
                if let Some(Ok(d)) = t.details() {
                    item_row(ui, d, group, index, mongo);
                }
            }
        }
        RowKind::GroupError { at, group } => {
            let d = cx.table(at).and_then(|t| t.details());
            if let Some(e) = d
                .and_then(|d| d.as_ref().ok())
                .and_then(|d| group_error(d, group))
            {
                ui.add(
                    Label::new(
                        RichText::new("Non disponible sur ce serveur")
                            .weak()
                            .small(),
                    )
                    .truncate(),
                )
                .on_hover_text(e);
            }
        }
    }
}

/// The arrow of a node.
fn toggle(ui: &mut egui::Ui, open: bool) -> Response {
    let spacing = ui.spacing().item_spacing.x;
    ui.spacing_mut().item_spacing.x = 0.0; // the arrow uses the full indent width
    let size = egui::vec2(ui.spacing().indent, ui.spacing().icon_width);
    let (_, rect) = ui.allocate_space(size);
    let response = ui.interact(rect, ui.id().with("toggle"), Sense::click());
    // Same small icon as egui's collapsing headers.
    let (mut icon, _) = ui.spacing().icon_rectangles(rect);
    icon.set_center(egui::pos2(rect.left() + size.x / 2.0, rect.center().y));
    let small = response.clone().with_new_rect(icon);
    paint_default_icon(ui, if open { 1.0 } else { 0.0 }, &small);
    ui.spacing_mut().item_spacing.x = spacing;
    response
}

fn clickable(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(Label::new(text).sense(Sense::click()).selectable(false))
}

/// A label that can be clicked and dragged.
fn draggable(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(
        Label::new(text)
            .sense(Sense::click_and_drag())
            .selectable(false),
    )
}

fn folder_row(ui: &mut egui::Ui, path: &str, count: usize, open: bool, actions: &mut Vec<Action>) {
    let toggled = toggle(ui, open).clicked();
    let glyph = if open {
        icon::FOLDER_OPEN
    } else {
        icon::FOLDER
    };
    let label = draggable(ui, format!("{glyph} {}", folders::name(path)));
    ui.label(RichText::new(count.to_string()).weak().small());
    if toggled || label.clicked() {
        actions.push(Action::SetOpen(folder_id(path), !open));
    }
    label.dnd_set_drag_payload(DragItem::Folder(path.to_string()));
    let label = label.on_hover_text(path.replace(SEP, " › "));
    label.context_menu(|ui| {
        let items = [
            (
                icon::PLUS,
                "Nouvelle connexion ici",
                Action::NewConnection(path.into()),
            ),
            (
                icon::FOLDER_PLUS,
                "Nouveau sous-dossier…",
                Action::NewFolder(path.into()),
            ),
            (
                icon::PENCIL_SIMPLE,
                "Renommer…",
                Action::RenameFolder(path.into()),
            ),
        ];
        for (glyph, text, action) in items {
            if ui.button(format!("{glyph} {text}")).clicked() {
                actions.push(action);
                ui.close_menu();
            }
        }
        ui.separator();
        if ui
            .button(format!("{} Supprimer", icon::TRASH))
            .on_hover_text("Son contenu remonte d'un niveau")
            .clicked()
        {
            actions.push(Action::DeleteFolder(path.into()));
            ui.close_menu();
        }
    });
}

/// What a schema row shows.
struct SchemaRow<'a> {
    connection: &'a str,
    schema: &'a str,
    count: usize,
    open: bool,
    /// Hidden, shown greyed (« Afficher les schémas masqués »).
    hidden: bool,
    family: Family,
    /// Redis: the key count (`Session::key_count`) and whether it is a
    /// lower bound; `None` when not known yet.
    keys: Option<(u64, bool)>,
}

/// « 1 234 clés », « 5 000+ clés » (keys listed up to the explorer's
/// limit), nothing while unknown.
fn key_count_label(keys: Option<(u64, bool)>) -> String {
    let Some((n, more)) = keys else {
        return String::new();
    };
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push('\u{202f}');
        }
        grouped.push(c);
    }
    let plus = if more { "+" } else { "" };
    let s = if n == 1 && !more { "" } else { "s" };
    format!("{grouped}{plus} clé{s}")
}

fn schema_row(ui: &mut egui::Ui, row: &SchemaRow, actions: &mut Vec<Action>) {
    let set_hidden = |hidden| Action::SetSchemaHidden {
        connection: row.connection.to_string(),
        schema: row.schema.to_string(),
        hidden,
    };
    let toggled = toggle(ui, row.open).clicked();
    let mut text = RichText::new(format!("{} {}", icon::DATABASE, row.schema));
    if row.hidden {
        text = text.weak().italics();
    }
    let label = clickable(ui, text);
    let count = match row.family {
        Family::KeyValue => key_count_label(row.keys),
        _ => row.count.to_string(),
    };
    ui.label(RichText::new(count).weak().small());
    if toggled || label.clicked() {
        let id = schema_id(row.connection, row.schema);
        actions.push(Action::SetOpen(id, !row.open));
    }
    if row.hidden
        && ui
            .small_button("Réafficher")
            .on_hover_text("Ce schéma est masqué")
            .clicked()
    {
        actions.push(set_hidden(false));
    }
    label.context_menu(|ui| {
        match row.family {
            Family::Document => mongo_db_menu(ui, row, actions),
            Family::KeyValue => redis_db_menu(ui, row, actions),
            Family::Sql => {}
        }
        let (glyph, text) = match (row.hidden, row.family) {
            (true, Family::Sql) => (icon::EYE, "Réafficher le schéma"),
            (false, Family::Sql) => (icon::EYE_SLASH, "Masquer le schéma"),
            (true, _) => (icon::EYE, "Réafficher la base"),
            (false, _) => (icon::EYE_SLASH, "Masquer la base"),
        };
        if ui.button(format!("{glyph} {text}")).clicked() {
            actions.push(set_hidden(!row.hidden));
            ui.close_menu();
        }
    });
}

/// A menu item pushing `action`.
fn menu_item(ui: &mut egui::Ui, text: String, action: Action, actions: &mut Vec<Action>) {
    if ui.button(text).clicked() {
        actions.push(action);
        ui.close_menu();
    }
}

/// The items of a MongoDB database's menu (before « Masquer »).
fn mongo_db_menu(ui: &mut egui::Ui, row: &SchemaRow, actions: &mut Vec<Action>) {
    let (connection, db) = (row.connection.to_string(), row.schema.to_string());
    menu_item(
        ui,
        format!("{} Nouvelle collection…", icon::FOLDER_SIMPLE_PLUS),
        Action::NewCollection {
            connection: connection.clone(),
            db: db.clone(),
        },
        actions,
    );
    menu_item(
        ui,
        format!("{} Nouvelle console", icon::TERMINAL_WINDOW),
        Action::NewConsole {
            connection: connection.clone(),
            query: format!("use {db}\n"),
        },
        actions,
    );
    ui.separator();
    if ui
        .button(RichText::new(format!("{} Supprimer la base…", icon::TRASH)).color(ERROR))
        .clicked()
    {
        actions.push(Action::DropDatabase { connection, db });
        ui.close_menu();
    }
    ui.separator();
}

/// The items of a Redis database's menu (before « Masquer »).
fn redis_db_menu(ui: &mut egui::Ui, row: &SchemaRow, actions: &mut Vec<Action>) {
    let Some(db) = redis_db(row.schema) else {
        return;
    };
    let connection = row.connection.to_string();
    menu_item(
        ui,
        format!("{} Parcourir les clés", icon::LIST_MAGNIFYING_GLASS),
        Action::OpenKeys {
            connection: connection.clone(),
            db,
            key: None,
        },
        actions,
    );
    menu_item(
        ui,
        format!("{} Nouvelle clé…", icon::PLUS),
        Action::NewKey {
            connection: connection.clone(),
            db,
        },
        actions,
    );
    menu_item(
        ui,
        format!("{} Nouvelle console", icon::TERMINAL_WINDOW),
        Action::NewConsole {
            connection: connection.clone(),
            query: format!("SELECT {db}\n"),
        },
        actions,
    );
    menu_item(
        ui,
        format!("{} Rafraîchir les clés", icon::ARROW_CLOCKWISE),
        Action::LoadKeys {
            connection: connection.clone(),
            db,
        },
        actions,
    );
    ui.separator();
    menu_item(
        ui,
        format!("{} Exporter…", icon::EXPORT),
        Action::RedisTransfer {
            connection: connection.clone(),
            db,
            kind: BatchKind::Export,
        },
        actions,
    );
    menu_item(
        ui,
        format!("{} Importer…", icon::DOWNLOAD_SIMPLE),
        Action::RedisTransfer {
            connection: connection.clone(),
            db,
            kind: BatchKind::Import,
        },
        actions,
    );
    if ui
        .button(RichText::new(format!("{} Vider la base…", icon::ERASER)).color(ERROR))
        .clicked()
    {
        actions.push(Action::FlushDb { connection, db });
        ui.close_menu();
    }
    ui.separator();
}

/// A MongoDB collection: the documents on double-click, its menu.
fn collection_row(ui: &mut egui::Ui, t: &TableRef, open: bool, actions: &mut Vec<Action>) {
    let TableRef {
        connection,
        schema,
        table,
        ..
    } = *t;
    if toggle(ui, open).clicked() {
        actions.push(Action::SetOpen(table_id(connection, schema, table), !open));
    }
    let label = clickable(ui, format!("{} {table}", icon::FILES));
    let connection = connection.to_string();
    let coll = CollectionRef {
        db: schema.to_string(),
        coll: table.to_string(),
    };
    let documents = |mode| Action::OpenDocuments {
        connection: connection.clone(),
        coll: coll.clone(),
        mode,
    };
    if label.double_clicked() {
        actions.push(documents(DocMode::Find));
    }
    label.context_menu(|ui| {
        menu_item(
            ui,
            format!("{} Ouvrir les documents", icon::FILES),
            documents(DocMode::Find),
            actions,
        );
        menu_item(
            ui,
            format!("{} Nouvelle console", icon::TERMINAL_WINDOW),
            Action::NewConsole {
                connection: connection.clone(),
                query: format!(
                    "use {}\n{}",
                    coll.db,
                    crate::gui::tabs::documents::find_command(&coll, "", "", "")
                ),
            },
            actions,
        );
        menu_item(
            ui,
            format!("{} Agrégation…", icon::FUNNEL),
            documents(DocMode::Aggregate),
            actions,
        );
        menu_item(
            ui,
            format!("{} Index…", icon::LIST_BULLETS),
            Action::Indexes {
                connection: connection.clone(),
                coll: coll.clone(),
            },
            actions,
        );
        ui.separator();
        menu_item(
            ui,
            format!("{} Exporter (JSON)…", icon::EXPORT),
            Action::CollectionTransfer {
                connection: connection.clone(),
                coll: coll.clone(),
                kind: BatchKind::Export,
            },
            actions,
        );
        menu_item(
            ui,
            format!("{} Importer (JSON)…", icon::DOWNLOAD_SIMPLE),
            Action::CollectionTransfer {
                connection: connection.clone(),
                coll: coll.clone(),
                kind: BatchKind::Import,
            },
            actions,
        );
        if ui
            .button(RichText::new(format!("{} Vider la collection…", icon::ERASER)).color(ERROR))
            .clicked()
        {
            actions.push(Action::EmptyCollection {
                connection: connection.clone(),
                coll: coll.clone(),
            });
            ui.close_menu();
        }
        if ui
            .button(RichText::new(format!("{} Supprimer la collection…", icon::TRASH)).color(ERROR))
            .clicked()
        {
            actions.push(Action::DropCollection {
                connection: connection.clone(),
                coll: coll.clone(),
            });
            ui.close_menu();
        }
        ui.separator();
        menu_item(
            ui,
            format!("{} Copier le nom", icon::COPY),
            Action::Copy(coll.to_string()),
            actions,
        );
    });
}

/// A Redis key: type icon, name, TTL badge; opened on double-click.
fn key_row(
    ui: &mut egui::Ui,
    db: &RedisDbRef,
    n: &KeyRow,
    info: &KeyInfo,
    actions: &mut Vec<Action>,
) {
    let label = clickable(ui, format!("{} {}", type_icon(&info.kind), n.label));
    if info.ttl.is_some() {
        ui.label(
            RichText::new(format!("{} {}", icon::TIMER, ttl_label(info.ttl)))
                .weak()
                .small(),
        );
    }
    let connection = db.connection.to_string();
    let key = info.key.clone();
    let open = || Action::OpenKeys {
        connection: connection.clone(),
        db: db.index,
        key: Some(key.clone()),
    };
    if label.double_clicked() {
        actions.push(open());
    }
    let name = redis::display_bytes(&key);
    let label = label.on_hover_text(format!("{name} · {}", type_label(&info.kind)));
    label.context_menu(|ui| {
        menu_item(ui, format!("{} Ouvrir", icon::KEY), open(), actions);
        menu_item(
            ui,
            format!("{} Renommer…", icon::PENCIL_SIMPLE),
            Action::RenameKey {
                connection: connection.clone(),
                db: db.index,
                key: key.clone(),
            },
            actions,
        );
        menu_item(
            ui,
            format!("{} Expiration…", icon::TIMER),
            Action::ExpireKey {
                connection: connection.clone(),
                db: db.index,
                key: key.clone(),
                ttl: info.ttl,
            },
            actions,
        );
        menu_item(
            ui,
            format!("{} Copier le nom", icon::COPY),
            Action::Copy(name.clone()),
            actions,
        );
        ui.separator();
        if ui
            .button(RichText::new(format!("{} Supprimer", icon::TRASH)).color(ERROR))
            .clicked()
        {
            actions.push(Action::DeleteKey {
                connection: connection.clone(),
                db: db.index,
                key: key.clone(),
            });
            ui.close_menu();
        }
    });
}

/// « Déplacer vers… »: the root, every folder, or a new one.
fn move_menu(ui: &mut egui::Ui, cx: &Cx, name: &str, actions: &mut Vec<Action>) {
    let current = folder_of(cx.connections, name);
    let mut target = None;
    if ui
        .add_enabled(
            !current.is_empty(),
            egui::Button::new(format!("{} Racine", icon::ARROW_LINE_UP)),
        )
        .clicked()
    {
        target = Some(String::new());
    }
    for path in cx.all_folders {
        let depth = path.matches(SEP).count();
        let text = format!(
            "{}{} {}",
            "    ".repeat(depth),
            icon::FOLDER,
            folders::name(path)
        );
        if ui
            .add_enabled(*path != current, egui::Button::new(text))
            .on_hover_text(path.replace(SEP, " › "))
            .clicked()
        {
            target = Some(path.clone());
        }
    }
    ui.separator();
    if ui
        .button(format!("{} Nouveau dossier…", icon::FOLDER_PLUS))
        .clicked()
    {
        actions.push(Action::MoveToNewFolder(name.to_string()));
        ui.close_menu();
    }
    if let Some(folder) = target {
        actions.push(Action::MoveConnection {
            name: name.to_string(),
            folder,
        });
        ui.close_menu();
    }
}

fn connection_row(
    ui: &mut egui::Ui,
    cx: &Cx,
    config: &ConnectionConfig,
    open: bool,
    show_hidden: bool,
    actions: &mut Vec<Action>,
) {
    let name = &config.name;
    let session = cx.sessions.get(name);
    let connecting = cx.sessions.connecting.contains(name);
    let toggled = toggle(ui, open).clicked();
    theme::dot(ui, theme::connection_color(config.color));
    let label = draggable(ui, RichText::new(name).strong());
    label.dnd_set_drag_payload(DragItem::Connection(name.clone()));
    ui.label(RichText::new(config.display_type()).weak().small());
    if connecting {
        ui.spinner();
    }
    if toggled || label.clicked() {
        actions.push(Action::SetOpen(connection_id(name), !open));
        if !open && session.is_none() && !connecting {
            actions.push(Action::Connect(name.clone()));
        }
    }

    label.context_menu(|ui| {
        if session.is_some() {
            if ui
                .button(format!("{} Nouvelle console", icon::TERMINAL_WINDOW))
                .clicked()
            {
                actions.push(Action::NewConsole {
                    connection: name.clone(),
                    query: String::new(),
                });
                ui.close_menu();
            }
            if ui
                .button(format!("{} Rafraîchir", icon::ARROW_CLOCKWISE))
                .clicked()
            {
                actions.push(Action::Refresh(name.clone()));
                ui.close_menu();
            }
            if ui.button(format!("{} Déconnecter", icon::PLUGS)).clicked() {
                actions.push(Action::Disconnect(name.clone()));
                ui.close_menu();
            }
        } else if ui
            .add_enabled(
                !connecting,
                egui::Button::new(format!("{} Se connecter", icon::PLUG)),
            )
            .clicked()
        {
            actions.push(Action::Connect(name.clone()));
            ui.close_menu();
        }
        let hidden = config.hidden_schemas.len();
        if hidden > 0 {
            let mut show = show_hidden;
            let text = format!("Afficher les schémas masqués ({hidden})");
            if ui.checkbox(&mut show, text).clicked() {
                actions.push(Action::SetOpen(show_hidden_id(name), show));
                ui.close_menu();
            }
        }
        ui.separator();
        if ui
            .button(format!("{} Modifier", icon::PENCIL_SIMPLE))
            .clicked()
        {
            actions.push(Action::Edit(name.clone()));
            ui.close_menu();
        }
        ui.menu_button(format!("{} Déplacer vers…", icon::FOLDER_SIMPLE), |ui| {
            move_menu(ui, cx, name, actions)
        });
        if keeps_sign_in(config)
            && ui
                .button(format!("{} Oublier la connexion Azure AD", icon::SIGN_OUT))
                .on_hover_text("La prochaine connexion repassera par le navigateur")
                .clicked()
        {
            actions.push(Action::ForgetSignIn(name.clone()));
            ui.close_menu();
        }
        if ui
            .add_enabled(
                session.is_none() && !connecting,
                egui::Button::new(format!("{} Supprimer", icon::TRASH)),
            )
            .on_disabled_hover_text("Déconnectez d'abord")
            .clicked()
        {
            actions.push(Action::Delete(name.clone()));
            ui.close_menu();
        }
        if session.is_some() {
            ui.separator();
            let batches: &[(BatchKind, &str, &str)] = match config.db_type.family() {
                Family::Sql => &[
                    (BatchKind::Export, icon::EXPORT, "Export par lot…"),
                    (BatchKind::Import, icon::DOWNLOAD_SIMPLE, "Import par lot…"),
                    (BatchKind::Truncate, icon::ERASER, "Vidage par lot…"),
                ],
                // JSON / JSON Lines / CSV; no truncate of every collection.
                Family::Document => &[
                    (BatchKind::Export, icon::EXPORT, "Export par lot…"),
                    (BatchKind::Import, icon::DOWNLOAD_SIMPLE, "Import par lot…"),
                ],
                // Per database (its menu).
                Family::KeyValue => &[],
            };
            for &(kind, glyph, label) in batches {
                if ui.button(format!("{glyph} {label}")).clicked() {
                    actions.push(Action::Batch {
                        connection: name.clone(),
                        kind,
                        only: None,
                    });
                    ui.close_menu();
                }
            }
        }
    });
}

/// Whether `config` signs in with the user's Entra ID account, whose tokens
/// are kept (and can be forgotten).
fn keeps_sign_in(config: &ConnectionConfig) -> bool {
    config.db_type == DatabaseType::Azure
        && config.azure_auth_method == Some(AzureAuthMethod::InteractiveBrowser)
}

fn table_row(ui: &mut egui::Ui, t: &TableRef, open: bool, actions: &mut Vec<Action>) {
    let TableRef {
        connection,
        schema,
        table,
        ..
    } = *t;
    let connection = connection.to_string();
    // The arrow toggles; the label opens the data (double-click) or a menu.
    if toggle(ui, open).clicked() {
        actions.push(Action::SetOpen(table_id(&connection, schema, table), !open));
    }
    let label = clickable(ui, format!("{} {table}", icon::TABLE));
    let open_data = || Action::OpenData {
        connection: connection.clone(),
        table: t.key(),
        title: table.to_string(),
    };
    if label.double_clicked() {
        actions.push(open_data());
    }
    label.context_menu(|ui| {
        if ui
            .button(format!("{} Ouvrir les données", icon::TABLE))
            .clicked()
        {
            actions.push(open_data());
            ui.close_menu();
        }
        if ui
            .button(format!("{} Nouvelle console", icon::TERMINAL_WINDOW))
            .clicked()
        {
            actions.push(Action::NewConsole {
                connection: connection.clone(),
                query: format!("SELECT * FROM {}", t.key()),
            });
            ui.close_menu();
        }
        if ui.button(format!("{} DDL", icon::CODE)).clicked() {
            actions.push(Action::Ddl {
                connection: connection.clone(),
                table: t.key(),
                title: table.to_string(),
            });
            ui.close_menu();
        }
        if ui.button(format!("{} Structure…", icon::COLUMNS)).clicked() {
            actions.push(Action::Structure {
                connection: connection.clone(),
                table: t.key(),
                title: table.to_string(),
            });
            ui.close_menu();
        }
        ui.separator();
        if ui.button(format!("{} Exporter…", icon::EXPORT)).clicked() {
            actions.push(Action::Batch {
                connection: connection.clone(),
                kind: BatchKind::Export,
                only: Some((schema.to_string(), table.to_string())),
            });
            ui.close_menu();
        }
        if ui
            .button(format!("{} Importer un CSV…", icon::DOWNLOAD_SIMPLE))
            .clicked()
        {
            actions.push(Action::ImportCsv {
                connection: connection.clone(),
                table: t.key(),
            });
            ui.close_menu();
        }
        if ui
            .button(RichText::new(format!("{} Vider la table…", icon::ERASER)).color(ERROR))
            .clicked()
        {
            actions.push(Action::Truncate {
                connection: connection.clone(),
                table: t.key(),
            });
            ui.close_menu();
        }
        ui.separator();
        if ui.button(format!("{} Copier le nom", icon::COPY)).clicked() {
            actions.push(Action::Copy(t.key()));
            ui.close_menu();
        }
    });
}

/// A (possibly multi-line) error on one small truncated line, full on hover.
fn error_line(ui: &mut egui::Ui, e: &str) {
    ui.add(Label::new(RichText::new(one_line(e)).color(ERROR).small()).truncate())
        .on_hover_text(e);
}

/// Line `index` of `group` of a table's details (`mongo`: a collection's
/// sampled fields).
fn item_row(ui: &mut egui::Ui, d: &TableDetails, group: Group, index: usize, mongo: bool) {
    match group {
        Group::Columns => {
            let Some(c) = d.columns.get(index) else {
                return;
            };
            let color = fk::link_color(ui.visuals().dark_mode);
            if c.is_primary_key {
                ui.label(RichText::new(icon::KEY).color(color))
                    .on_hover_text("Clé primaire");
            }
            let link = fk::link_of(&d.foreign_keys, &c.name);
            if let Some(l) = &link {
                ui.label(
                    RichText::new(format!("{} FK", icon::LINK))
                        .small()
                        .strong()
                        .color(color),
                )
                .on_hover_text(l.tooltip());
            }
            let name = ui.label(&c.name);
            if let Some(l) = &link {
                name.on_hover_text(l.tooltip());
            }
            ui.label(RichText::new(&c.type_name).weak());
            if mongo && c.nullable && !c.is_primary_key {
                ui.label(RichText::new("partiel").weak().small())
                    .on_hover_text("Absent ou null dans une partie de l'échantillon");
            } else if !mongo && !c.nullable {
                ui.label(RichText::new("NOT NULL").weak().small());
            }
        }
        Group::Keys => {
            let pk = usize::from(has_pk(d));
            if index < pk {
                let pk: Vec<&str> = d
                    .columns
                    .iter()
                    .filter(|c| c.is_primary_key)
                    .map(|c| c.name.as_str())
                    .collect();
                ui.label(format!("PK ({})", pk.join(", ")));
            } else if let Some(fk) = d.foreign_keys.get(index - pk) {
                ui.label(format!(
                    "{} ({}) {} {}({})",
                    fk.name,
                    fk.columns.join(", "),
                    icon::ARROW_RIGHT,
                    fk.ref_table,
                    fk.ref_columns.join(", ")
                ));
            }
        }
        Group::Indexes => {
            let Some(idx) = d.indexes.get(index) else {
                return;
            };
            ui.label(&idx.name);
            ui.label(RichText::new(format!("({})", idx.columns.join(", "))).weak());
            if idx.unique {
                ui.label(RichText::new("UNIQUE").weak().small());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::SchemaInfo;

    #[test]
    fn redis_key_counts_are_grouped_and_bounded() {
        assert_eq!(key_count_label(None), "");
        assert_eq!(key_count_label(Some((0, false))), "0 clés");
        assert_eq!(key_count_label(Some((1, false))), "1 clé");
        assert_eq!(key_count_label(Some((1234, false))), "1\u{202f}234 clés");
        assert_eq!(key_count_label(Some((5000, true))), "5\u{202f}000+ clés");
        assert_eq!(
            key_count_label(Some((1_234_567, false))),
            "1\u{202f}234\u{202f}567 clés"
        );
    }

    #[test]
    fn dragging_by_an_edge_scrolls_towards_it() {
        let view = egui::Rangef::new(100.0, 400.0);
        assert_eq!(edge_direction(view, 105.0, 20.0), 1.0, "top: up");
        assert_eq!(edge_direction(view, 395.0, 20.0), -1.0, "bottom: down");
        assert_eq!(edge_direction(view, 250.0, 20.0), 0.0);
        // Above the tree is the root's drop zone: no scrolling there.
        assert_eq!(edge_direction(view, 90.0, 20.0), 0.0);
        assert_eq!(edge_direction(view, 410.0, 20.0), 0.0);
    }

    #[test]
    fn only_browser_sign_ins_can_be_forgotten() {
        let browser = ConnectionConfig {
            db_type: DatabaseType::Azure,
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            ..Default::default()
        };
        assert!(keeps_sign_in(&browser));
        let cli = ConnectionConfig {
            azure_auth_method: Some(AzureAuthMethod::Interactive),
            ..browser.clone()
        };
        assert!(!keeps_sign_in(&cli));
        let sql_server = ConnectionConfig {
            db_type: DatabaseType::SQLServer,
            ..browser
        };
        assert!(!keeps_sign_in(&sql_server));
    }

    /// 3 connections × 5 000 tables, all open.
    /// `cargo test --release -- --ignored bench_explorer --nocapture`
    #[test]
    #[ignore]
    fn bench_explorer_frames() {
        use crate::engine::db::connector::DatabaseConnection;
        use crate::engine::models::DatabaseType;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_lazy("sqlite::memory:")
            .unwrap();
        let conn = std::sync::Arc::new(DatabaseConnection::SQLite(pool));
        let mut sessions = Sessions::default();
        let configs: Vec<ConnectionConfig> = (0..3)
            .map(|i| ConnectionConfig {
                name: format!("db{i}"),
                db_type: DatabaseType::SQLite,
                ..Default::default()
            })
            .collect();
        for c in &configs {
            let schemas = vec![SchemaInfo {
                name: "main".into(),
                tables: (0..5000).map(|t| format!("table_{t:05}")).collect(),
                expanded: true,
            }];
            sessions.open.insert(
                c.name.clone(),
                Session::new(c.clone(), conn.clone(), schemas),
            );
        }
        let config = AppConfig {
            connections: configs.clone(),
            ..Default::default()
        };
        let tree = std::cell::RefCell::new(Tree::default());
        for c in &configs {
            tree.borrow_mut().set_open(connection_id(&c.name), true);
        }
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 900.0));
        let frame = |events: Vec<egui::Event>, filter: &str| {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut actions = Vec::new();
                    tree_ui(
                        ui,
                        &mut tree.borrow_mut(),
                        filter,
                        &config,
                        &sessions,
                        &mut actions,
                    );
                });
            });
            std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
        };
        let time = |label: &str, n: usize, mut f: Box<dyn FnMut(usize) + '_>| {
            let t = std::time::Instant::now();
            for i in 0..n {
                f(i);
            }
            eprintln!(
                "explorer {label}: {:.2} ms/frame",
                t.elapsed().as_secs_f64() * 1000.0 / n as f64
            );
        };
        for _ in 0..3 {
            frame(Vec::new(), "");
        }
        time("idle", 30, Box::new(|_| frame(Vec::new(), "")));
        let over = egui::pos2(150.0, 450.0);
        time(
            "scrolling",
            60,
            Box::new(|i| {
                let dy = if i < 30 { -60.0 } else { 60.0 };
                frame(
                    vec![
                        egui::Event::PointerMoved(over),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, dy),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    "",
                )
            }),
        );
        time(
            "filtered (\"table_04\")",
            30,
            Box::new(|_| frame(Vec::new(), "table_04")),
        );
    }
}
