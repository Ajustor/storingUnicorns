//! Database explorer (left panel): connection → schemas → tables → columns /
//! keys / indexes, loaded lazily. Opening a connection node connects it;
//! opening a table node fetches its details once.

use egui::collapsing_header::{paint_default_icon, CollapsingState};
use egui::{Label, Response, RichText, Sense};
use egui_phosphor::regular as icon;

use crate::engine::models::{ConnectionConfig, SchemaInfo, TableDetails};
use crate::engine::ops::transfer::qualified;

use super::app::App;
use super::dialogs::transfer::{self, BatchKind};
use super::dialogs::{self, connection::ConnectionForm, Dialog};
use super::sessions::{Session, Sessions};
use super::status::one_line;
use super::theme::{self, ACCENT, ERROR};

/// `(schema, tables)` pairs whose table names contain `filter`
/// (case-insensitive); schemas without a match are dropped.
pub fn filter_tables<'a>(schemas: &'a [SchemaInfo], filter: &str) -> Vec<(&'a str, Vec<&'a str>)> {
    let needle = filter.to_lowercase();
    schemas
        .iter()
        .filter_map(|s| {
            let tables: Vec<&str> = s
                .tables
                .iter()
                .map(String::as_str)
                .filter(|t| needle.is_empty() || t.to_lowercase().contains(&needle))
                .collect();
            (!tables.is_empty()).then_some((s.name.as_str(), tables))
        })
        .collect()
}

/// What the user asked for while the tree was drawn (applied afterwards, so
/// drawing only needs shared borrows).
enum Action {
    NewConnection,
    RefreshAll,
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
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let filter = app.explorer_filter.trim();
            for config in &app.config.connections {
                connection_node(ui, config, &app.sessions, filter, &mut actions);
            }
        });
    for action in actions {
        apply(app, ui.ctx(), action);
    }
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

struct Node {
    /// The header's clickable label (context menus, double-clicks).
    label: Response,
    open: bool,
    /// Opened by the user during this frame.
    just_opened: bool,
}

/// How a tree row opens.
#[derive(Clone, Copy)]
struct NodeOpts {
    default_open: bool,
    /// Show the body without touching the stored state (filtering).
    force_open: bool,
    /// `false` collapses a node whose body has nothing to show (a closed
    /// connection).
    can_stay_open: bool,
    /// A click on the label toggles the node (else only the arrow does).
    toggle_on_label: bool,
}

/// A collapsible tree row: arrow + `header`, then `body` when open.
fn node(
    ui: &mut egui::Ui,
    id: egui::Id,
    opts: NodeOpts,
    header: impl FnOnce(&mut egui::Ui) -> Response,
    body: impl FnOnce(&mut egui::Ui),
) -> Node {
    let NodeOpts {
        default_open,
        force_open,
        can_stay_open,
        toggle_on_label,
    } = opts;
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    if !can_stay_open && state.is_open() {
        state.set_open(false);
    }
    let was_open = state.is_open();
    let row = ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = 0.0; // the toggle uses the full indent width
        state.show_toggle_button(ui, paint_default_icon);
        ui.spacing_mut().item_spacing.x = spacing;
        header(ui)
    });
    let label = row.inner;
    if toggle_on_label && label.clicked() {
        state.toggle(ui);
    }
    let open = state.is_open();
    if force_open {
        ui.indent(id, body);
        state.store(ui.ctx());
    } else {
        state.show_body_indented(&row.response, ui, body);
    }
    Node {
        label,
        open: open || force_open,
        just_opened: open && !was_open,
    }
}

fn clickable(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(Label::new(text).sense(Sense::click()).selectable(false))
}

