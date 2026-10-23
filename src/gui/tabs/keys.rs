//! « Clés » tab of a Redis database: the keys matching a pattern (SCAN,
//! « Charger plus »), and the value of the selected key on the right,
//! edited by type (`redis::grid_edits`, `KeyEdit`).

use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::models::{Column, QueryResult};
use crate::engine::nosql::redis::{self, KeyInfo, RedisType, RedisValue, ScanPage, ValuePage};
use crate::gui::grid::changes::RowRef;
use crate::gui::grid::{self, GridAction, GridOptions, GridState};
use crate::gui::theme::ERROR;
use crate::gui::value_panel::pretty_json;
use crate::gui::worker::{Event, TabId};

/// Elements of a value read per page.
pub const VALUE_PAGE: usize = 500;

/// The types the type filter offers.
pub const TYPES: [RedisType; 6] = [
    RedisType::String,
    RedisType::List,
    RedisType::Set,
    RedisType::ZSet,
    RedisType::Hash,
    RedisType::Stream,
];

/// The name of a type, as `TYPE` answers it.
pub fn type_label(t: &RedisType) -> &str {
    match t {
        RedisType::String => "string",
        RedisType::List => "list",
        RedisType::Set => "set",
        RedisType::ZSet => "zset",
        RedisType::Hash => "hash",
        RedisType::Stream => "stream",
        RedisType::Other(name) => name,
    }
}

/// The icon of a key of type `t`.
pub fn type_icon(t: &RedisType) -> &'static str {
    match t {
        RedisType::String => icon::TEXT_T,
        RedisType::List => icon::LIST_NUMBERS,
        RedisType::Set => icon::CIRCLES_THREE,
        RedisType::ZSet => icon::RANKING,
        RedisType::Hash => icon::HASH,
        RedisType::Stream => icon::WAVES,
        RedisType::Other(_) => icon::PUZZLE_PIECE,
    }
}

/// A TTL as shown: « persistante » or « 42 s ».
pub fn ttl_label(ttl: Option<i64>) -> String {
    match ttl {
        None => "persistante".into(),
        Some(s) => format!("{s} s"),
    }
}

/// The TTL typed in the field: empty = persistent, else whole seconds.
pub fn parse_ttl(text: &str) -> Result<Option<i64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<i64>() {
        Ok(n) if n > 0 => Ok(Some(n)),
        _ => Err("Expiration : un nombre de secondes positif, ou vide (persistante)".into()),
    }
}

/// The grid of the keys: clé, type, TTL, taille.
pub fn keys_result(keys: &[KeyInfo]) -> QueryResult {
    let column = |name: &str| Column {
        name: name.into(),
        type_name: String::new(),
        nullable: true,
        is_primary_key: false,
    };
    QueryResult {
        columns: ["clé", "type", "TTL", "taille"].map(column).to_vec(),
        rows: keys
            .iter()
            .map(|k| {
                vec![
                    redis::display_bytes(&k.key),
                    type_label(&k.kind).to_string(),
                    ttl_label(k.ttl),
                    k.size.map(|s| s.to_string()).unwrap_or_default(),
                ]
            })
            .collect(),
        ..Default::default()
    }
}

/// What the tab asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    /// SCAN from the start (`more: false`) or go on from the cursor.
    Scan {
        more: bool,
    },
    /// Read the value of the open key from `from` (offset or cursor).
    Read {
        from: u64,
    },
    /// Ask "Abandonner les modifications ?" before opening another key.
    Confirm,
    /// Apply the value grid's pending edits.
    SubmitValue,
    /// Write the string being edited.
    SaveString,
    /// Set the TTL of the open key (`None`: persistent).
    SetTtl(Option<i64>),
    /// « Renommer… » the open key.
    Rename,
    /// « Supprimer » the open key (asks first).
    Delete,
    /// « Nouvelle clé… ».
    NewKey,
    Applied(usize),
    SubmitFailed(String),
}

