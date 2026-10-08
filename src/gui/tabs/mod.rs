//! Centre tabs: consoles, table data editors and DDL views, each bound to a
//! connection.

pub mod console;
pub mod data;
pub mod ddl;

use crate::engine::services::query_tabs::{QueryTab, QueryTabsState};

use super::worker::{Event, RunId};

pub type TabId = super::worker::TabId;

pub enum TabKind {
    Console(console::ConsoleTab),
    Data(data::DataTab),
    Ddl(ddl::DdlTab),
}

impl TabKind {
    pub fn icon(&self) -> &'static str {
        use egui_phosphor::regular as icon;
        match self {
            Self::Console(_) => icon::TERMINAL_WINDOW,
            Self::Data(_) => icon::TABLE,
            Self::Ddl(_) => icon::CODE,
        }
    }
}

/// Run ids a tab is waiting for, one slot per kind of operation. Events of
/// any other run (replaced or cancelled) are stale and dropped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Runs {
    pub script: Option<RunId>,
    pub page: Option<RunId>,
    pub count: Option<RunId>,
    pub submit: Option<RunId>,
    pub ddl: Option<RunId>,
}

impl Runs {
    /// Whether `ev` is an outcome this tab waits for; frees its slot if so.
    /// Events that are not tab outcomes are never accepted.
    pub fn accept(&mut self, ev: &Event) -> bool {
        let (slot, run) = match ev {
            Event::Script { run, .. } => (&mut self.script, *run),
            Event::Page { run, .. } => (&mut self.page, *run),
            Event::Count { run, .. } => (&mut self.count, *run),
            Event::Submitted { run, .. } => (&mut self.submit, *run),
            Event::Ddl { run, .. } => (&mut self.ddl, *run),
            _ => return false,
        };
        if *slot == Some(run) {
            *slot = None;
            true
        } else {
            false
        }
    }
}

pub struct Tab {
    pub id: TabId,
    pub title: String,
    /// Connection name this tab is bound to.
    pub connection: String,
    pub kind: TabKind,
    pub runs: Runs,
    /// Last-execution summary shown in the status bar ("42 lignes · 12 ms").
    pub summary: Option<String>,
}

impl Tab {
    /// Hand an outcome of this tab to its body. Stale runs are dropped.
    pub fn on_event(&mut self, ev: Event) {
        if !self.runs.accept(&ev) {
            return;
        }
        match &mut self.kind {
            TabKind::Console(c) => c.on_event(ev),
            TabKind::Data(d) => d.on_event(ev),
            TabKind::Ddl(d) => d.on_event(ev),
        }
    }
}

pub struct Tabs {
    pub list: Vec<Tab>,
    pub active: usize,
    next_id: TabId,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            active: 0,
            next_id: 1,
        }
    }
}

impl Tabs {
    /// Add a tab after the others and activate it.
    pub fn add(&mut self, connection: String, title: String, kind: TabKind) -> TabId {
        let id = self.next_id;
        self.next_id += 1;
        self.list.push(Tab {
            id,
            title,
            connection,
            kind,
            runs: Runs::default(),
            summary: None,
        });
        self.active = self.list.len() - 1;
        id
    }

    /// Remove the tab at `index`; the active tab stays the same one when it
    /// survives, else its right neighbour (or the new last tab) is activated.
    pub fn close(&mut self, index: usize) -> Option<Tab> {
        if index >= self.list.len() {
            return None;
        }
        let tab = self.list.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.list.len().saturating_sub(1));
        Some(tab)
    }

    pub fn find(&mut self, id: TabId) -> Option<&mut Tab> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    pub fn active(&self) -> Option<&Tab> {
        self.list.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        self.list.get_mut(self.active)
    }

    /// Data tab already open for (connection, table)? → its index.
    pub fn find_data(&self, connection: &str, table: &str) -> Option<usize> {
        self.list.iter().position(|t| {
            t.connection == connection && matches!(&t.kind, TabKind::Data(d) if d.table == table)
        })
    }

    /// Number of console tabs (for default titles).
    pub fn console_count(&self) -> usize {
        self.list
            .iter()
            .filter(|t| matches!(t.kind, TabKind::Console(_)))
            .count()
    }
}

/// Remove from `pending` and return the persisted consoles to restore when
/// connection `name` opens: those bound to it, plus the unbound ones (written
/// by the TUI) when `with_unbound`.
pub fn take_consoles(pending: &mut Vec<QueryTab>, name: &str, with_unbound: bool) -> Vec<QueryTab> {
    let (taken, kept) = std::mem::take(pending)
        .into_iter()
        .partition(|q| match &q.connection {
            Some(c) => c == name,
            None => with_unbound,
        });
    *pending = kept;
    taken
}

