//! Connection folders shown as a tree in the Connections panel, and the
//! schemas hidden from the Tables panel and the completion.

use crate::engine::config::folders::{self, FolderNode};

use super::{AppState, DialogMode};

/// One visible line of the Connections panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnRow {
    Folder {
        path: String,
        name: String,
        depth: usize,
        collapsed: bool,
        /// Connections anywhere below the folder.
        count: usize,
    },
    Connection {
        index: usize,
        depth: usize,
    },
}

impl ConnRow {
    pub fn depth(&self) -> usize {
        match self {
            ConnRow::Folder { depth, .. } | ConnRow::Connection { depth, .. } => *depth,
        }
    }
}

/// What the folder prompt does with the path typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderPromptKind {
    /// Move the named connection; an empty path moves it to the root.
    MoveConnection(String),
    NewFolder,
    /// Rename (or move) the folder at this path.
    RenameFolder(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPrompt {
    pub kind: FolderPromptKind,
    pub input: String,
    /// Byte offset in `input`.
    pub cursor: usize,
}

impl FolderPrompt {
    fn new(kind: FolderPromptKind, input: String) -> Self {
        let cursor = input.len();
        Self {
            kind,
            input,
            cursor,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            FolderPromptKind::MoveConnection(_) => " Move to folder ",
            FolderPromptKind::NewFolder => " New folder ",
            FolderPromptKind::RenameFolder(_) => " Rename folder ",
        }
    }
}

fn count_connections(node: &FolderNode) -> usize {
    node.connections.len() + node.folders.iter().map(count_connections).sum::<usize>()
}

/// `a/b/c` -> `["a", "a/b", "a/b/c"]`.
fn with_ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for part in path.split(folders::SEP) {
        if !acc.is_empty() {
            acc.push(folders::SEP);
        }
        acc.push_str(part);
        out.push(acc.clone());
    }
    out
}

/// `path` itself, or one of its subfolders.
fn is_within(path: &str, folder: &str) -> bool {
    path == folder
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with(folders::SEP))
}

impl AppState {
    /// The visible lines of the Connections panel: folders first, then
    /// connections, the content of collapsed folders left out.
    pub fn connection_rows(&self) -> Vec<ConnRow> {
        fn walk(state: &AppState, node: &FolderNode, depth: usize, rows: &mut Vec<ConnRow>) {
            for folder in &node.folders {
                let collapsed = state.collapsed_folders.contains(&folder.path);
                rows.push(ConnRow::Folder {
                    path: folder.path.clone(),
                    name: folder.name.clone(),
                    depth,
                    collapsed,
                    count: count_connections(folder),
                });
                if !collapsed {
                    walk(state, folder, depth + 1, rows);
                }
            }
            for &index in &node.connections {
                rows.push(ConnRow::Connection { index, depth });
            }
        }
        let mut rows = Vec::new();
        walk(self, &folders::tree(&self.config), 0, &mut rows);
        rows
    }

    /// Position of the cursor in `rows` (0 when it is not visible).
    pub fn connection_cursor(&self, rows: &[ConnRow]) -> usize {
        rows.iter()
            .position(|row| match (row, &self.selected_folder) {
                (ConnRow::Folder { path, .. }, Some(sel)) => path == sel,
                (ConnRow::Connection { index, .. }, None) => *index == self.selected_connection,
                _ => false,
            })
            .unwrap_or(0)
    }

    /// Put the cursor on `row`.
    pub fn select_connection_row(&mut self, row: &ConnRow) {
        match row {
            ConnRow::Folder { path, .. } => self.selected_folder = Some(path.clone()),
            ConnRow::Connection { index, .. } => {
                self.selected_folder = None;
                self.selected_connection = *index;
            }
        }
    }

    /// Move the cursor by one visible line, wrapping around.
    pub fn move_connection_cursor(&mut self, forward: bool) {
        let rows = self.connection_rows();
        if rows.is_empty() {
            return;
        }
        let pos = self.connection_cursor(&rows);
        let next = if forward {
            (pos + 1) % rows.len()
        } else {
            pos.checked_sub(1).unwrap_or(rows.len() - 1)
        };
        self.select_connection_row(&rows[next]);
    }

