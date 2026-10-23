//! « Documents » tab of a MongoDB collection: one page of documents at a
//! time with server-side filter, sort and projection (shell syntax), inline
//! edits submitted as document writes (`mongo::doc_changes`), the JSON
//! editor for whole documents, and an aggregation mode.

use bson::{Bson, Document};
use egui::{Key, RichText};
use egui_phosphor::regular as icon;

use crate::engine::models::{DatabaseType, SchemaInfo};
use crate::engine::nosql::mongo::{self, shell};
use crate::engine::nosql::mongo::{CollectionRef, DocChange, DocPage, DocQuery};
use crate::engine::nosql::NoSqlScope;
use crate::gui::editor::completion::{self, Completion, CompletionSource, Scope};
use crate::gui::grid::changes::RowRef;
use crate::gui::grid::{self, GridAction, GridOptions, GridState};
use crate::gui::status::one_line_label;
use crate::gui::theme::ERROR;
use crate::gui::worker::{Event, TabId};

use super::data::{busy_hint, can_leave, page_count, Nav};

pub const PAGE_SIZE: u32 = 500;
/// Rows of an aggregation's result.
pub const MAX_AGGREGATE_ROWS: usize = 1000;

/// What the tab shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocMode {
    Find,
    Aggregate,
}

/// What the tab asks the app to do (it owns the worker and the sessions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocAction {
    /// Load `query`'s page, and count its documents again when `count`.
    Load {
        count: bool,
    },
    /// Submit the grid's pending edits.
    Submit,
    /// Ask "Abandonner les modifications ?" for `confirm`.
    Confirm,
    Applied(usize),
    SubmitFailed(String),
    /// Run the pipeline.
    Aggregate,
    /// Open the JSON editor on document `Some(row)` of the page, or on a
    /// new document (`None`).
    Edit(Option<usize>),
    /// A new document, copy of row `usize` without its `_id`.
    Duplicate(usize),
    /// Export the collection (transfer dialog, filter filled in).
    Export,
}

/// Aggregation stages offered by « Ajouter une étape ».
pub const STAGES: [(&str, &str); 9] = [
    ("$match", "{ $match: {} }"),
    ("$project", "{ $project: { _id: 1 } }"),
    ("$group", "{ $group: { _id: \"$champ\", total: { $sum: 1 } } }"),
    ("$sort", "{ $sort: { _id: 1 } }"),
    ("$limit", "{ $limit: 100 }"),
    ("$unwind", "{ $unwind: \"$champ\" }"),
    (
        "$lookup",
        "{ $lookup: { from: \"autre\", localField: \"champ\", foreignField: \"_id\", as: \"lies\" } }",
    ),
    ("$addFields", "{ $addFields: { nouveau: \"$champ\" } }"),
    ("$count", "{ $count: \"total\" }"),
];

/// `db.getCollection("name")` (the name as a JSON string: any character
/// survives).
pub fn collection_call(coll: &CollectionRef) -> String {
    let name = serde_json::to_string(&coll.coll).unwrap_or_else(|_| "\"\"".into());
    format!("db.getCollection({name})")
}

/// The console command reading a collection (`find` and its modifiers).
pub fn find_command(coll: &CollectionRef, filter: &str, sort: &str, projection: &str) -> String {
    let or_empty = |s: &str| {
        if s.trim().is_empty() {
            "{}".to_string()
        } else {
            s.trim().to_string()
        }
    };
    let mut cmd = collection_call(coll);
    if projection.trim().is_empty() {
        cmd.push_str(&format!(".find({})", or_empty(filter)));
    } else {
        cmd.push_str(&format!(
            ".find({}, {})",
            or_empty(filter),
            projection.trim()
        ));
    }
    if !sort.trim().is_empty() {
        cmd.push_str(&format!(".sort({})", sort.trim()));
    }
    cmd
}

/// The console command of an aggregation.
pub fn aggregate_command(coll: &CollectionRef, pipeline: &str) -> String {
    let pipeline = match pipeline.trim() {
        "" => "[]",
        p => p,
    };
    format!("{}.aggregate({pipeline})", collection_call(coll))
}

/// `pipeline` with `stage` appended before its closing `]` (a new
/// pipeline when it holds none).
pub fn add_stage(pipeline: &str, stage: &str) -> String {
    let trimmed = pipeline.trim_end();
    match trimmed.strip_suffix(']') {
        Some(body) if trimmed.trim_start().starts_with('[') => {
            let body = body.trim_end();
            let sep = if body.trim_start_matches('[').trim().is_empty() {
                ""
            } else {
                ","
            };
            format!("{body}{sep}\n  {stage}\n]")
        }
        _ => format!("[\n  {stage}\n]"),
    }
}