fn connection_node(
    ui: &mut egui::Ui,
    config: &ConnectionConfig,
    sessions: &Sessions,
    filter: &str,
    actions: &mut Vec<Action>,
) {
    let name = &config.name;
    let session = sessions.get(name);
    let connecting = sessions.connecting.contains(name);
    // While filtering, only open connections with matching tables are shown.
    let filtered = if filter.is_empty() {
        None
    } else {
        let Some(s) = session else { return };
        let groups = filter_tables(&s.schemas, filter);
        if groups.is_empty() {
            return;
        }
        Some(groups)
    };

    let id = ui.make_persistent_id(("connection", name));
    let n = node(
        ui,
        id,
        NodeOpts {
            default_open: false,
            force_open: filtered.is_some(),
            can_stay_open: session.is_some() || connecting,
            toggle_on_label: true,
        },
        |ui| {
            theme::dot(ui, theme::connection_color(config.color));
            let label = clickable(ui, RichText::new(name).strong());
            ui.label(RichText::new(config.db_type.to_string()).weak().small());
            if connecting {
                ui.spinner();
            }
            label
        },
        |ui| match (session, &filtered) {
            (None, _) => {
                ui.label(RichText::new("Connexion…").weak());
            }
            (Some(s), Some(groups)) => {
                for (schema, tables) in groups {
                    schema_node(ui, s, schema, tables, true, actions);
                }
            }
            (Some(s), None) if s.schemas.is_empty() => {
                ui.label(RichText::new("Aucune table").weak());
            }
            (Some(s), None) => {
                for schema in &s.schemas {
                    schema_node(ui, s, &schema.name, &schema.tables, false, actions);
                }
            }
        },
    );
    if session.is_none() {
        if let Some(e) = sessions.errors.get(name) {
            error_line(ui, e);
        }
        if n.just_opened && !connecting {
            actions.push(Action::Connect(name.clone()));
        }
    }

    n.label.context_menu(|ui| {
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

fn schema_node<T: AsRef<str>>(
    ui: &mut egui::Ui,
    session: &Session,
    schema: &str,
    tables: &[T],
    force_open: bool,
    actions: &mut Vec<Action>,
) {
    let id = ui.make_persistent_id(("schema", &session.config.name, schema));
    node(
        ui,
        id,
        NodeOpts {
            default_open: true,
            force_open,
            can_stay_open: true,
            toggle_on_label: true,
        },
        |ui| {
            let label = clickable(ui, format!("{} {schema}", icon::DATABASE));
            ui.label(RichText::new(tables.len().to_string()).weak().small());
            label
        },
        |ui| {
            for table in tables {
                table_node(ui, session, schema, table.as_ref(), actions);
            }
        },
    );
}

fn table_node(
    ui: &mut egui::Ui,
    session: &Session,
    schema: &str,
    table: &str,
    actions: &mut Vec<Action>,
) {
    let connection = &session.config.name;
    let id = ui.make_persistent_id(("table", connection, schema, table));
    // Body only runs when open; the qualified name is built lazily.
    let key = || qualified(schema, table, session.quotes);
    let n = node(
        ui,
        id,
        NodeOpts {
            default_open: false,
            force_open: false,
            can_stay_open: true,
            toggle_on_label: false,
        },
        |ui| clickable(ui, format!("{} {table}", icon::TABLE)),
        |ui| details_body(ui, session.details.get(&key())),
    );
    if n.open {
        let key = key();
        if !session.details.contains_key(&key) && !session.loading.contains_key(&key) {
            actions.push(Action::LoadDetails {
                connection: connection.clone(),
                table: key,
            });
        }
    }
    let open_data = || Action::OpenData {
        connection: connection.clone(),
        table: key(),
        title: table.to_string(),
    };
    if n.label.double_clicked() {
        actions.push(open_data());
    }
    n.label.context_menu(|ui| {
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
                query: format!("SELECT * FROM {}", key()),
            });
            ui.close_menu();
        }
        if ui.button(format!("{} DDL", icon::CODE)).clicked() {
            actions.push(Action::Ddl {
                connection: connection.clone(),
                table: key(),
                title: table.to_string(),
            });
            ui.close_menu();
        }
        if ui.button(format!("{} Structure…", icon::COLUMNS)).clicked() {
            actions.push(Action::Structure {
                connection: connection.clone(),
                table: key(),
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
                table: key(),
            });
            ui.close_menu();
        }
        if ui
            .button(RichText::new(format!("{} Vider la table…", icon::ERASER)).color(ERROR))
            .clicked()
        {
            actions.push(Action::Truncate {
                connection: connection.clone(),
                table: key(),
            });
            ui.close_menu();
        }
        ui.separator();
        if ui.button(format!("{} Copier le nom", icon::COPY)).clicked() {
            actions.push(Action::Copy(key()));
            ui.close_menu();
        }
    });
}