    /// The folder under the cursor, or the folder of the connection under it.
    pub fn cursor_folder(&self) -> Option<String> {
        match &self.selected_folder {
            Some(path) => Some(path.clone()),
            None => self
                .config
                .connections
                .get(self.selected_connection)
                .and_then(|c| c.folder.clone()),
        }
    }

    /// Fold or unfold the folder under the cursor.
    pub fn toggle_selected_folder(&mut self) {
        if let Some(path) = self.selected_folder.clone() {
            if !self.collapsed_folders.remove(&path) {
                self.collapsed_folders.insert(path);
            }
        }
    }

    /// ←: fold the folder under the cursor, else go to the parent folder.
    pub fn connections_left(&mut self) {
        if let Some(path) = &self.selected_folder {
            if !self.collapsed_folders.contains(path) {
                self.collapsed_folders.insert(path.clone());
                return;
            }
        }
        let rows = self.connection_rows();
        let pos = self.connection_cursor(&rows);
        let Some(depth) = rows.get(pos).map(ConnRow::depth) else {
            return;
        };
        if let Some(parent) = rows[..pos]
            .iter()
            .rev()
            .find(|r| matches!(r, ConnRow::Folder { .. }) && r.depth() < depth)
            .cloned()
        {
            self.select_connection_row(&parent);
        }
    }

    /// →: unfold the folder under the cursor.
    pub fn connections_right(&mut self) {
        if let Some(path) = &self.selected_folder {
            self.collapsed_folders.remove(path);
        }
    }

    /// Unfold `path` and its ancestors, so that what it holds is visible.
    fn reveal_folder(&mut self, path: Option<&str>) {
        for p in path.map(with_ancestors).unwrap_or_default() {
            self.collapsed_folders.remove(&p);
        }
    }

    /// Write the configuration now, reporting a failure in the status bar.
    pub fn persist_config(&mut self) {
        #[cfg(test)]
        {
            self.config_saves += 1;
        }
        if let Err(e) = self.config.save() {
            self.set_status(format!("Failed to save the configuration: {e}"));
        }
    }

    // ========== Folder prompt ==========

    fn open_folder_prompt(&mut self, kind: FolderPromptKind, input: String) {
        self.folder_prompt = Some(FolderPrompt::new(kind, input));
        self.dialog_mode = DialogMode::FolderPrompt;
    }

    /// `m` on a connection: move it, its folder prefilled. On a folder,
    /// the same as renaming it.
    pub fn open_move_prompt(&mut self) {
        if let Some(path) = self.selected_folder.clone() {
            self.open_folder_prompt(FolderPromptKind::RenameFolder(path.clone()), path);
        } else if let Some(conn) = self.config.connections.get(self.selected_connection) {
            let kind = FolderPromptKind::MoveConnection(conn.name.clone());
            let folder = conn.folder.clone().unwrap_or_default();
            self.open_folder_prompt(kind, folder);
        }
    }

    /// `f`: new folder, under the folder of the cursor.
    pub fn open_new_folder_prompt(&mut self) {
        let parent = self
            .cursor_folder()
            .map(|f| format!("{f}{}", folders::SEP))
            .unwrap_or_default();
        self.open_folder_prompt(FolderPromptKind::NewFolder, parent);
    }

    /// `r` on a folder: rename it (its whole path, so it can move too).
    pub fn open_rename_folder_prompt(&mut self) {
        if let Some(path) = self.selected_folder.clone() {
            self.open_folder_prompt(FolderPromptKind::RenameFolder(path.clone()), path);
        }
    }