/// A field name as a shell key: bare when it is an identifier, else
/// quoted.
pub fn shell_field(name: &str) -> String {
    let mut chars = name.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ident {
        name.to_string()
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""))
    }
}

/// The sort after a click on the header of `field`: ascending, then
/// descending, then ascending again.
pub fn toggle_sort(current: &str, field: &str) -> String {
    let asc = format!("{{{}: 1}}", shell_field(field));
    if compact(current) == compact(&asc) {
        format!("{{{}: -1}}", shell_field(field))
    } else {
        asc
    }
}

fn compact(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Sort arrow of a sort built by header clicks (`{f: 1}` / `{f: -1}`):
/// column index and ascending.
pub fn sort_indicator(sort: &str, columns: &[String]) -> Option<(usize, bool)> {
    let sort = compact(sort);
    columns.iter().enumerate().find_map(|(i, c)| {
        let f = shell_field(c);
        if sort == compact(&format!("{{{f}: 1}}")) {
            Some((i, true))
        } else if sort == compact(&format!("{{{f}: -1}}")) {
            Some((i, false))
        } else {
            None
        }
    })
}

/// A document as the JSON editor shows it: shell syntax, indented (relaxed
/// extended JSON when the printer gives nothing).
pub fn doc_text(doc: &Document) -> String {
    let shell = shell::to_shell(&Bson::Document(doc.clone()), true);
    if shell.trim().is_empty() {
        doc_json(doc)
    } else {
        shell
    }
}

/// A document as relaxed extended JSON, indented (« Copier en JSON »).
pub fn doc_json(doc: &Document) -> String {
    let json = Bson::Document(doc.clone()).into_relaxed_extjson();
    serde_json::to_string_pretty(&json).unwrap_or_default()
}

/// A copy of `doc` without its `_id` (« Dupliquer »).
pub fn without_id(doc: &Document) -> Document {
    let mut copy = doc.clone();
    copy.remove("_id");
    copy
}

/// Whether the text of a bar is a valid document (blank = `{}`).
pub fn check_bar(text: &str) -> Result<(), shell::ParseError> {
    if text.trim().is_empty() {
        Ok(())
    } else {
        shell::parse_document(text).map(|_| ())
    }
}

/// The three bars of the find mode.
const BARS: [(&str, &str); 3] = [
    ("Filtre", "{ age: { $gt: 30 } }"),
    ("Tri", "{ _id: -1 }"),
    ("Projection", "{ nom: 1 }"),
];

pub struct DocumentsTab {
    pub coll: CollectionRef,
    pub mode: DocMode,
    /// Filter, sort and projection as typed (applied on Entrée).
    pub inputs: [String; 3],
    /// The applied query (its skip / limit give the page).
    pub query: DocQuery,
    pub page_index: usize,
    pub page: Option<DocPage>,
    /// Documents matching the filter; `None` while counting.
    pub total: Option<u64>,
    pub count_error: Option<String>,
    pub grid: GridState,
    pub loading: bool,
    /// Last error (page load or submit), shown above the grid.
    pub error: Option<String>,
    /// Parse error of a bar: its index and the message.
    pub bar_error: Option<(usize, String)>,
    pub confirm: Option<Nav>,
    pub after_submit: Option<Nav>,
    pub close_after_submit: bool,
    pub needs_load: bool,
    /// Aggregation pipeline (a shell array).
    pub pipeline: String,
    pub pipeline_error: Option<String>,
    pub aggregated: Option<DocPage>,
    pub agg_grid: GridState,
    /// Column names of the page (sort arrows).
    columns: Vec<String>,
    sort: Option<(usize, bool)>,
    completions: [Completion; 4],
}

/// What the tab needs from the app to draw itself.
pub struct DocumentsContext<'a> {
    pub tab: TabId,
    pub connected: bool,
    pub submitting: bool,
    /// A page load is pending.
    pub loading: bool,
    /// An aggregation is running.
    pub aggregating: bool,
    /// Sampled fields (completion).
    pub source: &'a dyn CompletionSource,
    /// Databases → collections (completion).
    pub schemas: &'a [SchemaInfo],
}

impl DocumentsTab {
    pub fn new(coll: CollectionRef, mode: DocMode) -> Self {
        Self {
            query: DocQuery {
                coll: coll.clone(),
                filter: String::new(),
                sort: String::new(),
                projection: String::new(),
                skip: 0,
                limit: PAGE_SIZE,
            },
            coll,
            mode,
            inputs: Default::default(),
            page_index: 0,
            page: None,
            total: None,
            count_error: None,
            grid: GridState::default(),
            loading: false,
            error: None,
            bar_error: None,
            confirm: None,
            after_submit: None,
            close_after_submit: false,
            needs_load: mode == DocMode::Find,
            pipeline: add_stage("", STAGES[0].1),
            pipeline_error: None,
            aggregated: None,
            agg_grid: GridState::default(),
            columns: Vec::new(),
            sort: None,
            completions: Default::default(),
        }
    }

