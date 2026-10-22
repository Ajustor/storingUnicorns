//! Foreign key navigation: which columns link where (and how they are
//! marked), where a link leads for a row, the linked-row preview of the
//! value panel with its breadcrumb, and the SQL that loads that row.

use egui::{Color32, RichText};
use egui_phosphor::regular as icon;

use crate::engine::db::utils::{display_qualified, split_qualified};
use crate::engine::models::{
    Column, DatabaseType, ForeignKeyInfo, IncomingForeignKey, QueryResult, SchemaInfo,
};
use crate::engine::ops::transfer::qualified;
use crate::engine::sql::completion::TableRef;
use crate::engine::sql::paging::{build_select, DataQuery};
use crate::engine::sql::relations;
use crate::engine::sql::statements::quote_ident;

use super::sessions::details_key;
use super::theme::ACCENT;
use super::value_panel::CellKey;
use super::worker::RunId;

/// Rows the preview asks for: one is expected, a second one tells that
/// the key matches several rows.
pub const PREVIEW_ROWS: usize = 2;

/// The foreign key `column` is part of (the first one declared, exact name
/// first, then ignoring case) and the column's position in it.
pub fn fk_of(fks: &[ForeignKeyInfo], column: &str) -> Option<(usize, usize)> {
    let find = |eq: &dyn Fn(&str) -> bool| {
        fks.iter()
            .enumerate()
            .find_map(|(i, fk)| fk.columns.iter().position(|c| eq(c)).map(|p| (i, p)))
    };
    find(&|c| c == column).or_else(|| find(&|c| c.eq_ignore_ascii_case(column)))
}

/// Where a foreign key column points, as shown next to it.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnLink {
    /// Index of the key in the table's `foreign_keys`.
    pub fk: usize,
    /// `table.column` (`table` alone when the referenced column is
    /// implicit, a SQLite key on the primary key).
    pub target: String,
    /// The referenced table, unquoted (`schema.table` when qualified).
    pub table: String,
    /// The key has several columns.
    pub composite: bool,
}

impl ColumnLink {
    /// "Clé étrangère → users.id".
    pub fn tooltip(&self) -> String {
        let mut text = format!("Clé étrangère → {}", self.target);
        if self.composite {
            text.push_str(" (clé composée)");
        }
        text
    }
}

/// The link of `column` among `fks`, if it is a foreign key column.
pub fn link_of(fks: &[ForeignKeyInfo], column: &str) -> Option<ColumnLink> {
    let (i, pos) = fk_of(fks, column)?;
    let fk = &fks[i];
    let table = display_qualified(&fk.ref_table);
    let target = match fk.ref_columns.get(pos).filter(|c| !c.is_empty()) {
        Some(c) => format!("{table}.{c}"),
        None => table.clone(),
    };
    Some(ColumnLink {
        fk: i,
        target,
        table,
        composite: fk.columns.len() > 1,
    })
}

/// The link of each of `columns` (`None`: not a foreign key column).
pub fn column_links(columns: &[Column], fks: &[ForeignKeyInfo]) -> Vec<Option<ColumnLink>> {
    columns.iter().map(|c| link_of(fks, &c.name)).collect()
}

/// `orders (user_id)`: an incoming key, as listed in menus.
pub fn incoming_label(fk: &IncomingForeignKey) -> String {
    format!(
        "{} ({})",
        display_qualified(&fk.table),
        fk.columns.join(", ")
    )
}

/// The click that opens a link in a new tab (`modifiers.command`): Cmd on
/// macOS, Ctrl elsewhere.
pub fn tab_click(mac: bool) -> &'static str {
    if mac {
        "Cmd+clic"
    } else {
        "Ctrl+clic"
    }
}

/// [`tab_click`] for this platform.
pub fn tab_click_here() -> &'static str {
    tab_click(cfg!(target_os = "macos"))
}

/// What a grid cell of a foreign key column draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellLink {
    /// Not a foreign key column.
    None,
    /// A foreign key column whose value links nowhere (NULL, a row not
    /// saved yet, or a key edited but not submitted).
    Inert,
    /// Clickable: the value is shown as a link, with its icon.
    Active,
}

/// See [`CellLink`]. `saved`: the row exists in the database; `edited`:
/// the cell has a pending edit (links follow what the database holds).
pub fn cell_link(link: Option<&ColumnLink>, null: bool, saved: bool, edited: bool) -> CellLink {
    match link {
        None => CellLink::None,
        Some(_) if null || !saved || edited => CellLink::Inert,
        Some(_) => CellLink::Active,
    }
}