    /// Apply the folder prompt; it stays open on an invalid path.
    pub fn apply_folder_prompt(&mut self) {
        let Some(prompt) = self.folder_prompt.clone() else {
            return;
        };
        let input = prompt.input.trim();
        let result = match &prompt.kind {
            FolderPromptKind::MoveConnection(name) => {
                let target = (!input.trim_matches(folders::SEP).trim().is_empty()).then_some(input);
                self.config.move_connection(name, target).map(|()| {
                    let folder = self
                        .config
                        .connections
                        .iter()
                        .find(|c| &c.name == name)
                        .and_then(|c| c.folder.clone());
                    self.reveal_folder(folder.as_deref());
                    match folder {
                        Some(f) => format!("Moved {name} to {f}"),
                        None => format!("Moved {name} to the root"),
                    }
                })
            }
            FolderPromptKind::NewFolder => self.config.create_folder(input).map(|path| {
                if let Some((parent, _)) = path.rsplit_once(folders::SEP) {
                    self.reveal_folder(Some(parent));
                }
                self.selected_folder = Some(path.clone());
                format!("Created folder: {path}")
            }),
            FolderPromptKind::RenameFolder(from) => folders::normalize(input).and_then(|to| {
                self.config.rename_folder(from, &to).map(|()| {
                    // Folded folders keep their state under their new path.
                    let moved: Vec<String> = self
                        .collapsed_folders
                        .iter()
                        .filter(|p| is_within(p, from))
                        .cloned()
                        .collect();
                    for p in moved {
                        self.collapsed_folders.remove(&p);
                        self.collapsed_folders
                            .insert(format!("{to}{}", &p[from.len()..]));
                    }
                    if let Some((parent, _)) = to.rsplit_once(folders::SEP) {
                        self.reveal_folder(Some(parent));
                    }
                    self.selected_folder = Some(to.clone());
                    format!("Renamed folder {from} to {to}")
                })
            }),
        };
        match result {
            Ok(msg) => {
                self.folder_prompt = None;
                self.close_dialog();
                self.persist_config();
                self.set_status(msg);
            }
            Err(e) => self.set_status(e),
        }
    }

    /// A key typed in the folder prompt.
    pub fn handle_folder_prompt_key(&mut self, key: crossterm::event::KeyCode) {
        use crate::tui::{next_char_boundary, prev_char_boundary};
        use crossterm::event::KeyCode;
        if key == KeyCode::Enter {
            self.apply_folder_prompt();
            return;
        }
        if key == KeyCode::Esc {
            self.close_dialog();
            self.set_status("Cancelled");
            return;
        }
        let Some(p) = self.folder_prompt.as_mut() else {
            return;
        };
        match key {
            KeyCode::Char(c) => {
                p.input.insert(p.cursor, c);
                p.cursor += c.len_utf8();
            }
            KeyCode::Backspace if p.cursor > 0 => {
                let prev = prev_char_boundary(&p.input, p.cursor);
                p.input.remove(prev);
                p.cursor = prev;
            }
            KeyCode::Delete if p.cursor < p.input.len() => {
                p.input.remove(p.cursor);
            }
            KeyCode::Left => p.cursor = prev_char_boundary(&p.input, p.cursor),
            KeyCode::Right => p.cursor = next_char_boundary(&p.input, p.cursor),
            KeyCode::Home => p.cursor = 0,
            KeyCode::End => p.cursor = p.input.len(),
            _ => {}
        }
    }

    /// `d` on a folder: delete it, its content moving up one level.
    pub fn delete_selected_folder(&mut self) {
        let Some(path) = self.selected_folder.clone() else {
            return;
        };
        match self.config.delete_folder(&path) {
            Ok(()) => {
                self.collapsed_folders.retain(|p| !is_within(p, &path));
                // The cursor goes to the parent folder, else the first line.
                self.selected_folder = path
                    .rsplit_once(folders::SEP)
                    .map(|(parent, _)| parent.to_string());
                if self.selected_folder.is_none() {
                    if let Some(first) = self.connection_rows().first().cloned() {
                        self.select_connection_row(&first);
                    }
                }
                self.persist_config();
                self.set_status(format!("Deleted folder: {path}"));
            }
            Err(e) => self.set_status(e),
        }
    }

