//! Following foreign keys: the linked-row preview of the value panel and
//! the data tabs filtered on a link's condition.

use crate::engine::db::utils::display_table_name;
use crate::engine::models::{Column, ForeignKeyInfo, IncomingForeignKey};
use crate::gui::fk::{self, LinkTarget, Preview, Resolver};
use crate::gui::grid::changes::RowRef;
use crate::gui::tabs::data::{DataAction, DataTab};
use crate::gui::tabs::{TabId, TabKind};
use crate::gui::value_panel::CellKey;

use super::App;

/// Where a followed link is shown.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkOpen {
    /// A new data tab filtered on the link.
    Tab,
    /// The value panel's preview: a new one started from cell `origin`
    /// (of a table titled `origin_title`), or the next step of the open
    /// one (`origin: None`).
    Preview {
        origin: Option<CellKey>,
        origin_title: String,
    },
}

/// Title of a tab opened on a link: `orders (filtre)`.
pub fn filtered_title(table: &str) -> String {
    format!("{} (filtre)", display_table_name(table))
}

impl App {
    /// Request the details of `table` (qualified) of `connection` unless
    /// they are loaded or loading.
    pub fn ensure_details(&mut self, connection: &str, table: &str) {
        let missing = self
            .sessions
            .get(connection)
            .is_some_and(|s| !s.details.contains_key(table) && !s.loading.contains_key(table));
        if missing {
            if let Some(s) = self.sessions.get_mut(connection) {
                s.load_details(&mut self.worker, connection, table);
            }
        }
    }

    /// Always a new tab (the user's own tab of that table keeps its
    /// filter), showing the rows `target` selects.
    pub fn open_filtered(&mut self, connection: &str, target: &LinkTarget) {
        let quotes = self
            .sessions
            .get(connection)
            .map_or(('"', '"'), |s| s.quotes);
        let tab = DataTab::filtered(target.table.clone(), quotes, &target.condition);
        let id = self.tabs.add(
            connection.to_string(),
            filtered_title(&target.table),
            TabKind::Data(Box::new(tab)),
        );
        self.data_action(id, DataAction::Load { count: true });
    }

    /// Follow outgoing key `fk` of a row of `source` (its `columns` and
    /// `values`), shown as `open` asks.
    pub fn follow_fk(
        &mut self,
        connection: &str,
        source: &str,
        fk: &ForeignKeyInfo,
        columns: &[Column],
        values: &[String],
        open: LinkOpen,
    ) {
        let Some(session) = self.sessions.get(connection) else {
            self.not_connected(connection);
            return;
        };
        let resolver = Resolver {
            schemas: &session.schemas,
            quotes: session.quotes,
            db: &session.config.db_type,
            source,
        };
        let Some(target) = resolver.follow(fk, columns, values) else {
            self.info("Clé NULL : aucune ligne liée");
            return;
        };
        match open {
            LinkOpen::Tab => self.open_filtered(connection, &target),
            LinkOpen::Preview {
                origin,
                origin_title,
            } => self.preview(connection, target, origin, origin_title),
        }
    }

    /// Open the rows of incoming key `fk` that point at a row of `source`.
    pub fn open_referencing(
        &mut self,
        connection: &str,
        source: &str,
        fk: &IncomingForeignKey,
        columns: &[Column],
        values: &[String],
    ) {
        let Some(session) = self.sessions.get(connection) else {
            self.not_connected(connection);
            return;
        };
        let resolver = Resolver {
            schemas: &session.schemas,
            quotes: session.quotes,
            db: &session.config.db_type,
            source,
        };
        match resolver.referencing(fk, columns, values) {
            Some(target) => self.open_filtered(connection, &target),
            None => self.info("Clé NULL : aucun enregistrement lié"),
        }
    }

    /// Load `target`'s row into the preview: a new preview from `origin`,
    /// else the next step of the open one.
    fn preview(
        &mut self,
        connection: &str,
        target: LinkTarget,
        origin: Option<CellKey>,
        origin_title: String,
    ) {
        let Some(session) = self.sessions.get(connection) else {
            self.not_connected(connection);
            return;
        };
        let sql = fk::preview_sql(&target, &session.config.db_type);
        let runner = session.runner();
        let run = self
            .worker
            .linked_row(connection.to_string(), runner, target.table.clone(), sql);
        let table = target.table.clone();
        match (&mut self.fk_preview, origin) {
            (Some(p), None) if p.connection == connection => p.push(target, run),
            _ => {
                self.fk_preview = Some(Preview::new(
                    connection.to_string(),
                    origin,
                    origin_title,
                    target,
                    run,
                ))
            }
        }
        self.show_value_panel = true;
        // Its own keys, to keep following them.
        self.ensure_details(connection, &table);
    }

    /// A data tab's cell link (`Follow`) or row menu (`Referencing`).
    pub(super) fn data_link(&mut self, id: TabId, row: RowRef, link: DataLink) {
        let Some(tab) = self.tabs.find(id) else {
            return;
        };
        let TabKind::Data(d) = &tab.kind else {
            return;
        };
        let (Some(result), Some(values)) = (&d.result, d.saved_values(row)) else {
            return;
        };
        let connection = tab.connection.clone();
        let source = d.table.clone();
        let columns = result.columns.clone();
        let details = self
            .sessions
            .get(&connection)
            .and_then(|s| s.details.get(&source))
            .and_then(|r| r.as_ref().ok());
        match link {
            DataLink::Follow { col, new_tab } => {
                let Some(fk) = columns
                    .get(col)
                    .and_then(|c| details.and_then(|d| fk::fk_of(&d.foreign_keys, &c.name)))
                    .and_then(|(i, _)| details?.foreign_keys.get(i).cloned())
                else {
                    return;
                };
                let open = if new_tab {
                    LinkOpen::Tab
                } else {
                    LinkOpen::Preview {
                        origin: Some((id, 0, row, col)),
                        origin_title: display_table_name(&source),
                    }
                };
                self.follow_fk(&connection, &source, &fk, &columns, &values, open);
            }
            DataLink::Referencing(i) => {
                let Some(fk) = details.and_then(|d| d.incoming.get(i)).cloned() else {
                    return;
                };
                self.open_referencing(&connection, &source, &fk, &columns, &values);
            }
        }
    }
}

/// What a data tab asked to follow from a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataLink {
    Follow { col: usize, new_tab: bool },
    Referencing(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_tabs_are_titled_after_their_table() {
        assert_eq!(filtered_title("\"main\".\"orders\""), "orders (filtre)");
        assert_eq!(filtered_title("[dbo].[a.b]"), "a.b (filtre)");
    }
}