/// A (possibly multi-line) error on one small truncated line, full on hover.
fn error_line(ui: &mut egui::Ui, e: &str) {
    ui.add(Label::new(RichText::new(one_line(e)).color(ERROR).small()).truncate())
        .on_hover_text(e);
}

fn details_body(ui: &mut egui::Ui, details: Option<&Result<TableDetails, String>>) {
    let d = match details {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Chargement…").weak());
            });
            return;
        }
        Some(Err(e)) => {
            error_line(ui, e);
            return;
        }
        Some(Ok(d)) => d,
    };

    egui::CollapsingHeader::new(format!("{} Colonnes ({})", icon::COLUMNS, d.columns.len()))
        .id_salt("columns")
        .show(ui, |ui| {
            for c in &d.columns {
                ui.horizontal(|ui| {
                    if c.is_primary_key {
                        ui.label(RichText::new(icon::KEY).color(ACCENT))
                            .on_hover_text("Clé primaire");
                    }
                    ui.label(&c.name);
                    ui.label(RichText::new(&c.type_name).weak());
                    if !c.nullable {
                        ui.label(RichText::new("NOT NULL").weak().small());
                    }
                });
            }
        });

    let pk: Vec<&str> = d
        .columns
        .iter()
        .filter(|c| c.is_primary_key)
        .map(|c| c.name.as_str())
        .collect();
    let keys = usize::from(!pk.is_empty()) + d.foreign_keys.len();
    egui::CollapsingHeader::new(format!("{} Clés ({keys})", icon::KEY))
        .id_salt("keys")
        .show(ui, |ui| {
            if !pk.is_empty() {
                ui.label(format!("PK ({})", pk.join(", ")));
            }
            for fk in &d.foreign_keys {
                ui.label(format!(
                    "{} ({}) {} {}({})",
                    fk.name,
                    fk.columns.join(", "),
                    icon::ARROW_RIGHT,
                    fk.ref_table,
                    fk.ref_columns.join(", ")
                ));
            }
        });

    egui::CollapsingHeader::new(format!(
        "{} Index ({})",
        icon::LIST_BULLETS,
        d.indexes.len()
    ))
    .id_salt("indexes")
    .show(ui, |ui| {
        for idx in &d.indexes {
            ui.horizontal(|ui| {
                ui.label(&idx.name);
                ui.label(RichText::new(format!("({})", idx.columns.join(", "))).weak());
                if idx.unique {
                    ui.label(RichText::new("UNIQUE").weak().small());
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_is_case_insensitive_and_drops_empty_schemas() {
        let schemas = vec![
            SchemaInfo {
                name: "public".into(),
                tables: vec!["Users".into(), "orders".into()],
                expanded: true,
            },
            SchemaInfo {
                name: "audit".into(),
                tables: vec!["logs".into()],
                expanded: true,
            },
        ];
        let f = filter_tables(&schemas, "user");
        assert_eq!(f, vec![("public", vec!["Users"])]);
        assert_eq!(filter_tables(&schemas, "").len(), 2);
        assert!(filter_tables(&schemas, "nope").is_empty());
    }
}