    // ========== Hidden schemas ==========

    /// Whether the open connection hides `schema`.
    pub fn is_schema_hidden(&self, schema: &str) -> bool {
        let Some(current) = &self.current_connection_config else {
            return false;
        };
        self.config
            .get_connection(&current.name)
            .unwrap_or(current)
            .is_schema_hidden(schema)
    }

    /// `h`: hide the schema under the cursor of the Tables panel, or show it
    /// again when it is hidden.
    pub fn toggle_selected_schema_hidden(&mut self) {
        let Some(schema) = self
            .schemas
            .get(self.selected_schema)
            .map(|s| s.name.clone())
        else {
            return;
        };
        let Some(conn) = self
            .current_connection_config
            .as_ref()
            .map(|c| c.name.clone())
        else {
            return;
        };
        let hide = !self.is_schema_hidden(&schema);
        self.config.set_schema_hidden(&conn, &schema, hide);
        if let Some(current) = self.current_connection_config.as_mut() {
            if let Some(saved) = self.config.get_connection(&conn) {
                current.hidden_schemas = saved.hidden_schemas.clone();
            }
        }
        self.persist_config();
        if hide && !self.show_hidden_schemas {
            // Its lines are gone: the cursor goes to the first one left.
            self.selected_table = 0;
            if let Some(&(schema_idx, _)) = self.build_filtered_nav_items().first() {
                self.selected_schema = schema_idx;
            }
            self.tables_scroll = 0;
        }
        self.set_status(if hide {
            format!("Schema hidden: {schema} (H shows hidden schemas)")
        } else {
            format!("Schema shown: {schema}")
        });
    }