pub struct KeysTab {
    pub db: u8,
    /// SCAN pattern as typed, and the one the keys were listed with.
    pub pattern: String,
    pub applied_pattern: String,
    pub kind: Option<RedisType>,
    pub keys: Vec<KeyInfo>,
    /// Next SCAN cursor (0: the scan is over, once `scanned`).
    pub cursor: u64,
    pub scanned: bool,
    /// The pending scan appends (« Charger plus »).
    pub appending: bool,
    pub keys_result: QueryResult,
    pub keys_grid: GridState,
    pub error: Option<String>,
    pub needs_scan: bool,
    /// The key shown on the right.
    pub open_key: Option<Vec<u8>>,
    pub info: Option<KeyInfo>,
    pub value: Option<ValuePage>,
    /// Where the value page shown starts.
    pub value_from: u64,
    pub value_result: QueryResult,
    pub value_grid: GridState,
    pub value_error: Option<String>,
    /// A string value being edited, and as loaded.
    pub string_buf: String,
    string_initial: String,
    pub ttl_input: String,
    /// Key to open once the pending edits are dropped or submitted.
    pub confirm: Option<Vec<u8>>,
    /// Close the tab once the running write succeeds (« Submit » in the
    /// confirmation of its closing).
    pub close_after_submit: bool,
}

/// What the tab needs from the app to draw itself.
pub struct KeysContext {
    pub tab: TabId,
    pub connected: bool,
    pub scanning: bool,
    pub reading: bool,
    pub submitting: bool,
}

impl KeysTab {
    /// The keys of `db`; `open` is shown at once (from the explorer).
    pub fn new(db: u8, open: Option<Vec<u8>>) -> Self {
        Self {
            db,
            pattern: "*".into(),
            applied_pattern: "*".into(),
            kind: None,
            keys: Vec::new(),
            cursor: 0,
            scanned: false,
            appending: false,
            keys_result: keys_result(&[]),
            keys_grid: GridState::default(),
            error: None,
            needs_scan: true,
            open_key: open,
            info: None,
            value: None,
            value_from: 0,
            value_result: QueryResult::default(),
            value_grid: GridState::default(),
            value_error: None,
            string_buf: String::new(),
            string_initial: String::new(),
            ttl_input: String::new(),
            confirm: None,
            close_after_submit: false,
        }
    }

    /// Whether the open key can be edited (module types are read only).
    pub fn value_editable(&self) -> bool {
        self.info
            .as_ref()
            .is_some_and(|i| !matches!(i.kind, RedisType::Other(_) | RedisType::String))
            && self.value.is_some()
    }

    pub fn string_dirty(&self) -> bool {
        self.string_buf != self.string_initial
    }

    /// Pending value edits (grid or string).
    pub fn has_pending_edits(&self) -> bool {
        !self.value_grid.edits.is_empty() || self.string_dirty()
    }

    pub fn commit_editor(&mut self) {
        self.value_grid.commit_editing(&self.value_result.rows);
    }

    /// Show `key`, unless value edits are pending (then confirm first).
    pub fn open(&mut self, key: Vec<u8>) -> Option<KeyAction> {
        if self.open_key.as_ref() == Some(&key) {
            return None;
        }
        self.commit_editor();
        if self.has_pending_edits() {
            self.confirm = Some(key);
            return Some(KeyAction::Confirm);
        }
        self.show_key(key)
    }

    fn show_key(&mut self, key: Vec<u8>) -> Option<KeyAction> {
        self.open_key = Some(key);
        self.info = None;
        self.value = None;
        self.value_from = 0;
        self.value_error = None;
        self.value_result = QueryResult::default();
        self.value_grid.new_result();
        self.string_buf.clear();
        self.string_initial.clear();
        Some(KeyAction::Read { from: 0 })
    }

    /// "Abandonner": drop the value edits and open the key asked.
    pub fn discard_and_continue(&mut self) -> Option<KeyAction> {
        let key = self.confirm.take()?;
        self.value_grid.discard_edits();
        self.string_buf = self.string_initial.clone();
        self.show_key(key)
    }

