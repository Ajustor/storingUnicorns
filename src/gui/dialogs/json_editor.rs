//! JSON editor of a whole MongoDB document (« + Document », « Éditer le
//! document… », « Dupliquer »): shell syntax, checked as it is typed.

use bson::{Bson, Document};
use egui::RichText;
use egui_phosphor::regular as icon;

use crate::engine::nosql::mongo::shell::{self, ParseError};
use crate::engine::nosql::mongo::{CollectionRef, DocChange};
use crate::gui::modal::{self, MessageKind, ModalButton, ModalResponse, ModalSize, ModalSpec};
use crate::gui::tabs::documents::doc_text;
use crate::gui::worker::TabId;

/// What saving the editor does.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonTarget {
    /// Replace the document with this `_id`.
    Replace(Bson),
    /// Insert a new document.
    Insert,
}

pub struct JsonEditor {
    pub connection: String,
    /// The « Documents » tab the write reports to.
    pub tab: TabId,
    pub coll: CollectionRef,
    pub target: JsonTarget,
    pub text: String,
    /// The parse error of `checked`.
    pub error: Option<ParseError>,
    /// Why the last save was refused (another `_id`…), until the text
    /// changes.
    pub refused: Option<String>,
    /// The text last checked (it is checked again when it changes).
    checked: Option<String>,
}

/// Line and column (1-based) of byte `offset` of `text`.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

impl JsonEditor {
    /// Edit `doc` (its `_id` identifies it).
    pub fn edit(connection: &str, tab: TabId, coll: CollectionRef, doc: &Document) -> Self {
        let target = match doc.get("_id") {
            Some(id) => JsonTarget::Replace(id.clone()),
            None => JsonTarget::Insert,
        };
        Self::new(connection, tab, coll, target, doc_text(doc))
    }

    /// A new document, starting from `doc` (a copy, or an empty one).
    pub fn insert(connection: &str, tab: TabId, coll: CollectionRef, doc: &Document) -> Self {
        let text = if doc.is_empty() {
            "{\n  \n}".to_string()
        } else {
            doc_text(doc)
        };
        Self::new(connection, tab, coll, JsonTarget::Insert, text)
    }

    fn new(
        connection: &str,
        tab: TabId,
        coll: CollectionRef,
        target: JsonTarget,
        text: String,
    ) -> Self {
        Self {
            connection: connection.to_string(),
            tab,
            coll,
            target,
            text,
            error: None,
            refused: None,
            checked: None,
        }
    }

    /// Check the text when it changed since the last check.
    pub fn check(&mut self) {
        if self.checked.as_deref() == Some(self.text.as_str()) {
            return;
        }
        self.error = shell::parse_document(&self.text).err();
        self.refused = None;
        self.checked = Some(self.text.clone());
    }

    /// The write saving the editor does; the error to show otherwise.
    pub fn change(&self) -> Result<DocChange, String> {
        let mut doc = shell::parse_document(&self.text).map_err(|e| self.describe(&e))?;
        match &self.target {
            JsonTarget::Insert => Ok(DocChange::Insert(doc)),
            JsonTarget::Replace(id) => {
                match doc.get("_id") {
                    None => {
                        doc.insert("_id", id.clone());
                    }
                    Some(new) if new != id => {
                        return Err(
                            "_id ne peut pas changer : dupliquez le document pour en créer un autre"
                                .into(),
                        );
                    }
                    Some(_) => {}
                }
                Ok(DocChange::Replace {
                    id: id.clone(),
                    doc,
                })
            }
        }
    }

    fn describe(&self, e: &ParseError) -> String {
        let (line, col) = line_col(&self.text, e.offset);
        format!("{} (ligne {line}, colonne {col})", e.message)
    }

    /// Draw the editor in the standard modal; `()` saves it.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> ModalResponse<()> {
        self.check();
        let title = match self.target {
            JsonTarget::Insert => "Nouveau document",
            JsonTarget::Replace(_) => "Modifier le document",
        };
        let message = self
            .error
            .as_ref()
            .map(|e| self.describe(e))
            .or_else(|| self.refused.clone());
        let save = ModalButton::primary("Enregistrer", ()).enabled(self.error.is_none());
        let save = match &message {
            Some(m) => save.hint(m.clone()),
            None => save,
        };
        modal::show(
            ui,
            ModalSpec::new(title, ModalSize::Large).icon(icon::BRACKETS_CURLY),
            |ui| {
                ui.label(RichText::new(self.coll.to_string()).weak().small());
                ui.label(
                    RichText::new(
                        "Syntaxe du shell MongoDB : ObjectId(\"…\"), ISODate(\"…\"), \
                         NumberLong(…)… ou JSON étendu.",
                    )
                    .weak()
                    .small(),
                );
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(18),
                );
            },
            message.as_deref().map(|m| (MessageKind::Error, m)),
            std::slice::from_ref(&save),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coll() -> CollectionRef {
        CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        }
    }

    #[test]
    fn errors_are_placed_by_line_and_column() {
        let text = "{\n  a: 1,\n  b: é}";
        assert_eq!(line_col(text, 0), (1, 1));
        assert_eq!(line_col(text, 4), (2, 3));
        assert_eq!(line_col(text, text.len()), (3, 8));
        assert_eq!(line_col(text, 999), (3, 8));
    }

    #[test]
    fn editing_targets_the_document_s_id() {
        let doc = bson::doc! { "_id": 7, "a": 1 };
        let e = JsonEditor::edit("m", 1, coll(), &doc);
        assert_eq!(e.target, JsonTarget::Replace(Bson::Int32(7)));
        assert!(!e.text.is_empty());
        let new = JsonEditor::insert("m", 1, coll(), &Document::new());
        assert_eq!(new.target, JsonTarget::Insert);
        assert!(new.text.starts_with('{'));
    }

    #[test]
    fn the_id_of_a_replaced_document_is_kept() {
        let mut e = JsonEditor::edit("m", 1, coll(), &bson::doc! { "_id": 7 });
        // With the shell parser, whatever it accepts: a missing `_id` is
        // put back, another one is refused.
        e.text = "{ a: 2 }".into();
        if let Ok(DocChange::Replace { id, doc }) = e.change() {
            assert_eq!(id, Bson::Int32(7));
            assert_eq!(doc.get("_id"), Some(&Bson::Int32(7)));
        }
        e.text = "{ _id: 8 }".into();
        assert!(e.change().is_err());
    }

    #[test]
    fn the_text_is_checked_once_per_change() {
        let mut e = JsonEditor::insert("m", 1, coll(), &Document::new());
        e.text = "{ a: ".into();
        e.check();
        let first = e.error.clone();
        e.check();
        assert_eq!(e.error, first);
        assert_eq!(e.checked.as_deref(), Some("{ a: "));
    }
}