    /// Number of pages when the total is known.
    pub fn pages(&self) -> Option<usize> {
        self.total.map(|t| page_count(t, self.query.limit as usize))
    }

    /// Status bar text: "page 2/9 · 500 documents · 12 ms".
    pub fn summary(&self) -> Option<String> {
        let page = self.page.as_ref()?;
        let pages = self
            .pages()
            .map_or_else(|| "?".to_string(), |p| p.to_string());
        let n = page.docs.len().max(page.result.rows.len());
        Some(format!(
            "page {}/{pages} · {n} document{} · {} ms",
            self.page_index + 1,
            if n == 1 { "" } else { "s" },
            page.result.execution_time_ms
        ))
    }

    /// The page was read with a projection (not `{}`): its documents lack
    /// fields, so writing them back would lose those (`$set` of a projected
    /// sub-document, `$slice`d arrays, a Replace from the JSON editor).
    pub fn projected(&self) -> bool {
        let p = self.query.projection.trim();
        !p.is_empty() && shell::parse_document(p).map_or(true, |d| !d.is_empty())
    }

    /// Rows can be edited: a page is loaded without a projection and it
    /// shows `_id` (the key the writes find documents by).
    pub fn editable(&self) -> bool {
        !self.projected()
            && self.page.as_ref().is_some_and(|p| {
                p.result.primary_key.iter().any(|k| k == "_id")
                    || p.result.columns.iter().any(|c| c.name == "_id")
            })
    }

    /// The document of saved row `r` of the grid shown.
    pub fn doc(&self, r: RowRef) -> Option<&Document> {
        let RowRef::Base(i) = r else {
            return None;
        };
        let page = match self.mode {
            DocMode::Find => self.page.as_ref(),
            DocMode::Aggregate => self.aggregated.as_ref(),
        }?;
        page.docs.get(i)
    }

    /// The document of the focused row.
    pub fn selected_doc(&self) -> Option<&Document> {
        let grid = match self.mode {
            DocMode::Find => &self.grid,
            DocMode::Aggregate => &self.agg_grid,
        };
        self.doc(grid.selected?.0)
    }

    /// The document writes of the grid's pending edits (`mongo::doc_changes`),
    /// `None` without a page or edits.
    pub fn writes(&self) -> Option<Result<Vec<DocChange>, String>> {
        let page = self.page.as_ref()?;
        if self.grid.edits.is_empty() {
            return None;
        }
        let changes = self.grid.edits.to_row_changes(&page.result.rows);
        Some(mongo::doc_changes(page, &changes))
    }

    /// Write the open cell editor's text into the pending edits.
    pub fn commit_editor(&mut self) {
        if let Some(p) = &self.page {
            self.grid.commit_editing(&p.result.rows);
        }
    }

    /// Request `nav`, or ask for confirmation first when edits are pending.
    pub fn navigate(&mut self, nav: Nav) -> Option<DocAction> {
        self.commit_editor();
        if can_leave(&self.grid.edits) {
            self.apply(nav)
        } else {
            self.confirm = Some(nav);
            Some(DocAction::Confirm)
        }
    }

    /// "Abandonner": drop the edits and do the navigation that was asked.
    pub fn discard_and_continue(&mut self) -> Option<DocAction> {
        let nav = self.confirm.take()?;
        self.grid.discard_edits();
        self.apply(nav)
    }

    /// "Submit" in the confirmation: submit, then navigate on success.
    pub fn submit_and_continue(&mut self) -> DocAction {
        self.after_submit = self.confirm.take();
        DocAction::Submit
    }

    /// Change the query for `nav`; the load to run (`None`: a bar does not
    /// parse, its error is shown).
    fn apply(&mut self, nav: Nav) -> Option<DocAction> {
        match nav {
            Nav::Page(p) => {
                self.page_index = p;
                self.query.skip = p as u64 * u64::from(self.query.limit);
                Some(DocAction::Load { count: false })
            }
            Nav::Refresh => Some(DocAction::Load { count: true }),
            Nav::Apply => {
                for (i, text) in self.inputs.iter().enumerate() {
                    if let Err(e) = check_bar(text) {
                        self.bar_error = Some((i, e.to_string()));
                        return None;
                    }
                }
                self.bar_error = None;
                let [filter, sort, projection] = self.inputs.clone().map(|s| s.trim().to_string());
                self.query.filter = filter;
                self.query.sort = sort;
                self.query.projection = projection;
                self.page_index = 0;
                self.query.skip = 0;
                Some(DocAction::Load { count: true })
            }
            Nav::Sort(sort) => {
                self.inputs[1] = sort.clone();
                self.query.sort = sort;
                self.page_index = 0;
                self.query.skip = 0;
                Some(DocAction::Load { count: false })
            }
        }
    }