/// Markers before a column name in a header: key icons, then a badge.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderMarks {
    pub pk: bool,
    pub fk: Option<String>,
}

/// The markers and hover text of a grid header.
pub fn header_marks(column: &Column, link: Option<&ColumnLink>) -> (HeaderMarks, String) {
    let mut hover = column.type_name.clone();
    if column.is_primary_key {
        hover.push_str(" · Clé primaire");
    }
    if let Some(l) = link {
        hover.push_str(" · ");
        hover.push_str(&l.tooltip());
    }
    (
        HeaderMarks {
            pk: column.is_primary_key,
            fk: link.map(|l| l.target.clone()),
        },
        hover,
    )
}

/// Foreground of FK markers and links: the accent, lightened on dark
/// backgrounds so it stays readable.
pub fn link_color(dark: bool) -> Color32 {
    if dark {
        ACCENT.lerp_to_gamma(Color32::WHITE, 0.25)
    } else {
        ACCENT.lerp_to_gamma(Color32::BLACK, 0.15)
    }
}

/// A small "FK" badge (accent frame), with `hover` on hover.
pub fn fk_badge(ui: &mut egui::Ui, hover: &str) -> egui::Response {
    badge(ui, "FK", hover)
}

/// A small "PK" badge, like [`fk_badge`].
pub fn pk_badge(ui: &mut egui::Ui) -> egui::Response {
    badge(ui, "PK", "Clé primaire")
}

fn badge(ui: &mut egui::Ui, text: &str, hover: &str) -> egui::Response {
    let color = link_color(ui.visuals().dark_mode);
    let icon = if text == "PK" { icon::KEY } else { icon::LINK };
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(3, 0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.label(
                RichText::new(format!("{icon} {text}"))
                    .small()
                    .strong()
                    .color(color),
            );
        })
        .response
        .on_hover_text(hover)
}

/// The value of column `name` in a row of `columns` (exact name first,
/// then ignoring case).
pub fn row_value(columns: &[Column], values: &[String], name: &str) -> Option<String> {
    let at = columns.iter().position(|c| c.name == name).or_else(|| {
        columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    })?;
    values.get(at).cloned()
}

/// Key in the session's details (qualified, quoted) of `table`, a name
/// given by the engine (`schema.table`, final): the schema listing it,
/// matched like completion does. A bare name falls back to `source`'s
/// schema, then to the first schema holding it; quoted as given when no
/// schema lists it.
pub fn target_key(
    schemas: &[SchemaInfo],
    quotes: (char, char),
    source: &str,
    table: &str,
) -> String {
    let (schema, name) = split_qualified(table);
    let preferred = schema.clone().or_else(|| split_qualified(source).0);
    let t = |schema| TableRef {
        schema,
        name: name.clone(),
        alias: None,
    };
    details_key(schemas, &t(preferred), quotes)
        .or_else(|| details_key(schemas, &t(None), quotes))
        .unwrap_or_else(|| match &schema {
            Some(s) => qualified(s, &name, quotes),
            None => quote_ident(&name, quotes),
        })
}

/// Rows a link selects: a table and a condition.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkTarget {
    /// Qualified, quoted table (key of the session's details).
    pub table: String,
    /// Unquoted name, for titles.
    pub title: String,
    /// `WHERE` condition, without the keyword.
    pub condition: String,
}

/// What a row of the session's tables needs to resolve links.
pub struct Resolver<'a> {
    pub schemas: &'a [SchemaInfo],
    pub quotes: (char, char),
    pub db: &'a DatabaseType,
    /// Key of the table the row belongs to.
    pub source: &'a str,
}

impl Resolver<'_> {
    fn target(&self, table: &str, condition: String) -> LinkTarget {
        LinkTarget {
            table: target_key(self.schemas, self.quotes, self.source, table),
            title: display_qualified(table),
            condition,
        }
    }

    /// The row `fk` (of the source table) points at; `None` for a NULL key.
    pub fn follow(
        &self,
        fk: &ForeignKeyInfo,
        columns: &[Column],
        values: &[String],
    ) -> Option<LinkTarget> {
        let row = |name: &str| row_value(columns, values, name);
        let (table, condition) = relations::follow(fk, &row, self.db)?;
        Some(self.target(&table, condition))
    }

    /// The rows of another table pointing at this row through `fk`.
    pub fn referencing(
        &self,
        fk: &IncomingForeignKey,
        columns: &[Column],
        values: &[String],
    ) -> Option<LinkTarget> {
        let row = |name: &str| row_value(columns, values, name);
        let (table, condition) = relations::referencing(fk, &row, self.db)?;
        Some(self.target(&table, condition))
    }
}