    /// `H`: show the hidden schemas (greyed) or hide them again.
    pub fn toggle_show_hidden_schemas(&mut self) {
        self.show_hidden_schemas = !self.show_hidden_schemas;
        if !self.show_hidden_schemas
            && self
                .schemas
                .get(self.selected_schema)
                .is_some_and(|s| self.is_schema_hidden(&s.name))
        {
            self.selected_table = 0;
            if let Some(&(schema_idx, _)) = self.build_filtered_nav_items().first() {
                self.selected_schema = schema_idx;
            }
            self.tables_scroll = 0;
        }
        self.set_status(if self.show_hidden_schemas {
            "Showing hidden schemas"
        } else {
            "Hidden schemas are hidden"
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::config::AppConfig;
    use crate::engine::models::{ConnectionConfig, SchemaInfo};

    fn conn(name: &str, folder: Option<&str>) -> ConnectionConfig {
        ConnectionConfig {
            name: name.into(),
            folder: folder.map(Into::into),
            ..Default::default()
        }
    }

    fn state() -> AppState {
        AppState::new(
            AppConfig {
                connections: vec![
                    conn("local", None),
                    conn("acme-prod", Some("Clients/ACME")),
                    conn("acme-dev", Some("Clients/ACME")),
                    conn("beta", Some("Clients")),
                ],
                folders: vec!["Empty".into()],
                ..Default::default()
            },
            false,
            true,
        )
    }

    fn labels(state: &AppState) -> Vec<String> {
        state
            .connection_rows()
            .iter()
            .map(|r| match r {
                ConnRow::Folder { path, .. } => format!("{}[{path}]", "  ".repeat(r.depth())),
                ConnRow::Connection { index, .. } => format!(
                    "{}{}",
                    "  ".repeat(r.depth()),
                    state.config.connections[*index].name
                ),
            })
            .collect()
    }

    #[test]
    fn rows_list_folders_then_connections_indented() {
        let s = state();
        assert_eq!(
            labels(&s),
            [
                "[Clients]",
                "  [Clients/ACME]",
                "    acme-dev",
                "    acme-prod",
                "  beta",
                "[Empty]",
                "local",
            ]
        );
    }

    #[test]
    fn navigation_goes_through_folders_and_folding_hides_content() {
        let mut s = state();
        s.selected_connection = 0; // local, the last line
        s.move_connection_cursor(true); // wraps to the first line
        assert_eq!(s.selected_folder.as_deref(), Some("Clients"));
        s.move_connection_cursor(true);
        assert_eq!(s.selected_folder.as_deref(), Some("Clients/ACME"));
        s.move_connection_cursor(true);
        assert_eq!(s.selected_folder, None);
        assert_eq!(s.selected_connection, 2); // acme-dev

        // ← on a connection goes to its folder, ← again folds it.
        s.connections_left();
        assert_eq!(s.selected_folder.as_deref(), Some("Clients/ACME"));
        s.connections_left();
        assert!(s.collapsed_folders.contains("Clients/ACME"));
        assert_eq!(labels(&s).len(), 5);
        s.move_connection_cursor(true);
        assert_eq!(s.selected_connection, 3); // beta, past the folded content
        s.move_connection_cursor(false);
        s.connections_right();
        assert_eq!(labels(&s).len(), 7);
        s.toggle_selected_folder();
        assert_eq!(labels(&s).len(), 5);
        s.toggle_selected_folder();
        assert_eq!(labels(&s).len(), 7);
    }

    #[test]
    fn moving_a_connection_normalizes_and_persists() {
        let mut s = state();
        s.selected_connection = 0;
        s.open_move_prompt();
        assert_eq!(s.dialog_mode, DialogMode::FolderPrompt);
        assert_eq!(s.folder_prompt.as_ref().unwrap().input, "");
        s.folder_prompt.as_mut().unwrap().input = " Clients / New ".into();
        s.collapsed_folders.insert("Clients".into());
        s.apply_folder_prompt();
        assert_eq!(s.dialog_mode, DialogMode::None);
        assert_eq!(
            s.config.connections[0].folder.as_deref(),
            Some("Clients/New")
        );
        // Revealed where it went.
        assert!(!s.collapsed_folders.contains("Clients"));
        assert_eq!(s.config_saves, 1);

        // Its folder prefilled; emptied, it goes back to the root.
        s.open_move_prompt();
        assert_eq!(s.folder_prompt.as_ref().unwrap().input, "Clients/New");
        s.folder_prompt.as_mut().unwrap().input = "  ".into();
        s.apply_folder_prompt();
        assert_eq!(s.config.connections[0].folder, None);
        assert_eq!(s.config_saves, 2);
    }

    #[test]
    fn an_invalid_folder_keeps_the_prompt_open() {
        let mut s = state();
        s.open_new_folder_prompt();
        s.folder_prompt.as_mut().unwrap().input = "a/../b".into();
        s.apply_folder_prompt();
        assert_eq!(s.dialog_mode, DialogMode::FolderPrompt);
        assert_eq!(s.config_saves, 0);
    }

    #[test]
    fn new_folder_goes_under_the_cursor_folder_and_is_selected() {
        let mut s = state();
        s.selected_connection = 1; // acme-prod
        s.open_new_folder_prompt();
        assert_eq!(s.folder_prompt.as_ref().unwrap().input, "Clients/ACME/");
        s.folder_prompt.as_mut().unwrap().input.push_str("Archive");
        s.apply_folder_prompt();
        assert!(s
            .config
            .folders
            .contains(&"Clients/ACME/Archive".to_string()));
        assert_eq!(s.selected_folder.as_deref(), Some("Clients/ACME/Archive"));
        assert_eq!(s.config_saves, 1);
    }

    #[test]
    fn renaming_a_folder_moves_its_content_and_folded_state() {
        let mut s = state();
        s.selected_folder = Some("Clients".into());
        s.collapsed_folders.insert("Clients/ACME".into());
        s.open_rename_folder_prompt();
        assert_eq!(s.folder_prompt.as_ref().unwrap().input, "Clients");
        s.folder_prompt.as_mut().unwrap().input = "Customers".into();
        s.apply_folder_prompt();
        assert_eq!(
            s.config.connections[1].folder.as_deref(),
            Some("Customers/ACME")
        );
        assert_eq!(s.config.connections[3].folder.as_deref(), Some("Customers"));
        assert!(s.collapsed_folders.contains("Customers/ACME"));
        assert_eq!(s.selected_folder.as_deref(), Some("Customers"));
        assert_eq!(s.config_saves, 1);
    }

    #[test]
    fn deleting_a_folder_moves_its_content_up() {
        let mut s = state();
        s.selected_folder = Some("Clients/ACME".into());
        s.delete_selected_folder();
        assert_eq!(s.config.connections[1].folder.as_deref(), Some("Clients"));
        assert_eq!(s.selected_folder.as_deref(), Some("Clients"));
        assert_eq!(s.config_saves, 1);
    }

    fn connected() -> AppState {
        let mut s = state();
        s.current_connection_config = Some(s.config.connections[0].clone());
        s.schemas = ["public", "audit", "pg_toast"]
            .iter()
            .map(|n| SchemaInfo {
                name: n.to_string(),
                tables: vec![format!("{n}_t")],
                expanded: true,
            })
            .collect();
        s
    }

    fn panel_schemas(s: &AppState) -> Vec<String> {
        s.get_filtered_schemas()
            .iter()
            .map(|(_, schema, _)| schema.name.clone())
            .collect()
    }

    #[test]
    fn hidden_schemas_leave_the_panel_and_the_completion() {
        let mut s = connected();
        s.selected_schema = 2;
        s.selected_table = 1; // pg_toast_t: hides its schema
        s.toggle_selected_schema_hidden();
        assert_eq!(s.config.connections[0].hidden_schemas, ["pg_toast"]);
        assert_eq!(s.config_saves, 1);
        assert_eq!(panel_schemas(&s), ["public", "audit"]);
        assert!(!s.get_known_tables().contains(&"pg_toast.pg_toast_t".into()));
        assert_eq!((s.selected_schema, s.selected_table), (0, 0));

        // Shown greyed on demand, and shown again with `h`.
        s.toggle_show_hidden_schemas();
        assert_eq!(panel_schemas(&s), ["public", "audit", "pg_toast"]);
        assert!(!s.get_known_tables().contains(&"pg_toast.pg_toast_t".into()));
        s.selected_schema = 2;
        s.toggle_selected_schema_hidden();
        assert!(s.config.connections[0].hidden_schemas.is_empty());
        assert!(s.get_known_tables().contains(&"pg_toast.pg_toast_t".into()));
        assert_eq!(s.config_saves, 2);
    }

    fn screen(state: &AppState) -> Vec<String> {
        use crate::tui::ui::clickable::ClickableRegistry;
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let registry = ClickableRegistry::new();
        terminal
            .draw(|f| crate::tui::ui::render_ui(f, state, &registry))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    #[test]
    fn the_panel_draws_the_tree_indented_at_80x24() {
        let mut s = state();
        s.collapsed_folders.insert("Empty".into());
        let rows = screen(&s).join(
            "
",
        );
        for line in [
            "▼ Clients/ (3)",
            "  ▼ ACME/ (2)",
            "      acme-dev",
            "    acme-prod",
            "▶ Empty/ (0)",
            "  local",
        ] {
            assert!(
                rows.contains(line),
                "{line:?} missing in
{rows}"
            );
        }
        // Folded: its content is not drawn.
        s.collapsed_folders.insert("Clients".into());
        let rows = screen(&s).join(
            "
",
        );
        assert!(rows.contains("▶ Clients/ (3)"), "{rows}");
        assert!(!rows.contains("acme-dev"), "{rows}");
    }

    #[test]
    fn the_prompt_and_greyed_hidden_schemas_are_drawn_at_80x24() {
        let mut s = connected();
        s.config.set_schema_hidden("local", "audit", true);
        s.show_hidden_schemas = true;
        s.active_panel = crate::tui::ActivePanel::Tables;
        let rows = screen(&s).join(
            "
",
        );
        // Greyed and marked (cut by the narrow sidebar).
        assert!(rows.contains("▼ audit (1) [hid"), "{rows}");
        assert!(rows.contains("h/H"), "{rows}");

        s.active_panel = crate::tui::ActivePanel::Connections;
        s.open_move_prompt();
        let rows = screen(&s).join(
            "
",
        );
        assert!(rows.contains("Move to folder"), "{rows}");
        assert!(rows.contains("empty = root"), "{rows}");
    }

    fn press(s: &mut AppState, key: crossterm::event::KeyCode) {
        use crate::tui::key_handlers::{handle_connections_keys, KeyAction};
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        if s.is_dialog_open() {
            assert_eq!(s.dialog_mode, DialogMode::FolderPrompt);
            s.handle_folder_prompt_key(key);
        } else {
            assert_eq!(
                rt.block_on(handle_connections_keys(s, key)),
                KeyAction::Consumed
            );
        }
    }

    fn type_text(s: &mut AppState, text: &str) {
        for c in text.chars() {
            press(s, crossterm::event::KeyCode::Char(c));
        }
    }

    #[test]
    fn keys_create_move_rename_and_delete_folders() {
        use crossterm::event::KeyCode;
        let mut s = state();
        s.selected_connection = 0; // local, at the root

        // f: new folder at the root.
        press(&mut s, KeyCode::Char('f'));
        type_text(&mut s, "Ops");
        press(&mut s, KeyCode::Enter);
        assert!(s.config.folders.contains(&"Ops".to_string()));
        assert_eq!(s.selected_folder.as_deref(), Some("Ops"));

        // Enter on a folder folds it (no connection is opened).
        press(&mut s, KeyCode::Enter);
        assert!(s.collapsed_folders.contains("Ops"));
        press(&mut s, KeyCode::Right);
        assert!(!s.collapsed_folders.contains("Ops"));

        // m on a connection: its folder typed, Backspace and Esc work.
        s.selected_folder = None;
        s.selected_connection = 0;
        press(&mut s, KeyCode::Char('m'));
        type_text(&mut s, "Opz");
        press(&mut s, KeyCode::Backspace);
        type_text(&mut s, "s");
        press(&mut s, KeyCode::Esc);
        assert_eq!(s.config.connections[0].folder, None);
        press(&mut s, KeyCode::Char('m'));
        type_text(&mut s, "Ops");
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.config.connections[0].folder.as_deref(), Some("Ops"));

        // r on a folder renames it, with its connections.
        s.selected_folder = Some("Ops".into());
        press(&mut s, KeyCode::Char('r'));
        for _ in 0..3 {
            press(&mut s, KeyCode::Backspace);
        }
        type_text(&mut s, "Infra/Ops");
        press(&mut s, KeyCode::Enter);
        assert_eq!(s.config.connections[0].folder.as_deref(), Some("Infra/Ops"));

        // d on a folder deletes it: its connection moves up, none is deleted.
        press(&mut s, KeyCode::Char('d'));
        assert_eq!(s.config.connections.len(), 4);
        assert_eq!(s.config.connections[0].folder.as_deref(), Some("Infra"));
        assert_eq!(s.config_saves, 4);
    }

    #[test]
    fn editing_a_connection_keeps_its_folder_and_hidden_schemas() {
        let mut s = state();
        s.config.connections[1].hidden_schemas = vec!["audit".into()];
        s.open_edit_connection_dialog(1);
        s.new_connection.name = "acme-production".into();
        let edited = s.new_connection.to_config();
        assert_eq!(edited.folder.as_deref(), Some("Clients/ACME"));
        assert_eq!(edited.hidden_schemas, ["audit"]);

        // A new connection goes into the folder of the cursor.
        s.close_dialog();
        s.selected_folder = Some("Empty".into());
        s.open_new_connection_dialog();
        assert_eq!(
            s.new_connection.to_config().folder.as_deref(),
            Some("Empty")
        );
    }
}