    /// Outcomes of this tab's loads, counts, aggregations and submits.
    pub fn on_event(&mut self, ev: Event) -> Option<DocAction> {
        match ev {
            Event::DocPage { outcome, .. } => {
                self.loading = false;
                match outcome {
                    Ok(page) => {
                        self.columns = page.result.columns.iter().map(|c| c.name.clone()).collect();
                        self.sort = sort_indicator(&self.query.sort, &self.columns);
                        self.page = Some(page);
                        self.grid.new_result();
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
                None
            }
            Event::Count { outcome, .. } => {
                (self.total, self.count_error) = match outcome {
                    Ok(n) => (Some(n), None),
                    Err(e) => (None, Some(e)),
                };
                None
            }
            Event::Aggregated { outcome, .. } => {
                match outcome {
                    Ok(page) => {
                        self.aggregated = Some(page);
                        self.agg_grid.new_result();
                        self.pipeline_error = None;
                    }
                    Err(e) => self.pipeline_error = Some(e),
                }
                None
            }
            Event::Submitted { outcome, .. } => match outcome {
                Ok(n) => {
                    self.grid.discard_edits();
                    self.error = None;
                    if let Some(nav) = self.after_submit.take() {
                        self.apply(nav);
                    }
                    Some(DocAction::Applied(n))
                }
                Err(e) => {
                    self.after_submit = None;
                    self.close_after_submit = false;
                    self.error = Some(format!("Submit interrompu : {e}"));
                    Some(DocAction::SubmitFailed(e))
                }
            },
            _ => None,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, cx: DocumentsContext) -> Option<DocAction> {
        if self.needs_load && cx.connected && !self.loading && self.mode == DocMode::Find {
            self.needs_load = false;
            return Some(DocAction::Load { count: true });
        }
        let mut action = self.toolbar(ui, &cx);
        if let Some(e) = &self.error {
            error_frame(ui, e);
        }
        match self.mode {
            DocMode::Find => {
                if let Some(a) = self.bars(ui, &cx) {
                    action = Some(a);
                }
                ui.separator();
                if let Some(a) = self.find_grid(ui, &cx) {
                    action = Some(a);
                }
            }
            DocMode::Aggregate => {
                if let Some(a) = self.aggregation(ui, &cx) {
                    action = Some(a);
                }
            }
        }
        action
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cx: &DocumentsContext) -> Option<DocAction> {
        let mut nav = None;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} {}", icon::FILES, self.coll)).strong());
            ui.separator();
            ui.selectable_value(&mut self.mode, DocMode::Find, "Documents");
            ui.selectable_value(&mut self.mode, DocMode::Aggregate, "Agrégation");
            ui.separator();
            if self.mode == DocMode::Aggregate {
                return;
            }
            let can = cx.connected && !cx.submitting;
            if ui
                .add_enabled(can, egui::Button::new(icon::ARROW_CLOCKWISE))
                .on_hover_text(format!(
                    "Rafraîchir ({})",
                    crate::gui::shortcuts::label_here(&crate::gui::app::REFRESH)
                ))
                .clicked()
            {
                nav = Some(Nav::Refresh);
            }
            let page = self.page_index;
            let pages = self.pages();
            let full = self.page.as_ref().is_some_and(|p| {
                p.docs.len().max(p.result.rows.len()) >= self.query.limit as usize
            });
            let has_next = pages.map_or(full, |p| page + 1 < p);
            let button = |ui: &mut egui::Ui, text: &str, enabled: bool, hint: &str| {
                ui.add_enabled(can && enabled, egui::Button::new(text))
                    .on_hover_text(hint)
                    .clicked()
            };
            if button(ui, icon::CARET_LINE_LEFT, page > 0, "Première page") {
                nav = Some(Nav::Page(0));
            }
            if button(ui, icon::CARET_LEFT, page > 0, "Page précédente") {
                nav = Some(Nav::Page(page - 1));
            }
            let total = pages.map_or_else(|| "?".to_string(), |p| p.to_string());
            let text = format!("page {} / {total}", page + 1);
            match (&self.count_error, self.total) {
                (Some(e), _) => {
                    ui.colored_label(ERROR, format!("{text} {}", icon::WARNING))
                        .on_hover_text(format!("Comptage impossible : {e}"));
                }
                (None, Some(t)) => {
                    ui.label(text).on_hover_text(format!("{t} document(s)"));
                }
                (None, None) => {
                    ui.label(text).on_hover_text("Comptage…");
                }
            }
            if button(ui, icon::CARET_RIGHT, has_next, "Page suivante") {
                nav = Some(Nav::Page(page + 1));
            }
            let last = pages.filter(|&p| page + 1 < p);
            if button(ui, icon::CARET_LINE_RIGHT, last.is_some(), "Dernière page") {
                if let Some(p) = last {
                    nav = Some(Nav::Page(p - 1));
                }
            }
            ui.label(RichText::new(format!("{} documents/page", self.query.limit)).weak());
            if cx.loading || cx.submitting {
                ui.spinner();
            }
            ui.separator();
            let selected = self.grid.selected.and_then(|(r, _)| match r {
                RowRef::Base(i) => Some(i),
                RowRef::New(_) => None,
            });
            let can_write = can && self.page.is_some();
            if ui
                .add_enabled(
                    can_write,
                    egui::Button::new(format!("{} Document", icon::PLUS)),
                )
                .on_hover_text("Nouveau document (éditeur JSON)")
                .clicked()
            {
                action = Some(DocAction::Edit(None));
            }
            // A projected document is partial: no editor, copy or delete.
            let with_doc = can_write
                && !self.projected()
                && selected.is_some_and(|i| self.doc(RowRef::Base(i)).is_some());
            if ui
                .add_enabled(
                    with_doc,
                    egui::Button::new(format!("{} Éditer le document…", icon::PENCIL_SIMPLE)),
                )
                .clicked()
            {
                action = selected.map(|i| DocAction::Edit(Some(i)));
            }
            if ui
                .add_enabled(with_doc, egui::Button::new(icon::COPY_SIMPLE))
                .on_hover_text("Dupliquer le document")
                .clicked()
            {
                action = selected.map(DocAction::Duplicate);
            }
            if ui
                .add_enabled(with_doc, egui::Button::new(icon::TRASH))
                .on_hover_text("Supprimer le document (au prochain Submit)")
                .clicked()
            {
                if let Some(i) = selected {
                    self.grid.edits.toggle_delete(RowRef::Base(i));
                }
            }
            if ui
                .add_enabled(
                    self.selected_doc().is_some(),
                    egui::Button::new(icon::BRACKETS_CURLY),
                )
                .on_hover_text("Copier le document en JSON")
                .clicked()
            {
                if let Some(doc) = self.selected_doc() {
                    ui.ctx().copy_text(doc_json(doc));
                }
            }
        });
        match nav {
            Some(nav) => self.navigate(nav),
            None => action,
        }
    }