    /// The keys listed, in the grid's order: the one at `row`.
    pub fn key_at(&self, row: RowRef) -> Option<&KeyInfo> {
        match row {
            RowRef::Base(i) => self.keys.get(i),
            RowRef::New(_) => None,
        }
    }

    /// The info the value grid's edits are made against (`grid_edits`).
    pub fn edit_target(&self) -> Option<(&KeyInfo, &ValuePage)> {
        Some((self.info.as_ref()?, self.value.as_ref()?))
    }

    /// The key edits of the value grid's pending edits (`redis::grid_edits`),
    /// `None` without a value or edits.
    pub fn edits(&self) -> Option<Result<Vec<redis::KeyEdit>, String>> {
        let (info, page) = self.edit_target()?;
        if self.value_grid.edits.is_empty() {
            return None;
        }
        let changes = self
            .value_grid
            .edits
            .to_row_changes(&self.value_result.rows);
        Some(redis::grid_edits(info, page, &changes))
    }

    /// What applies the pending value edits: the string typed, else the
    /// grid's.
    pub fn submit_pending(&self) -> Option<KeyAction> {
        if self.string_dirty() {
            Some(KeyAction::SaveString)
        } else if !self.value_grid.edits.is_empty() {
            Some(KeyAction::SubmitValue)
        } else {
            None
        }
    }

    /// The string being edited, as bytes (`redis::parse_display`).
    pub fn string_value(&self) -> Result<Vec<u8>, String> {
        redis::parse_display(&self.string_buf)
    }

