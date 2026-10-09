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
use egui::{Id, Label, Response, RichText, Sense};
use egui_phosphor::regular as icon;

use crate::engine::models::{ConnectionConfig, TableDetails};
use crate::engine::ops::transfer::qualified;

use super::app::App;
use super::dialogs::transfer::{self, BatchKind};
use super::dialogs::{self, connection::ConnectionForm, Dialog};
use super::rows::fixed_row;
use super::sessions::{Session, Sessions};
use super::status::one_line;
use super::theme::{self, ACCENT, ERROR};
use tree::{
    conn_views, connection_id, flatten, group_id, has_pk, inputs_key, schema_id, table_id, Group,
    Row, RowKind, TableAt, Tree,
};

/// What the user asked for while the tree was drawn (applied afterwards, so
/// drawing only needs shared borrows).
enum Action {
    NewConnection,
    RefreshAll,
    /// Open or close a node.
    SetOpen(Id, bool),
    Connect(String),
    Disconnect(String),
    Refresh(String),
    Edit(String),
    Delete(String),
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
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.strong("Explorateur");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button(icon::PLUS)
                .on_hover_text("Nouvelle connexion")
                .clicked()
            {
                actions.push(Action::NewConnection);
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
    ui.separator();
    if app.config.connections.is_empty() {
        ui.label(RichText::new("Aucune connexion. Cliquez sur + pour en ajouter une.").weak());
    }
    tree_ui(
        ui,
        &mut app.explorer_tree,
        app.explorer_filter.trim(),
        &app.config.connections,
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
    connections: &[ConnectionConfig],
    sessions: &Sessions,
    actions: &mut Vec<Action>,
) {
    let key = inputs_key(filter, connections, sessions, tree.generation);
    let flat = tree.rows(key, |open| {
        flatten(&conn_views(connections, sessions), filter, |id| {
            open.get(&id).copied()
        })
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
    let cx = Cx {
        connections,
        sessions,
        row_height: ui.spacing().interact_size.y,
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, cx.row_height, flat.rows.len(), |ui, range| {
            for row in &flat.rows[range] {
                row_ui(ui, &cx, row, actions);
            }
        });
}

fn apply(app: &mut App, ctx: &egui::Context, action: Action) {
    match action {
        Action::NewConnection => {
            app.dialog = Some(Dialog::Connection(Box::new(ConnectionForm::new(None))));
        }
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
                app.dialog = Some(Dialog::Connection(Box::new(ConnectionForm::new(Some(
                    c.clone(),
                )))));
            }
        }
        Action::Delete(name) => app.dialog = Some(Dialog::ConfirmDeleteConnection(name)),
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
    }
}

/// What drawing a row reads.
struct Cx<'a> {
    connections: &'a [ConnectionConfig],
    sessions: &'a Sessions,
    row_height: f32,
}

/// A table row's names, resolved against the current sessions (`None` if
/// they changed since the rows were built: the row is left blank).
struct TableRef<'a> {
    connection: &'a str,
    session: &'a Session,
    schema: &'a str,
    table: &'a str,
}

impl<'a> Cx<'a> {
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
        RowKind::Connection { conn, open } => {
            if let Some(config) = cx.connections.get(conn) {
                connection_row(ui, cx, config, open, actions);
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
        } => {
            let names = cx.connections.get(conn).and_then(|c| {
                let s = cx.sessions.get(&c.name)?.schemas.get(schema)?;
                Some((&c.name, &s.name))
            });
            if let Some((connection, schema)) = names {
                let id = schema_id(connection, schema);
                let toggled = toggle(ui, open).clicked();
                let label = clickable(ui, format!("{} {schema}", icon::DATABASE));
                ui.label(RichText::new(count.to_string()).weak().small());
                if toggled || label.clicked() {
                    actions.push(Action::SetOpen(id, !open));
                }
            }
        }
        RowKind::Table { at, open } => {
            if let Some(t) = cx.table(at) {
                table_row(ui, &t, open, actions);
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
                let (glyph, title) = match group {
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
            if let Some(Ok(d)) = cx.table(at).and_then(|t| t.details()) {
                item_row(ui, d, group, index);
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

fn connection_row(
    ui: &mut egui::Ui,
    cx: &Cx,
    config: &ConnectionConfig,
    open: bool,
    actions: &mut Vec<Action>,
) {
    let name = &config.name;
    let session = cx.sessions.get(name);
    let connecting = cx.sessions.connecting.contains(name);
    let toggled = toggle(ui, open).clicked();
    theme::dot(ui, theme::connection_color(config.color));
    let label = clickable(ui, RichText::new(name).strong());
    ui.label(RichText::new(config.db_type.to_string()).weak().small());
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
        ui.separator();
        if ui
            .button(format!("{} Modifier", icon::PENCIL_SIMPLE))
            .clicked()
        {
            actions.push(Action::Edit(name.clone()));
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
            let batches = [
                (BatchKind::Export, icon::EXPORT, "Export par lot…"),
                (BatchKind::Import, icon::DOWNLOAD_SIMPLE, "Import par lot…"),
                (BatchKind::Truncate, icon::ERASER, "Vidage par lot…"),
            ];
            for (kind, glyph, label) in batches {
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

/// Line `index` of `group` of a table's details.
fn item_row(ui: &mut egui::Ui, d: &TableDetails, group: Group, index: usize) {
    match group {
        Group::Columns => {
            let Some(c) = d.columns.get(index) else {
                return;
            };
            if c.is_primary_key {
                ui.label(RichText::new(icon::KEY).color(ACCENT))
                    .on_hover_text("Clé primaire");
            }
            ui.label(&c.name);
            ui.label(RichText::new(&c.type_name).weak());
            if !c.nullable {
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
                        &configs,
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