    /// Filtre / Tri / Projection; Entrée applies them (it accepts the
    /// suggestion instead while a completion popup is open).
    fn bars(&mut self, ui: &mut egui::Ui, cx: &DocumentsContext) -> Option<DocAction> {
        let call = collection_call(&self.coll);
        let prefixes = [
            format!("{call}.find("),
            format!("{call}.find({{}}).sort("),
            format!("{call}.find({{}}, "),
        ];
        let scope = NoSqlScope {
            database: self.coll.db.clone(),
        };
        let mut apply = false;
        egui::Grid::new(("doc_bars", cx.tab))
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for (i, (label, hint)) in BARS.iter().enumerate() {
                    let id = egui::Id::new(("doc_bar", cx.tab, i));
                    ui.label(RichText::new(*label).strong());
                    let text = &mut self.inputs[i];
                    let completion = &mut self.completions[i];
                    let triggered = completion.intercept_keys(ui, text, id);
                    let out = egui::TextEdit::singleline(text)
                        .id(id)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .hint_text(*hint)
                        .show(ui);
                    apply |= out.response.lost_focus()
                        && ui.input_mut(|inp| {
                            let modifiers = inp.modifiers;
                            inp.consume_key(modifiers, Key::Enter)
                        });
                    let nosql = completion::NoSqlScope {
                        db: &DatabaseType::MongoDB,
                        scope: &scope,
                        schemas: cx.schemas,
                        keys: &[],
                        prefix: &prefixes[i],
                    };
                    completion.after_edit(
                        ui,
                        &out,
                        text,
                        id,
                        cx.source,
                        Scope::NoSql(nosql),
                        triggered,
                    );
                    ui.end_row();
                    if let Some((_, e)) = self.bar_error.as_ref().filter(|(b, _)| *b == i) {
                        ui.label("");
                        ui.colored_label(ERROR, e);
                        ui.end_row();
                    }
                }
            });
        let mut action = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    cx.connected,
                    egui::Button::new(format!("{} Appliquer", icon::FUNNEL)),
                )
                .on_hover_text("Appliquer le filtre, le tri et la projection (Entrée)")
                .clicked()
            {
                apply = true;
            }
            if ui
                .button(format!(
                    "{} Copier comme commande console",
                    icon::TERMINAL_WINDOW
                ))
                .clicked()
            {
                let [f, s, p] = &self.inputs;
                ui.ctx().copy_text(find_command(&self.coll, f, s, p));
            }
            if ui
                .add_enabled(
                    cx.connected,
                    egui::Button::new(format!("{} Exporter…", icon::EXPORT)),
                )
                .clicked()
            {
                action = Some(DocAction::Export);
            }
        });
        if apply && cx.connected && !cx.submitting {
            return self.navigate(Nav::Apply);
        }
        action
    }

    fn find_grid(&mut self, ui: &mut egui::Ui, cx: &DocumentsContext) -> Option<DocAction> {
        let busy = busy_hint(cx.submitting, cx.loading);
        if self.projected() && self.page.is_some() {
            ui.label(
                RichText::new(format!(
                    "{} {} — videz « Projection » pour modifier",
                    icon::LOCK,
                    crate::engine::nosql::PROJECTION_READ_ONLY
                ))
                .weak(),
            );
        }
        if let (Some(hint), true) = (busy, self.editable()) {
            ui.label(RichText::new(format!("{} {hint}", icon::HOURGLASS)).weak());
        }
        let editable = self.editable() && busy.is_none();
        let Some(page) = &self.page else {
            if cx.loading {
                ui.spinner();
            }
            return None;
        };
        let grid_action = grid::show(
            ui,
            egui::Id::new(("doc_grid", cx.tab)),
            &mut self.grid,
            GridOptions {
                result: &page.result,
                editable,
                server_sort: true,
                sort_indicator: self.sort,
                links: &[],
                incoming: &[],
            },
        );
        match grid_action {
            GridAction::SortBy(col) => {
                let name = self.columns.get(col)?.clone();
                let sort = toggle_sort(&self.query.sort, &name);
                self.navigate(Nav::Sort(sort))
            }
            GridAction::Submit if !self.grid.edits.is_empty() && !cx.submitting => {
                Some(DocAction::Submit)
            }
            GridAction::Export => Some(DocAction::Export),
            _ => None,
        }
    }

    fn aggregation(&mut self, ui: &mut egui::Ui, cx: &DocumentsContext) -> Option<DocAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.menu_button(format!("{} Ajouter une étape", icon::PLUS), |ui| {
                for (name, stage) in STAGES {
                    if ui.button(name).clicked() {
                        self.pipeline = add_stage(&self.pipeline, stage);
                        ui.close_menu();
                    }
                }
            });
            if ui
                .add_enabled(
                    cx.connected && !cx.aggregating,
                    egui::Button::new(format!("{} Exécuter", icon::PLAY)),
                )
                .clicked()
            {
                action = Some(DocAction::Aggregate);
            }
            if cx.aggregating {
                ui.spinner();
            }
            if ui
                .button(format!(
                    "{} Copier comme commande console",
                    icon::TERMINAL_WINDOW
                ))
                .clicked()
            {
                ui.ctx()
                    .copy_text(aggregate_command(&self.coll, &self.pipeline));
            }
        });
        let id = egui::Id::new(("doc_pipeline", cx.tab));
        let call = collection_call(&self.coll);
        let prefix = format!("{call}.aggregate(");
        let scope = NoSqlScope {
            database: self.coll.db.clone(),
        };
        let completion = &mut self.completions[3];
        let triggered = completion.intercept_keys(ui, &mut self.pipeline, id);
        let out = egui::ScrollArea::vertical()
            .id_salt(id.with("scroll"))
            .max_height(220.0)
            .show(ui, |ui| {
                egui::TextEdit::multiline(&mut self.pipeline)
                    .id(id)
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(8)
                    .hint_text("[ { $match: {} } ]")
                    .show(ui)
            })
            .inner;
        let nosql = completion::NoSqlScope {
            db: &DatabaseType::MongoDB,
            scope: &scope,
            schemas: cx.schemas,
            keys: &[],
            prefix: &prefix,
        };
        completion.after_edit(
            ui,
            &out,
            &mut self.pipeline,
            id,
            cx.source,
            Scope::NoSql(nosql),
            triggered,
        );
        if let Some(e) = &self.pipeline_error {
            error_frame(ui, e);
        }
        ui.separator();
        match &self.aggregated {
            Some(page) => {
                ui.label(RichText::new(format!("{} Résultat en lecture seule", icon::LOCK)).weak());
                let grid_action = grid::show(
                    ui,
                    egui::Id::new(("doc_agg_grid", cx.tab)),
                    &mut self.agg_grid,
                    GridOptions {
                        result: &page.result,
                        editable: false,
                        server_sort: false,
                        sort_indicator: None,
                        links: &[],
                        incoming: &[],
                    },
                );
                if grid_action == GridAction::Export {
                    action = Some(DocAction::Export);
                }
            }
            None => {
                ui.label(RichText::new("Exécutez le pipeline pour voir son résultat.").weak());
            }
        }
        action
    }

    /// Fields whose sampled columns the bars' completion needs (taken once).
    pub fn take_completion_requests(&mut self) -> Vec<crate::engine::sql::completion::TableRef> {
        let mut out = Vec::new();
        for c in &mut self.completions {
            for t in c.take_requests() {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }
}

fn error_frame(ui: &mut egui::Ui, e: &str) {
    egui::Frame::new()
        .fill(ERROR.gamma_multiply(0.18))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            one_line_label(ui, &format!("{} {e}", icon::WARNING), ERROR);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{Column, QueryResult};

    fn coll() -> CollectionRef {
        CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        }
    }

    fn page(n: usize) -> DocPage {
        let docs: Vec<Document> = (0..n)
            .map(|i| bson::doc! { "_id": i as i32, "name": format!("n{i}") })
            .collect();
        let column = |name: &str| Column {
            name: name.into(),
            type_name: "string".into(),
            nullable: true,
            is_primary_key: name == "_id",
        };
        DocPage {
            result: QueryResult {
                columns: vec![column("_id"), column("name")],
                rows: (0..n)
                    .map(|i| vec![i.to_string(), format!("n{i}")])
                    .collect(),
                primary_key: vec!["_id".into()],
                ..Default::default()
            },
            docs,
        }
    }

    fn loaded() -> DocumentsTab {
        let mut t = DocumentsTab::new(coll(), DocMode::Find);
        t.on_event(Event::DocPage {
            tab: 1,
            run: 1,
            outcome: Ok(page(3)),
        });
        t
    }

    #[test]
    fn console_commands_quote_the_collection() {
        let c = CollectionRef {
            db: "shop".into(),
            coll: "fs.chunks".into(),
        };
        assert_eq!(
            find_command(&c, "", "", ""),
            r#"db.getCollection("fs.chunks").find({})"#
        );
        assert_eq!(
            find_command(&c, "{a: 1}", "{b: -1}", "{a: 1}"),
            r#"db.getCollection("fs.chunks").find({a: 1}, {a: 1}).sort({b: -1})"#
        );
        assert_eq!(
            aggregate_command(&c, " "),
            r#"db.getCollection("fs.chunks").aggregate([])"#
        );
    }

    #[test]
    fn stages_are_appended_to_the_pipeline() {
        assert_eq!(add_stage("", "{ $limit: 1 }"), "[\n  { $limit: 1 }\n]");
        assert_eq!(add_stage("[]", "{ $limit: 1 }"), "[\n  { $limit: 1 }\n]");
        let one = add_stage("", "{ $match: {} }");
        assert_eq!(
            add_stage(&one, "{ $limit: 1 }"),
            "[\n  { $match: {} },\n  { $limit: 1 }\n]"
        );
        // Not a pipeline: a new one.
        assert_eq!(
            add_stage("oops", "{ $count: \"n\" }"),
            "[\n  { $count: \"n\" }\n]"
        );
    }

    #[test]
    fn header_clicks_toggle_the_sort() {
        assert_eq!(toggle_sort("", "age"), "{age: 1}");
        assert_eq!(toggle_sort("{ age : 1 }", "age"), "{age: -1}");
        assert_eq!(toggle_sort("{age: -1}", "age"), "{age: 1}");
        assert_eq!(toggle_sort("{age: 1}", "a b"), "{\"a b\": 1}");
        let cols = vec!["_id".to_string(), "a b".to_string()];
        assert_eq!(sort_indicator("{_id: -1}", &cols), Some((0, false)));
        assert_eq!(sort_indicator("{\"a b\": 1}", &cols), Some((1, true)));
        assert_eq!(sort_indicator("{_id: 1, x: 1}", &cols), None);
    }

    #[test]
    fn pages_skip_whole_pages_and_bars_reset_to_the_first() {
        let mut t = loaded();
        assert_eq!(
            t.navigate(Nav::Page(2)),
            Some(DocAction::Load { count: false })
        );
        assert_eq!((t.page_index, t.query.skip), (2, 1000));
        // Blank bars are `{}` and always valid.
        assert_eq!(
            t.navigate(Nav::Apply),
            Some(DocAction::Load { count: true })
        );
        assert_eq!((t.page_index, t.query.skip), (0, 0));
        assert_eq!(t.bar_error, None);
    }

    #[test]
    fn a_bar_that_does_not_parse_blocks_the_load() {
        let mut t = loaded();
        t.inputs[0] = "{ age: ".into();
        // Whatever the parser says, an error keeps the applied query.
        match shell::parse_document("{ age: ") {
            Ok(_) => {}
            Err(_) => {
                assert_eq!(t.navigate(Nav::Apply), None);
                assert_eq!(t.bar_error.as_ref().map(|e| e.0), Some(0));
                assert_eq!(t.query.filter, "");
            }
        }
    }

    #[test]
    fn a_projection_makes_the_page_read_only() {
        let mut t = loaded();
        assert!(!t.projected() && t.editable());
        t.query.projection = "{name: 1}".into();
        assert!(t.projected());
        assert!(!t.editable(), "hidden fields would be lost");
        t.query.projection = " { } ".into();
        assert!(!t.projected() && t.editable(), "{{}} projects nothing");
    }

    #[test]
    fn edits_need_confirmation_and_submit_results_are_reported() {
        let mut t = loaded();
        assert!(t.editable());
        let rows = t.page.as_ref().unwrap().result.rows.clone();
        t.grid.edits.set(&rows, RowRef::Base(0), 1, "x".into());
        assert_eq!(t.navigate(Nav::Page(1)), Some(DocAction::Confirm));
        assert_eq!(t.submit_and_continue(), DocAction::Submit);
        let failed = t.on_event(Event::Submitted {
            tab: 1,
            run: 2,
            outcome: Err("document introuvable".into()),
        });
        assert_eq!(
            failed,
            Some(DocAction::SubmitFailed("document introuvable".into()))
        );
        assert!(!t.grid.edits.is_empty(), "edits kept");
        assert_eq!(t.page_index, 0);
        let ok = t.on_event(Event::Submitted {
            tab: 1,
            run: 3,
            outcome: Ok(1),
        });
        assert_eq!(ok, Some(DocAction::Applied(1)));
        assert!(t.grid.edits.is_empty());
    }

    #[test]
    fn grid_edits_become_the_engine_s_document_writes() {
        let mut t = loaded();
        assert_eq!(t.writes(), None, "nothing to write");
        let rows = t.page.as_ref().unwrap().result.rows.clone();
        t.grid
            .edits
            .set(&rows, RowRef::Base(1), 1, "renamed".into());
        t.grid.edits.toggle_delete(RowRef::Base(2));
        // Exactly what the engine maps the pending edits to (by `_id`).
        let expected = crate::engine::ops::rows::RowChanges {
            updates: vec![(rows[1].clone(), vec![rows[1][0].clone(), "renamed".into()])],
            inserts: vec![],
            deletes: vec![rows[2].clone()],
        };
        assert_eq!(
            t.writes(),
            Some(mongo::doc_changes(t.page.as_ref().unwrap(), &expected))
        );
    }

    #[test]
    fn the_selected_row_gives_its_document() {
        let mut t = loaded();
        assert_eq!(t.selected_doc(), None);
        t.grid.selected = Some((RowRef::Base(2), 1));
        assert_eq!(
            t.selected_doc().and_then(|d| d.get_i32("_id").ok()),
            Some(2)
        );
        t.grid.selected = Some((RowRef::New(0), 1));
        assert_eq!(t.selected_doc(), None);
        assert_eq!(t.summary().unwrap(), "page 1/? · 3 documents · 0 ms");
        assert!(!without_id(&t.page.as_ref().unwrap().docs[0]).contains_key("_id"));
    }

    #[test]
    fn copied_json_is_relaxed_extended_json() {
        let id = bson::oid::ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
        let doc = bson::doc! { "_id": id, "n": 1 };
        let json: serde_json::Value = serde_json::from_str(&doc_json(&doc)).unwrap();
        assert_eq!(json["_id"]["$oid"], "65a1b2c3d4e5f60718293a4b");
        assert_eq!(json["n"], 1);
        assert!(!doc_text(&doc).is_empty());
    }

    #[test]
    fn aggregation_results_are_kept_apart() {
        let mut t = DocumentsTab::new(coll(), DocMode::Aggregate);
        assert!(!t.needs_load, "nothing loads before Exécuter");
        t.on_event(Event::Aggregated {
            tab: 1,
            run: 1,
            outcome: Err("unknown stage".into()),
        });
        assert_eq!(t.pipeline_error.as_deref(), Some("unknown stage"));
        t.on_event(Event::Aggregated {
            tab: 1,
            run: 2,
            outcome: Ok(page(2)),
        });
        assert_eq!(t.pipeline_error, None);
        t.agg_grid.selected = Some((RowRef::Base(1), 0));
        assert!(t.selected_doc().is_some());
        assert!(t.page.is_none(), "the find page is untouched");
    }
}