/// `SELECT *` of the rows `target` selects, at most [`PREVIEW_ROWS`].
pub fn preview_sql(target: &LinkTarget, db: &DatabaseType) -> String {
    build_select(
        &DataQuery {
            table: target.table.clone(),
            filter: target.condition.clone(),
            order_by: String::new(),
            page: 0,
            page_size: PREVIEW_ROWS,
        },
        db,
    )
}

/// The linked row, as loaded.
#[derive(Debug, Clone)]
pub enum RowState {
    Loading,
    /// No row matches: the key is orphaned.
    Missing,
    /// The first matching row (`result.rows[0]`); `many` when another one
    /// matched too.
    Found {
        result: QueryResult,
        many: bool,
    },
    Failed(String),
}

impl RowState {
    pub fn from_outcome(outcome: Result<QueryResult, String>) -> Self {
        match outcome {
            Err(e) => RowState::Failed(e),
            Ok(r) if r.rows.is_empty() => RowState::Missing,
            Ok(r) => RowState::Found {
                many: r.rows.len() > 1,
                result: r,
            },
        }
    }

    /// The message shown above (or instead of) the row.
    pub fn notice(&self) -> Option<&str> {
        match self {
            RowState::Loading => Some("Chargement…"),
            RowState::Missing => Some("Aucune ligne liée (clé orpheline)"),
            RowState::Found { many: true, .. } => {
                Some("Plusieurs lignes correspondent : la première est affichée")
            }
            RowState::Found { .. } => None,
            RowState::Failed(e) => Some(e),
        }
    }
}

/// One linked row of the breadcrumb.
#[derive(Debug, Clone)]
pub struct Step {
    pub target: LinkTarget,
    /// Load request awaited for this step.
    pub run: RunId,
    pub state: RowState,
}

/// The linked-row preview of the value panel: the rows followed from a
/// cell, as a breadcrumb that can go back and forward.
#[derive(Debug, Clone)]
pub struct Preview {
    pub connection: String,
    /// The grid cell the navigation started from: focusing another cell
    /// closes the preview.
    pub origin: Option<CellKey>,
    /// First crumb: the table of that cell.
    pub origin_title: String,
    steps: Vec<Step>,
    pos: usize,
}

impl Preview {
    pub fn new(
        connection: String,
        origin: Option<CellKey>,
        origin_title: String,
        target: LinkTarget,
        run: RunId,
    ) -> Self {
        Self {
            connection,
            origin,
            origin_title,
            steps: vec![Step {
                target,
                run,
                state: RowState::Loading,
            }],
            pos: 0,
        }
    }

    pub fn current(&self) -> &Step {
        &self.steps[self.pos]
    }

    /// Follow a link of the current row: the steps after it are dropped.
    pub fn push(&mut self, target: LinkTarget, run: RunId) {
        self.steps.truncate(self.pos + 1);
        self.steps.push(Step {
            target,
            run,
            state: RowState::Loading,
        });
        self.pos = self.steps.len() - 1;
    }

    /// One step back; false on the first step (going back closes the
    /// preview).
    pub fn back(&mut self) -> bool {
        if self.pos == 0 {
            return false;
        }
        self.pos -= 1;
        true
    }

    pub fn can_forward(&self) -> bool {
        self.pos + 1 < self.steps.len()
    }

    pub fn forward(&mut self) {
        if self.can_forward() {
            self.pos += 1;
        }
    }

    /// Jump to step `i` of the breadcrumb (steps after it are kept).
    pub fn go(&mut self, i: usize) {
        if i < self.steps.len() {
            self.pos = i;
        }
    }

    /// Store the answer to request `run`; false when no step awaits it.
    pub fn on_loaded(&mut self, run: RunId, outcome: Result<QueryResult, String>) -> bool {
        match self
            .steps
            .iter_mut()
            .find(|s| s.run == run && matches!(s.state, RowState::Loading))
        {
            Some(step) => {
                step.state = RowState::from_outcome(outcome);
                true
            }
            None => false,
        }
    }