/// Consoles to write to `queries.toml`: the open ones, then those not
/// restored yet (their connection wasn't opened this session).
pub fn persisted_consoles(tabs: &Tabs, pending: &[QueryTab]) -> QueryTabsState {
    let open = tabs.list.iter().filter_map(|t| match &t.kind {
        TabKind::Console(c) => Some(QueryTab {
            name: t.title.clone(),
            query: c.query.clone(),
            cursor_position: c.cursor,
            is_modified: false,
            connection: Some(t.connection.clone()),
        }),
        _ => None,
    });
    QueryTabsState {
        tabs: open.chain(pending.iter().cloned()).collect(),
        active_tab: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console() -> TabKind {
        TabKind::Console(console::ConsoleTab::new(String::new(), 0))
    }

    fn data(table: &str) -> TabKind {
        TabKind::Data(data::DataTab::new(table.into()))
    }

    #[test]
    fn add_activates_and_ids_are_unique() {
        let mut tabs = Tabs::default();
        let a = tabs.add("c".into(), "A".into(), console());
        let b = tabs.add("c".into(), "B".into(), console());
        assert_ne!(a, b);
        assert_eq!(tabs.active, 1);
        assert_eq!(tabs.find(a).unwrap().title, "A");
        assert!(tabs.find(99).is_none());
    }

    #[test]
    fn close_keeps_the_active_index_valid() {
        let mut tabs = Tabs::default();
        for t in ["A", "B", "C", "D"] {
            tabs.add("c".into(), t.into(), console());
        }
        tabs.active = 2; // C
        tabs.close(0); // before the active one: C stays active
        assert_eq!(tabs.active().unwrap().title, "C");
        tabs.close(1); // the active one: its right neighbour D takes over
        assert_eq!(tabs.active().unwrap().title, "D");
        tabs.close(1); // the active last one: the new last tab
        assert_eq!(tabs.active().unwrap().title, "B");
        assert!(tabs.close(5).is_none());
        tabs.close(0);
        assert!(tabs.active().is_none());
        assert_eq!(tabs.active, 0);
        assert!(tabs.close(0).is_none());
    }

    #[test]
    fn find_data_matches_connection_and_table() {
        let mut tabs = Tabs::default();
        tabs.add("prod".into(), "Console".into(), console());
        tabs.add("prod".into(), "users".into(), data("\"main\".\"users\""));
        tabs.add("dev".into(), "users".into(), data("\"main\".\"users\""));
        assert_eq!(tabs.find_data("prod", "\"main\".\"users\""), Some(1));
        assert_eq!(tabs.find_data("dev", "\"main\".\"users\""), Some(2));
        assert_eq!(tabs.find_data("dev", "\"main\".\"orders\""), None);
        assert_eq!(tabs.console_count(), 1);
    }

    #[test]
    fn runs_accept_only_the_awaited_run() {
        let mut runs = Runs {
            page: Some(2),
            count: Some(3),
            ..Default::default()
        };
        let page = |run| Event::Page {
            tab: 1,
            run,
            outcome: Err("x".into()),
        };
        assert!(!runs.accept(&page(1)), "superseded page load");
        assert!(!runs.accept(&page(3)), "a count id is not a page id");
        assert!(runs.accept(&page(2)));
        assert!(!runs.accept(&page(2)), "accepted once");
        assert_eq!(runs.page, None);
        assert_eq!(runs.count, Some(3));
        assert!(!runs.accept(&Event::TestFinished(Ok(()))));
    }

    fn saved(name: &str, connection: Option<&str>) -> QueryTab {
        QueryTab {
            connection: connection.map(str::to_string),
            ..QueryTab::new(name.into())
        }
    }

    #[test]
    fn take_consoles_by_connection_and_unbound_once() {
        let mut pending = vec![
            saved("a", Some("prod")),
            saved("b", None),
            saved("c", Some("dev")),
        ];
        let names = |v: Vec<QueryTab>| v.into_iter().map(|q| q.name).collect::<Vec<_>>();
        assert_eq!(names(take_consoles(&mut pending, "dev", true)), ["b", "c"]);
        assert_eq!(
            names(take_consoles(&mut pending, "other", false)),
            Vec::<String>::new()
        );
        assert_eq!(names(take_consoles(&mut pending, "prod", false)), ["a"]);
        assert!(pending.is_empty());
    }

    #[test]
    fn persisted_consoles_keep_open_then_pending() {
        let mut tabs = Tabs::default();
        tabs.add(
            "prod".into(),
            "Console 1".into(),
            TabKind::Console(console::ConsoleTab::new("SELECT 1".into(), 3)),
        );
        tabs.add("prod".into(), "users".into(), data("users"));
        let state = persisted_consoles(&tabs, &[saved("old", Some("dev"))]);
        assert_eq!(state.tabs.len(), 2);
        let q = &state.tabs[0];
        assert_eq!(
            (q.name.as_str(), q.query.as_str(), q.cursor_position),
            ("Console 1", "SELECT 1", 3)
        );
        assert_eq!(q.connection.as_deref(), Some("prod"));
        assert_eq!(state.tabs[1].name, "old");
    }
}