    /// Outcomes of this tab's scans, reads and writes.
    pub fn on_event(&mut self, ev: Event) -> Option<KeyAction> {
        match ev {
            Event::KeyPage { outcome, .. } => {
                match outcome {
                    Ok(ScanPage { keys, cursor }) => {
                        if !std::mem::take(&mut self.appending) {
                            self.keys.clear();
                            self.keys_grid.new_result();
                        }
                        self.keys.extend(keys);
                        self.cursor = cursor;
                        self.scanned = true;
                        self.keys_result = keys_result(&self.keys);
                        self.error = None;
                    }
                    Err(e) => {
                        self.appending = false;
                        self.error = Some(e);
                    }
                }
                None
            }
            Event::KeyValue { key, outcome, .. } => {
                if self.open_key.as_ref() != Some(&key) {
                    return None;
                }
                match outcome {
                    Ok(Some((info, page))) => {
                        self.ttl_input = info.ttl.map(|t| t.to_string()).unwrap_or_default();
                        if let RedisValue::String(bytes) = &page.value {
                            self.string_buf = redis::display_bytes(bytes);
                            self.string_initial = self.string_buf.clone();
                        }
                        self.value_result = redis::value_result(&page.value);
                        self.value_grid.new_result();
                        self.info = Some(info);
                        self.value = Some(page);
                        self.value_error = None;
                    }
                    Ok(None) => {
                        self.info = None;
                        self.value = None;
                        self.value_error = Some("Clé introuvable (supprimée ou expirée)".into());
                    }
                    Err(e) => self.value_error = Some(e),
                }
                None
            }
            Event::Submitted { outcome, .. } => match outcome {
                Ok(n) => {
                    self.value_grid.discard_edits();
                    self.string_initial = self.string_buf.clone();
                    self.value_error = None;
                    Some(KeyAction::Applied(n))
                }
                Err(e) => {
                    self.close_after_submit = false;
                    self.value_error = Some(format!("Modification interrompue : {e}"));
                    Some(KeyAction::SubmitFailed(e))
                }
            },
            _ => None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: KeysContext) -> Option<KeyAction> {
        if self.needs_scan && cx.connected && !cx.scanning {
            self.needs_scan = false;
            return Some(KeyAction::Scan { more: false });
        }
        let mut action = self.toolbar(ui, &cx);
        if let Some(e) = &self.error {
            ui.colored_label(ERROR, format!("{} {e}", icon::WARNING));
        }
        ui.separator();
        let width = ui.available_width();
        let left = (width * 0.42).clamp(240.0, 560.0);
        ui.horizontal_top(|ui| {
            let height = ui.available_height();
            ui.allocate_ui(egui::vec2(left, height), |ui| {
                ui.vertical(|ui| {
                    if let Some(a) = self.keys_list(ui, &cx) {
                        action = Some(a);
                    }
                });
            });
            ui.separator();
            ui.vertical(|ui| {
                if let Some(a) = self.value_view(ui, &cx) {
                    action = Some(a);
                }
            });
        });
        action
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &KeysContext) -> Option<KeyAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} db{}", icon::DATABASE, self.db)).strong());
            ui.separator();
            ui.label("Motif");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.pattern)
                    .code_editor()
                    .desired_width(200.0)
                    .hint_text("user:*"),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let label = self.kind.as_ref().map_or("Tous les types", type_label);
            let mut kind = self.kind.clone();
            egui::ComboBox::from_id_salt(("keys_type", cx.tab))
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut kind, None, "Tous les types");
                    for t in TYPES {
                        let text = format!("{} {}", type_icon(&t), type_label(&t));
                        ui.selectable_value(&mut kind, Some(t), text);
                    }
                });
            let kind_changed = kind != self.kind;
            self.kind = kind;
            let can = cx.connected && !cx.scanning;
            if ui
                .add_enabled(
                    can,
                    egui::Button::new(format!("{} Chercher", icon::MAGNIFYING_GLASS)),
                )
                .clicked()
                || (can && (enter || kind_changed))
            {
                self.applied_pattern = match self.pattern.trim() {
                    "" => "*".to_string(),
                    p => p.to_string(),
                };
                action = Some(KeyAction::Scan { more: false });
            }
            if cx.scanning {
                ui.spinner();
            }
            ui.separator();
            if ui
                .add_enabled(
                    cx.connected,
                    egui::Button::new(format!("{} Nouvelle clé…", icon::PLUS)),
                )
                .clicked()
            {
                action = Some(KeyAction::NewKey);
            }
        });
        action
    }

    fn keys_list(&mut self, ui: &mut egui::Ui, cx: &KeysContext) -> Option<KeyAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            let n = self.keys.len();
            let more = self.scanned && self.cursor != 0;
            ui.label(
                RichText::new(format!(
                    "{n} clé{}{}",
                    if n == 1 { "" } else { "s" },
                    if more { " (scan non terminé)" } else { "" }
                ))
                .weak(),
            );
            if ui
                .add_enabled(
                    more && cx.connected && !cx.scanning,
                    egui::Button::new(format!("{} Charger plus", icon::ARROW_DOWN)),
                )
                .on_hover_text("Continuer le SCAN")
                .clicked()
            {
                action = Some(KeyAction::Scan { more: true });
            }
        });
        let before = self.keys_grid.selected.map(|(r, _)| r);
        let grid_action = grid::show(
            ui,
            egui::Id::new(("keys_grid", cx.tab)),
            &mut self.keys_grid,
            GridOptions {
                result: &self.keys_result,
                editable: false,
                server_sort: false,
                sort_indicator: None,
                links: &[],
                incoming: &[],
            },
        );
        let after = self.keys_grid.selected.map(|(r, _)| r);
        if grid_action == GridAction::SelectionChanged && after != before {
            let key = after.and_then(|r| self.key_at(r)).map(|k| k.key.clone());
            if let Some(key) = key {
                if let Some(a) = self.open(key) {
                    action = Some(a);
                }
            }
        }
        action
    }

    fn value_view(&mut self, ui: &mut egui::Ui, cx: &KeysContext) -> Option<KeyAction> {
        let Some(key) = self.open_key.clone() else {
            ui.label(RichText::new("Sélectionnez une clé.").weak());
            return None;
        };
        let mut action = None;
        let can = cx.connected && !cx.submitting;
        ui.horizontal_wrapped(|ui| {
            let kind = self.info.as_ref().map(|i| &i.kind);
            let glyph = kind.map_or(icon::KEY, type_icon);
            ui.label(RichText::new(format!("{glyph} {}", redis::display_bytes(&key))).strong());
            if let Some(i) = &self.info {
                ui.label(RichText::new(type_label(&i.kind)).weak());
                if let Some(v) = &self.value {
                    ui.label(RichText::new(format!("{} élément(s)", v.total)).weak());
                }
            }
            if cx.reading || cx.submitting {
                ui.spinner();
            }
        });
        ui.horizontal(|ui| {
            ui.label("TTL");
            ui.add(
                egui::TextEdit::singleline(&mut self.ttl_input)
                    .desired_width(80.0)
                    .hint_text("persistante"),
            )
            .on_hover_text("Secondes ; vide = persistante");
            if ui
                .add_enabled(can && self.info.is_some(), egui::Button::new("Appliquer"))
                .clicked()
            {
                match parse_ttl(&self.ttl_input) {
                    Ok(ttl) => action = Some(KeyAction::SetTtl(ttl)),
                    Err(e) => self.value_error = Some(e),
                }
            }
            ui.separator();
            if ui
                .add_enabled(can, egui::Button::new(icon::ARROW_CLOCKWISE))
                .on_hover_text("Recharger")
                .clicked()
            {
                action = Some(KeyAction::Read {
                    from: self.value_from,
                });
            }
            if ui
                .add_enabled(
                    can && self.info.is_some(),
                    egui::Button::new(format!("{} Renommer…", icon::PENCIL_SIMPLE)),
                )
                .clicked()
            {
                action = Some(KeyAction::Rename);
            }
            if ui
                .add_enabled(
                    can && self.info.is_some(),
                    egui::Button::new(
                        RichText::new(format!("{} Supprimer", icon::TRASH)).color(ERROR),
                    ),
                )
                .clicked()
            {
                action = Some(KeyAction::Delete);
            }
        });
        if let Some(e) = &self.value_error {
            ui.colored_label(ERROR, format!("{} {e}", icon::WARNING));
        }
        let (Some(info), Some(page)) = (&self.info, &self.value) else {
            return action;
        };
        ui.separator();
        match &info.kind {
            RedisType::String => {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            can && self.string_dirty(),
                            egui::Button::new(format!("{} Enregistrer", icon::CHECK)),
                        )
                        .clicked()
                    {
                        action = Some(KeyAction::SaveString);
                    }
                    if ui
                        .add_enabled(self.string_dirty(), egui::Button::new("Rétablir"))
                        .clicked()
                    {
                        self.string_buf = self.string_initial.clone();
                    }
                });
                let json = pretty_json(&self.string_buf);
                egui::ScrollArea::vertical()
                    .id_salt(("keys_string", cx.tab))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.string_buf)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .desired_rows(10),
                        );
                        if let Some(json) = json {
                            ui.add_space(6.0);
                            ui.label(RichText::new("Aperçu JSON").weak().small());
                            ui.add(
                                egui::TextEdit::multiline(&mut json.as_str())
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        }
                    });
            }
            kind => {
                let from = self.value_from;
                let next = page.next;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(can && from > 0, egui::Button::new(icon::CARET_LINE_LEFT))
                        .on_hover_text("Début")
                        .clicked()
                    {
                        action = Some(KeyAction::Read { from: 0 });
                    }
                    if ui
                        .add_enabled(can && next.is_some(), egui::Button::new(icon::CARET_RIGHT))
                        .on_hover_text(format!("{VALUE_PAGE} éléments suivants"))
                        .clicked()
                    {
                        if let Some(n) = next {
                            action = Some(KeyAction::Read { from: n });
                        }
                    }
                    if matches!(kind, RedisType::Stream) {
                        ui.label(
                            RichText::new("Nouvelle entrée : id « * » (attribué par le serveur)")
                                .weak()
                                .small(),
                        );
                    }
                    if matches!(kind, RedisType::Other(_)) {
                        ui.label(
                            RichText::new(format!("{} Type de module : lecture seule", icon::LOCK))
                                .weak(),
                        );
                    }
                });
                let editable = self.value_editable() && can && !cx.reading;
                let grid_action = grid::show(
                    ui,
                    egui::Id::new(("keys_value", cx.tab)),
                    &mut self.value_grid,
                    GridOptions {
                        result: &self.value_result,
                        editable,
                        server_sort: false,
                        sort_indicator: None,
                        links: &[],
                        incoming: &[],
                    },
                );
                if grid_action == GridAction::Submit && !self.value_grid.edits.is_empty() {
                    action = Some(KeyAction::SubmitValue);
                }
            }
        }
        action
    }

    /// Record a value read from `from` was requested.
    pub fn reading_from(&mut self, from: u64) {
        self.value_from = from;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, kind: RedisType, ttl: Option<i64>) -> KeyInfo {
        KeyInfo {
            key: name.as_bytes().to_vec(),
            kind,
            ttl,
            size: Some(3),
        }
    }

    #[test]
    fn keys_grid_shows_type_ttl_and_size() {
        let r = keys_result(&[
            key("user:1", RedisType::Hash, None),
            key("cache", RedisType::Other("ReJSON-RL".into()), Some(30)),
        ]);
        let names: Vec<_> = r.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["clé", "type", "TTL", "taille"]);
        assert_eq!(r.rows[0], ["user:1", "hash", "persistante", "3"]);
        assert_eq!(r.rows[1], ["cache", "ReJSON-RL", "30 s", "3"]);
    }

    #[test]
    fn ttl_field_is_seconds_or_persistent() {
        assert_eq!(parse_ttl(" "), Ok(None));
        assert_eq!(parse_ttl("60"), Ok(Some(60)));
        assert!(parse_ttl("0").is_err());
        assert!(parse_ttl("-1").is_err());
        assert!(parse_ttl("1h").is_err());
    }

    fn scanned(t: &mut KeysTab, keys: Vec<KeyInfo>, cursor: u64) {
        t.on_event(Event::KeyPage {
            tab: 1,
            run: 1,
            outcome: Ok(ScanPage { keys, cursor }),
        });
    }

    #[test]
    fn load_more_appends_a_new_scan_replaces() {
        let mut t = KeysTab::new(0, None);
        scanned(&mut t, vec![key("a", RedisType::String, None)], 42);
        assert_eq!((t.keys.len(), t.cursor, t.scanned), (1, 42, true));
        t.appending = true;
        scanned(&mut t, vec![key("b", RedisType::List, None)], 0);
        assert_eq!(t.keys.len(), 2);
        assert_eq!(t.keys_result.rows.len(), 2);
        assert!(!t.appending);
        scanned(&mut t, vec![key("c", RedisType::Set, None)], 0);
        assert_eq!(t.keys.len(), 1, "a new scan replaces the list");
        t.on_event(Event::KeyPage {
            tab: 1,
            run: 2,
            outcome: Err("NOPERM".into()),
        });
        assert_eq!(t.error.as_deref(), Some("NOPERM"));
        assert_eq!(t.keys.len(), 1, "the list stays");
    }

    fn value(t: &mut KeysTab, name: &str, info: KeyInfo, v: RedisValue) {
        t.on_event(Event::KeyValue {
            tab: 1,
            run: 3,
            key: name.as_bytes().to_vec(),
            outcome: Ok(Some((
                info,
                ValuePage {
                    value: v,
                    total: 1,
                    from: 0,
                    next: None,
                },
            ))),
        });
    }

    #[test]
    fn values_of_other_keys_are_ignored_and_strings_are_editable_text() {
        let mut t = KeysTab::new(0, Some(b"greeting".to_vec()));
        value(
            &mut t,
            "other",
            key("other", RedisType::String, None),
            RedisValue::String(b"x".to_vec()),
        );
        assert!(t.info.is_none(), "not the open key");
        value(
            &mut t,
            "greeting",
            key("greeting", RedisType::String, Some(9)),
            RedisValue::String(b"hello".to_vec()),
        );
        assert_eq!(t.string_buf, "hello");
        assert_eq!(t.ttl_input, "9");
        assert!(!t.string_dirty());
        assert!(!t.value_editable(), "strings are edited as text");
        t.string_buf.push('!');
        assert!(t.has_pending_edits());
        // Opening another key asks first.
        assert_eq!(t.open(b"b".to_vec()), Some(KeyAction::Confirm));
        assert_eq!(t.open_key.as_deref(), Some(&b"greeting"[..]));
        assert_eq!(t.discard_and_continue(), Some(KeyAction::Read { from: 0 }));
        assert_eq!(t.open_key.as_deref(), Some(&b"b"[..]));
        assert!(!t.has_pending_edits());
    }

    #[test]
    fn a_vanished_key_is_reported() {
        let mut t = KeysTab::new(0, Some(b"gone".to_vec()));
        t.on_event(Event::KeyValue {
            tab: 1,
            run: 3,
            key: b"gone".to_vec(),
            outcome: Ok(None),
        });
        assert!(t.value_error.as_deref().unwrap().contains("introuvable"));
    }

    #[test]
    fn value_edits_become_the_engine_s_key_edits() {
        let mut t = KeysTab::new(0, Some(b"l".to_vec()));
        assert_eq!(t.edits(), None);
        value(
            &mut t,
            "l",
            key("l", RedisType::List, None),
            RedisValue::List(vec![b"a".to_vec()]),
        );
        assert_eq!(t.edits(), None, "no edits");
        assert_eq!(t.submit_pending(), None);
        // A new element typed in the grid: what the engine makes of it.
        let r = t.value_grid.edits.add_row(1);
        t.value_grid
            .edits
            .set(&t.value_result.rows.clone(), r, 0, "b".into());
        let changes = t.value_grid.edits.to_row_changes(&t.value_result.rows);
        let (info, page) = t.edit_target().unwrap();
        assert_eq!(t.edits(), Some(redis::grid_edits(info, page, &changes)));
        assert_eq!(t.submit_pending(), Some(KeyAction::SubmitValue));
    }

    #[test]
    fn module_types_are_read_only() {
        let mut t = KeysTab::new(0, Some(b"j".to_vec()));
        value(
            &mut t,
            "j",
            key("j", RedisType::Other("ReJSON-RL".into()), None),
            RedisValue::List(vec![]),
        );
        assert!(!t.value_editable());
        value(
            &mut t,
            "j",
            key("j", RedisType::List, None),
            RedisValue::List(vec![b"a".to_vec()]),
        );
        assert!(t.value_editable());
        assert!(t.edit_target().is_some());
    }

    #[test]
    fn submitted_edits_are_cleared_on_success_only() {
        let mut t = KeysTab::new(0, Some(b"s".to_vec()));
        value(
            &mut t,
            "s",
            key("s", RedisType::String, None),
            RedisValue::String(b"a".to_vec()),
        );
        t.string_buf = "b".into();
        let failed = t.on_event(Event::Submitted {
            tab: 1,
            run: 4,
            outcome: Err("WRONGTYPE".into()),
        });
        assert_eq!(failed, Some(KeyAction::SubmitFailed("WRONGTYPE".into())));
        assert!(t.string_dirty());
        let ok = t.on_event(Event::Submitted {
            tab: 1,
            run: 5,
            outcome: Ok(1),
        });
        assert_eq!(ok, Some(KeyAction::Applied(1)));
        assert!(!t.string_dirty());
    }
}