    /// Titles of the steps, and whether each is the current one.
    pub fn crumbs(&self) -> Vec<(&str, bool)> {
        self.steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.target.title.as_str(), i == self.pos))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::grid::changes::RowRef;

    fn fk(columns: &[&str], table: &str, refs: &[&str]) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: "fk".into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            ref_table: table.into(),
            ref_columns: refs.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn column(name: &str, pk: bool) -> Column {
        Column {
            name: name.into(),
            type_name: "INTEGER".into(),
            nullable: true,
            is_primary_key: pk,
        }
    }

    fn result(rows: usize) -> QueryResult {
        QueryResult {
            columns: vec![column("id", true)],
            rows: (0..rows).map(|i| vec![i.to_string()]).collect(),
            ..Default::default()
        }
    }

    fn target(title: &str) -> LinkTarget {
        LinkTarget {
            table: format!("\"main\".\"{title}\""),
            title: title.into(),
            condition: "\"id\" = 1".into(),
        }
    }

    #[test]
    fn columns_are_matched_to_their_key() {
        let fks = [
            fk(&["user_id"], "users", &["id"]),
            fk(&["a", "b"], "public.pairs", &["x", "y"]),
            fk(&["owner"], "people", &[""]),
        ];
        assert_eq!(fk_of(&fks, "user_id"), Some((0, 0)));
        assert_eq!(fk_of(&fks, "USER_ID"), Some((0, 0)), "ignoring case");
        assert_eq!(fk_of(&fks, "b"), Some((1, 1)));
        assert_eq!(fk_of(&fks, "id"), None);
        let l = link_of(&fks, "b").unwrap();
        assert_eq!((l.target.as_str(), l.composite), ("public.pairs.y", true));
        assert_eq!(l.tooltip(), "Clé étrangère → public.pairs.y (clé composée)");
        let l = link_of(&fks, "user_id").unwrap();
        assert_eq!(l.tooltip(), "Clé étrangère → users.id");
        assert_eq!(link_of(&fks, "owner").unwrap().target, "people");
        let cols = [column("id", true), column("user_id", false)];
        let links = column_links(&cols, &fks);
        assert!(links[0].is_none());
        assert_eq!(links[1].as_ref().map(|l| l.fk), Some(0));
    }

    #[test]
    fn headers_mark_primary_and_foreign_keys() {
        let fks = [fk(&["user_id"], "users", &["id"])];
        let link = link_of(&fks, "user_id");
        let (marks, hover) = header_marks(&column("user_id", false), link.as_ref());
        assert_eq!(
            marks,
            HeaderMarks {
                pk: false,
                fk: Some("users.id".into())
            }
        );
        assert_eq!(hover, "INTEGER · Clé étrangère → users.id");
        let (marks, hover) = header_marks(&column("id", true), None);
        assert_eq!(marks, HeaderMarks { pk: true, fk: None });
        assert_eq!(hover, "INTEGER · Clé primaire");
        let (marks, _) = header_marks(&column("name", false), None);
        assert_eq!(
            marks,
            HeaderMarks {
                pk: false,
                fk: None
            }
        );
    }

    #[test]
    fn the_new_tab_click_is_named_for_the_platform() {
        assert_eq!(tab_click(true), "Cmd+clic");
        assert_eq!(tab_click(false), "Ctrl+clic");
    }

    #[test]
    fn only_saved_non_null_fk_cells_are_links() {
        let fks = [fk(&["user_id"], "users", &["id"])];
        let link = link_of(&fks, "user_id");
        assert_eq!(
            cell_link(link.as_ref(), false, true, false),
            CellLink::Active
        );
        assert_eq!(cell_link(link.as_ref(), true, true, false), CellLink::Inert);
        assert_eq!(
            cell_link(link.as_ref(), false, false, false),
            CellLink::Inert
        );
        assert_eq!(
            cell_link(link.as_ref(), false, true, true),
            CellLink::Inert,
            "an edited key links nowhere until submitted"
        );
        assert_eq!(cell_link(None, false, true, false), CellLink::None);
    }

    #[test]
    fn targets_resolve_in_the_source_schema_first() {
        let schemas = vec![
            SchemaInfo {
                name: "public".into(),
                tables: vec!["users".into()],
                expanded: false,
            },
            SchemaInfo {
                name: "sales".into(),
                tables: vec!["users".into(), "orders".into()],
                expanded: false,
            },
        ];
        let q = ('"', '"');
        let src = "\"sales\".\"orders\"";
        assert_eq!(target_key(&schemas, q, src, "users"), "\"sales\".\"users\"");
        assert_eq!(
            target_key(&schemas, q, src, "public.users"),
            "\"public\".\"users\""
        );
        assert_eq!(
            target_key(&schemas, q, "\"other\".\"t\"", "users"),
            "\"public\".\"users\"",
            "first schema holding it"
        );
        assert_eq!(target_key(&schemas, q, src, "ghost"), "\"ghost\"");
    }

    #[test]
    fn following_a_key_builds_the_target_and_its_sql() {
        let schemas = vec![SchemaInfo {
            name: "main".into(),
            tables: vec!["users".into(), "orders".into()],
            expanded: false,
        }];
        let r = Resolver {
            schemas: &schemas,
            quotes: ('"', '"'),
            db: &DatabaseType::SQLite,
            source: "\"main\".\"orders\"",
        };
        let key = fk(&["user_id"], "users", &["id"]);
        let cols = [column("id", true), column("user_id", false)];
        let t = r.follow(&key, &cols, &["7".into(), "3".into()]).unwrap();
        assert_eq!(t.table, "\"main\".\"users\"");
        assert_eq!(t.title, "users");
        // The engine's condition, as is.
        assert_eq!(
            relations::follow(
                &key,
                &|n| row_value(&cols, &["7".into(), "3".into()], n),
                &DatabaseType::SQLite
            )
            .map(|(_, c)| c),
            Some(t.condition.clone())
        );
        let sql = preview_sql(&t, &DatabaseType::SQLite);
        assert_eq!(
            sql,
            format!(
                "SELECT * FROM \"main\".\"users\" WHERE ({}) LIMIT 2 OFFSET 0",
                t.condition
            )
        );
        let null = crate::engine::models::NULL_CELL.to_string();
        assert!(r.follow(&key, &cols, &["7".into(), null]).is_none());

        let incoming = IncomingForeignKey {
            name: "fk".into(),
            table: "main.orders".into(),
            columns: vec!["user_id".into()],
            ref_columns: vec!["id".into()],
        };
        assert_eq!(incoming_label(&incoming), "main.orders (user_id)");
        let users = Resolver {
            source: "\"main\".\"users\"",
            ..r
        };
        let t = users
            .referencing(&incoming, &cols[..1], &["3".into()])
            .unwrap();
        assert_eq!(t.table, "\"main\".\"orders\"");
        assert!(t.condition.contains("user_id"));
    }

    #[test]
    fn row_states_tell_orphans_and_duplicates() {
        assert!(matches!(
            RowState::from_outcome(Ok(result(0))),
            RowState::Missing
        ));
        let one = RowState::from_outcome(Ok(result(1)));
        assert!(matches!(one, RowState::Found { many: false, .. }));
        assert_eq!(one.notice(), None);
        let many = RowState::from_outcome(Ok(result(2)));
        assert!(matches!(&many, RowState::Found { many: true, result } if result.rows.len() == 2));
        assert_eq!(
            many.notice(),
            Some("Plusieurs lignes correspondent : la première est affichée")
        );
        assert_eq!(
            RowState::from_outcome(Ok(result(0))).notice(),
            Some("Aucune ligne liée (clé orpheline)")
        );
        let failed = RowState::from_outcome(Err("boom".into()));
        assert_eq!(failed.notice(), Some("boom"));
    }

    #[test]
    fn breadcrumb_goes_back_and_forward() {
        let origin = (1, 0, RowRef::Base(0), 1);
        let mut p = Preview::new(
            "c".into(),
            Some(origin),
            "orders".into(),
            target("users"),
            10,
        );
        assert_eq!(p.crumbs(), vec![("users", true)]);
        assert!(!p.back(), "the first step closes the preview");
        p.push(target("teams"), 11);
        p.push(target("sites"), 12);
        assert_eq!(
            p.crumbs(),
            vec![("users", false), ("teams", false), ("sites", true)]
        );
        assert!(p.back());
        assert_eq!(p.current().target.title, "teams");
        assert!(p.can_forward());
        p.forward();
        assert_eq!(p.current().target.title, "sites");
        assert!(!p.can_forward());
        p.forward();
        assert_eq!(p.current().target.title, "sites", "nothing further");
        p.go(0);
        assert_eq!(p.current().target.title, "users");
        assert!(p.can_forward(), "jumping back keeps the later steps");
        p.push(target("roles"), 13);
        assert_eq!(p.crumbs(), vec![("users", false), ("roles", true)]);
        p.go(9);
        assert_eq!(p.current().target.title, "roles");
    }

    #[test]
    fn answers_go_to_the_step_awaiting_them() {
        let mut p = Preview::new("c".into(), None, "orders".into(), target("users"), 10);
        p.push(target("teams"), 11);
        assert!(p.on_loaded(10, Ok(result(1))), "an earlier step");
        assert!(!p.on_loaded(10, Ok(result(1))), "already answered");
        assert!(!p.on_loaded(99, Ok(result(1))), "not awaited");
        assert!(matches!(p.current().state, RowState::Loading));
        assert!(p.on_loaded(11, Ok(result(0))));
        assert!(matches!(p.current().state, RowState::Missing));
        p.back();
        assert!(matches!(
            p.current().state,
            RowState::Found { many: false, .. }
        ));
    }
}
