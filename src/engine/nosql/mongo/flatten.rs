//! Documents as a grid (plan 13, decision 4) and back: cells, sampled
//! fields, grid edits → MongoDB writes. Pure functions, no I/O.

use bson::{Bson, Document};

use super::shell::{self, fmt_double, iso_date, parse_value, to_shell, MongoCommand, UpdateSpec};
use super::{CollectionRef, DocChange, DocPage, MongoIndex};
use crate::engine::models::{Column, IndexInfo, QueryResult, NULL_CELL};
use crate::engine::nosql::{is_absent, ABSENT_CELL};
use crate::engine::ops::rows::RowChanges;

/// The type of a value, as shown in column details ("string|int").
pub(crate) fn type_label(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => "javascript",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Symbol(_) => "symbol",
        Bson::Decimal128(_) => "decimal",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "maxKey",
        Bson::MinKey => "minKey",
        Bson::DbPointer(_) => "dbPointer",
    }
}

/// The text of a grid cell: plain text for strings, numbers, booleans and
/// dates (ISO 8601), compact shell syntax for the rest, `NULL_CELL` for null.
pub fn cell_text(v: &Bson) -> String {
    match v {
        Bson::Null => NULL_CELL.to_string(),
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(f) => fmt_double(*f),
        Bson::Decimal128(d) => d.to_string(),
        Bson::Boolean(b) => b.to_string(),
        Bson::DateTime(d) => iso_date(*d).unwrap_or_else(|| to_shell(v, false)),
        other => to_shell(other, false),
    }
}

/// Types seen for one field, in first-seen order.
#[derive(Default)]
struct FieldStats {
    types: Vec<&'static str>,
    /// Documents where the field holds a non-null value.
    present: usize,
}

impl FieldStats {
    fn add(&mut self, v: &Bson) {
        let t = type_label(v);
        if !self.types.contains(&t) {
            self.types.push(t);
        }
        if !matches!(v, Bson::Null) {
            self.present += 1;
        }
    }

    fn type_name(&self) -> String {
        let types: Vec<&str> = self
            .types
            .iter()
            .copied()
            .filter(|t| *t != "null")
            .collect();
        if types.is_empty() {
            "null".into()
        } else {
            types.join("|")
        }
    }
}

/// Ordered field statistics, `_id` first.
#[derive(Default)]
struct Fields {
    order: Vec<String>,
    stats: std::collections::HashMap<String, FieldStats>,
}

impl Fields {
    fn add(&mut self, name: &str, v: &Bson) {
        if !self.stats.contains_key(name) {
            if name == "_id" {
                self.order.insert(0, name.to_string());
            } else {
                self.order.push(name.to_string());
            }
        }
        self.stats.entry(name.to_string()).or_default().add(v);
    }

    fn columns(&self, total: usize) -> Vec<Column> {
        self.order
            .iter()
            .map(|name| {
                let s = &self.stats[name];
                let id = name == "_id";
                Column {
                    name: name.clone(),
                    type_name: s.type_name(),
                    nullable: !id && s.present < total,
                    is_primary_key: id,
                }
            })
            .collect()
    }
}

/// Grid of documents: top-level fields as columns (`_id` first, then by
/// first appearance), `NULL_CELL` for null, `ABSENT_CELL` for a missing
/// field; `_id` is the primary key.
pub fn flatten(docs: &[Document]) -> QueryResult {
    let mut fields = Fields::default();
    for doc in docs {
        for (k, v) in doc {
            fields.add(k, v);
        }
    }
    let columns = fields.columns(docs.len());
    let rows = docs
        .iter()
        .map(|doc| {
            columns
                .iter()
                .map(|c| {
                    doc.get(&c.name)
                        .map_or_else(|| ABSENT_CELL.to_string(), cell_text)
                })
                .collect()
        })
        .collect();
    let primary_key = if fields.stats.contains_key("_id") {
        vec!["_id".to_string()]
    } else {
        Vec::new()
    };
    QueryResult {
        columns,
        rows,
        primary_key,
        ..Default::default()
    }
}

/// Maximum depth of the sampled dotted paths (`a.b.c`).
const MAX_DEPTH: usize = 3;

/// Fields of sampled documents: dotted paths up to 3 levels (objects and
/// their fields), type "string|int", nullable when missing or null in
/// some document, `_id` as the primary key.
pub fn fields_of(docs: &[Document]) -> Vec<Column> {
    fn walk(fields: &mut Fields, doc: &Document, prefix: &str, depth: usize) {
        for (k, v) in doc {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            fields.add(&path, v);
            if let Bson::Document(sub) = v {
                if depth < MAX_DEPTH {
                    walk(fields, sub, &path, depth + 1);
                }
            }
        }
    }
    let mut fields = Fields::default();
    for doc in docs {
        walk(&mut fields, doc, "", 1);
    }
    fields.columns(docs.len())
}

/// The value typed into a cell, keeping the type of `original` when the
/// text reads as that type; else shell syntax (`NumberLong(5)`,
/// `{a: 1}`…). A new field (`original` = `None` or null) that isn't valid
/// shell syntax is a string. `Ok(None)` = remove the field (`$unset`).
pub fn cell_to_bson(original: Option<&Bson>, text: &str) -> Result<Option<Bson>, String> {
    if is_absent(text) {
        return Ok(None);
    }
    if text == NULL_CELL {
        return Ok(Some(Bson::Null));
    }
    let t = text.trim();
    let typed = match original {
        Some(Bson::String(_)) => return Ok(Some(Bson::String(text.to_string()))),
        Some(Bson::Int32(_)) => t.parse::<i32>().ok().map(Bson::Int32),
        Some(Bson::Int64(_)) => t.parse::<i64>().ok().map(Bson::Int64),
        Some(Bson::Double(_)) => match t {
            "NaN" => Some(Bson::Double(f64::NAN)),
            "Infinity" => Some(Bson::Double(f64::INFINITY)),
            "-Infinity" => Some(Bson::Double(f64::NEG_INFINITY)),
            _ => t.parse::<f64>().ok().map(Bson::Double),
        },
        Some(Bson::Decimal128(_)) => t.parse::<bson::Decimal128>().ok().map(Bson::Decimal128),
        Some(Bson::Boolean(_)) => match t {
            "true" => Some(Bson::Boolean(true)),
            "false" => Some(Bson::Boolean(false)),
            _ => None,
        },
        Some(Bson::DateTime(_)) => shell::parse_iso_date(t).map(Bson::DateTime),
        Some(Bson::ObjectId(_)) => bson::oid::ObjectId::parse_str(t).ok().map(Bson::ObjectId),
        _ => None,
    };
    if let Some(v) = typed {
        return Ok(Some(v));
    }
    match parse_value(text) {
        Ok(v) => Ok(Some(v)),
        Err(_) if matches!(original, None | Some(Bson::Null)) => {
            Ok(Some(Bson::String(text.to_string())))
        }
        Err(e) => Err(format!(
            "valeur invalide pour un champ {} : {e}",
            original.map_or("", type_label)
        )),
    }
}

/// The document of a grid row: the page's row equal to `row` (all its
/// cells), the document at that position. Not by the `_id` text alone:
/// `"1"`, `1` and `NumberLong(1)` show alike. Two equal rows: refused.
fn row_doc<'a>(page: &'a DocPage, id_col: usize, row: &[String]) -> Result<&'a Document, String> {
    let id = row.get(id_col).ok_or("ligne incomplète")?;
    let mut found = page
        .result
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.as_slice() == row)
        .map(|(i, _)| i);
    match (found.next(), found.next()) {
        (Some(i), None) => page
            .docs
            .get(i)
            .ok_or_else(|| format!("document introuvable (_id {id})")),
        (Some(_), Some(_)) => Err(format!(
            "plusieurs documents s'affichent à l'identique (_id {id}) :              modifier par la console ou l'éditeur JSON"
        )),
        (None, _) => Err(format!("document introuvable (_id {id})")),
    }
}

/// The writes of the grid's pending edits (rows found by `_id`): changed
/// cells → `Set` / `Unset`, new rows → `Insert`, deleted rows → `Delete`.
pub fn doc_changes(page: &DocPage, changes: &RowChanges) -> Result<Vec<DocChange>, String> {
    let columns = &page.result.columns;
    let id_col = columns.iter().position(|c| c.name == "_id");
    let need_id =
        || id_col.ok_or_else(|| "pas de colonne _id : modification impossible".to_string());
    let mut out = Vec::new();
    for (original, edited) in &changes.updates {
        let col = need_id()?;
        let doc = row_doc(page, col, original)?;
        let id = doc.get("_id").cloned().unwrap_or(Bson::Null);
        for (i, c) in columns.iter().enumerate() {
            let (Some(before), Some(after)) = (original.get(i), edited.get(i)) else {
                continue;
            };
            if before == after {
                continue;
            }
            if c.name == "_id" {
                return Err("_id n'est pas modifiable".into());
            }
            let current = doc.get(&c.name);
            match cell_to_bson(current, after).map_err(|e| format!("{} : {e}", c.name))? {
                Some(value) => out.push(DocChange::Set {
                    id: id.clone(),
                    field: c.name.clone(),
                    value,
                }),
                None if current.is_some() => out.push(DocChange::Unset {
                    id: id.clone(),
                    field: c.name.clone(),
                }),
                None => {}
            }
        }
    }
    for row in &changes.inserts {
        let mut doc = Document::new();
        for (c, cell) in columns.iter().zip(row) {
            let Some(text) = cell else { continue };
            let value = if c.name == "_id" && is_object_id_hex(text.trim()) {
                bson::oid::ObjectId::parse_str(text.trim())
                    .ok()
                    .map(Bson::ObjectId)
            } else {
                cell_to_bson(None, text).map_err(|e| format!("{} : {e}", c.name))?
            };
            if let Some(v) = value {
                doc.insert(c.name.clone(), v);
            }
        }
        out.push(DocChange::Insert(doc));
    }
    for row in &changes.deletes {
        let doc = row_doc(page, need_id()?, row)?;
        out.push(DocChange::Delete {
            id: doc.get("_id").cloned().unwrap_or(Bson::Null),
        });
    }
    Ok(out)
}

pub(crate) fn is_object_id_hex(s: &str) -> bool {
    s.len() == 24 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The console command of a change.
pub(crate) fn change_command(coll: &CollectionRef, c: &DocChange) -> MongoCommand {
    let by_id = |id: &Bson| bson::doc! { "_id": id.clone() };
    let coll = coll.coll.clone();
    match c {
        DocChange::Set { id, field, value } => MongoCommand::Update {
            coll,
            filter: by_id(id),
            update: UpdateSpec::Document(bson::doc! { "$set": { field.clone(): value.clone() } }),
            many: false,
            upsert: false,
        },
        DocChange::Unset { id, field } => MongoCommand::Update {
            coll,
            filter: by_id(id),
            update: UpdateSpec::Document(bson::doc! { "$unset": { field.clone(): "" } }),
            many: false,
            upsert: false,
        },
        DocChange::Replace { id, doc } => MongoCommand::ReplaceOne {
            coll,
            filter: by_id(id),
            replacement: doc.clone(),
            upsert: false,
        },
        DocChange::Insert(doc) => MongoCommand::InsertOne {
            coll,
            doc: doc.clone(),
        },
        DocChange::Delete { id } => MongoCommand::Delete {
            coll,
            filter: by_id(id),
            many: false,
        },
    }
}

/// The shell command of a change (preview, TUI debug mode).
pub fn change_to_shell(coll: &CollectionRef, c: &DocChange) -> String {
    shell::command_to_shell(&change_command(coll, c))
}

fn truthy(v: Option<&Bson>) -> bool {
    match v {
        Some(Bson::Boolean(b)) => *b,
        Some(Bson::Int32(i)) => *i != 0,
        Some(Bson::Int64(i)) => *i != 0,
        Some(Bson::Double(f)) => *f != 0.0,
        _ => false,
    }
}

fn as_i64(v: Option<&Bson>) -> Option<i64> {
    match v? {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        Bson::Double(f) => Some(*f as i64),
        _ => None,
    }
}

/// An index from its `listIndexes` description.
pub(crate) fn index_from_doc(d: &Document) -> MongoIndex {
    MongoIndex {
        name: d.get_str("name").unwrap_or_default().to_string(),
        keys: d.get_document("key").cloned().unwrap_or_default(),
        unique: truthy(d.get("unique")),
        sparse: truthy(d.get("sparse")),
        ttl: as_i64(d.get("expireAfterSeconds")),
        partial: d.get_document("partialFilterExpression").ok().cloned(),
        hidden: truthy(d.get("hidden")),
    }
}

/// The explorer's view of an index: columns "a: 1", primary = `_id_`.
pub fn index_info(i: &MongoIndex) -> IndexInfo {
    let primary = i.name == "_id_";
    IndexInfo {
        name: i.name.clone(),
        columns: i
            .keys
            .iter()
            .map(|(k, v)| format!("{k}: {}", to_shell(v, false)))
            .collect(),
        unique: i.unique || primary,
        primary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId};

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_bytes([n; 12])
    }

    #[test]
    fn flatten_orders_columns_and_tells_absent_from_null() {
        let docs = vec![
            doc! { "name": "a", "_id": oid(1), "n": 1 },
            doc! { "_id": oid(2), "n": Bson::Null, "tags": ["x", 2], "sub": { "a": 1 } },
        ];
        let r = flatten(&docs);
        let names: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["_id", "name", "n", "tags", "sub"]);
        assert_eq!(r.primary_key, ["_id"]);
        assert!(r.columns[0].is_primary_key && !r.columns[0].nullable);
        assert!(!r.columns[1].is_primary_key && r.columns[1].nullable);
        assert_eq!(r.columns[2].type_name, "int");
        assert_eq!(
            r.rows[0],
            [
                format!("ObjectId(\"{}\")", oid(1).to_hex()),
                "a".into(),
                "1".into(),
                ABSENT_CELL.into(),
                ABSENT_CELL.into()
            ]
        );
        assert_eq!(r.rows[1][2], NULL_CELL);
        assert_eq!(r.rows[1][3], "[\"x\", 2]");
        assert_eq!(r.rows[1][4], "{a: 1}");
        assert!(flatten(&[]).columns.is_empty());
    }

    #[test]
    fn cells_are_plain_for_scalars() {
        assert_eq!(cell_text(&Bson::String("a\"b".into())), "a\"b");
        assert_eq!(cell_text(&Bson::Int64(5)), "5");
        assert_eq!(cell_text(&Bson::Double(1.0)), "1.0");
        assert_eq!(cell_text(&Bson::Double(2.5)), "2.5");
        assert_eq!(cell_text(&Bson::Boolean(true)), "true");
        assert_eq!(
            cell_text(&Bson::DateTime(bson::DateTime::from_millis(
                1_704_164_645_678
            ))),
            "2024-01-02T03:04:05.678Z"
        );
        assert_eq!(
            cell_text(&Bson::Decimal128("1.10".parse().unwrap())),
            "1.10"
        );
    }

    #[test]
    fn cell_to_bson_keeps_the_type() {
        let s = Bson::String("x".into());
        assert_eq!(
            cell_to_bson(Some(&s), "12"),
            Ok(Some(Bson::String("12".into())))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Int32(1)), " 12 "),
            Ok(Some(Bson::Int32(12)))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Int64(1)), "12"),
            Ok(Some(Bson::Int64(12)))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Double(1.0)), "12"),
            Ok(Some(Bson::Double(12.0)))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Boolean(true)), "false"),
            Ok(Some(Bson::Boolean(false)))
        );
        let d = Bson::DateTime(bson::DateTime::from_millis(0));
        assert_eq!(
            cell_to_bson(Some(&d), "2024-01-02T03:04:05.678Z"),
            Ok(Some(Bson::DateTime(bson::DateTime::from_millis(
                1_704_164_645_678
            ))))
        );
        let o = Bson::ObjectId(oid(1));
        assert_eq!(
            cell_to_bson(Some(&o), &oid(2).to_hex()),
            Ok(Some(Bson::ObjectId(oid(2))))
        );
        assert_eq!(
            cell_to_bson(Some(&o), &cell_text(&Bson::ObjectId(oid(3)))),
            Ok(Some(Bson::ObjectId(oid(3))))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Decimal128("1".parse().unwrap())), "2.5"),
            Ok(Some(Bson::Decimal128("2.5".parse().unwrap())))
        );
        // Shell syntax changes the type on purpose.
        assert_eq!(
            cell_to_bson(Some(&Bson::Int32(1)), "NumberLong(3)"),
            Ok(Some(Bson::Int64(3)))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Document(doc! {})), "{a: [1]}"),
            Ok(Some(Bson::Document(doc! { "a": [1] })))
        );
        assert!(cell_to_bson(Some(&Bson::Int32(1)), "abc").is_err());
        assert!(cell_to_bson(Some(&Bson::Document(doc! {})), "{a:").is_err());
    }

    #[test]
    fn cell_to_bson_null_absent_and_new_fields() {
        assert_eq!(
            cell_to_bson(Some(&Bson::Int32(1)), NULL_CELL),
            Ok(Some(Bson::Null))
        );
        assert_eq!(cell_to_bson(Some(&Bson::Int32(1)), ABSENT_CELL), Ok(None));
        assert_eq!(cell_to_bson(None, "12"), Ok(Some(Bson::Int32(12))));
        assert_eq!(cell_to_bson(None, "true"), Ok(Some(Bson::Boolean(true))));
        assert_eq!(
            cell_to_bson(None, "hello world"),
            Ok(Some(Bson::String("hello world".into())))
        );
        assert_eq!(
            cell_to_bson(Some(&Bson::Null), "\"7\""),
            Ok(Some(Bson::String("7".into())))
        );
    }

    fn page() -> DocPage {
        let docs = vec![
            doc! { "_id": oid(1), "name": "a", "n": 1 },
            doc! { "_id": 2, "name": "b" },
        ];
        DocPage {
            result: flatten(&docs),
            docs,
        }
    }

    #[test]
    fn doc_changes_from_grid_edits() {
        let p = page();
        let row0 = p.result.rows[0].clone();
        let row1 = p.result.rows[1].clone();
        let mut edited0 = row0.clone();
        edited0[1] = "z".into();
        edited0[2] = ABSENT_CELL.into();
        let mut edited1 = row1.clone();
        edited1[2] = "5".into();
        let changes = RowChanges {
            updates: vec![(row0.clone(), edited0), (row1.clone(), edited1)],
            inserts: vec![vec![Some(oid(9).to_hex()), Some("new".into()), None]],
            deletes: vec![row1.clone()],
        };
        let out = doc_changes(&p, &changes).unwrap();
        assert_eq!(
            out,
            vec![
                DocChange::Set {
                    id: Bson::ObjectId(oid(1)),
                    field: "name".into(),
                    value: "z".into()
                },
                DocChange::Unset {
                    id: Bson::ObjectId(oid(1)),
                    field: "n".into()
                },
                DocChange::Set {
                    id: Bson::Int32(2),
                    field: "n".into(),
                    value: Bson::Int32(5)
                },
                DocChange::Insert(doc! { "_id": oid(9), "name": "new" }),
                DocChange::Delete { id: Bson::Int32(2) },
            ]
        );
    }

    #[test]
    fn rows_are_found_by_position_not_by_the_id_text() {
        // `"1"`, `1` and `NumberLong(1)` all show as 1.
        let docs = vec![
            doc! { "_id": "1", "a": 1 },
            doc! { "_id": 1, "a": 2 },
            doc! { "_id": 1_i64, "a": 1 },
        ];
        let p = DocPage {
            result: flatten(&docs),
            docs,
        };
        assert_eq!(p.result.rows[0], p.result.rows[2]);
        let row1 = p.result.rows[1].clone();
        let mut edited = row1.clone();
        edited[1] = "3".into();
        let out = doc_changes(
            &p,
            &RowChanges {
                updates: vec![(row1, edited)],
                deletes: vec![],
                inserts: vec![],
            },
        )
        .unwrap();
        assert_eq!(
            out,
            [DocChange::Set {
                id: Bson::Int32(1),
                field: "a".into(),
                value: Bson::Int32(3)
            }]
        );
        // Rows 0 and 2 can't be told apart: refused, nothing guessed.
        let e = doc_changes(
            &p,
            &RowChanges {
                deletes: vec![p.result.rows[0].clone()],
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(e.contains("plusieurs documents"), "{e}");
    }

    #[test]
    fn doc_changes_errors() {
        let p = page();
        let row0 = p.result.rows[0].clone();
        let mut edited = row0.clone();
        edited[0] = "x".into();
        let e = doc_changes(
            &p,
            &RowChanges {
                updates: vec![(row0.clone(), edited)],
                ..Default::default()
            },
        );
        assert!(e.unwrap_err().contains("_id"));
        let mut ghost = row0.clone();
        ghost[0] = "nope".into();
        let e = doc_changes(
            &p,
            &RowChanges {
                deletes: vec![ghost],
                ..Default::default()
            },
        );
        assert!(e.unwrap_err().contains("introuvable"));
        let mut bad = row0.clone();
        bad[2] = "abc".into();
        let e = doc_changes(
            &p,
            &RowChanges {
                updates: vec![(row0, bad)],
                ..Default::default()
            },
        );
        assert!(e.unwrap_err().starts_with("n : "));
    }

    #[test]
    fn changes_as_shell_commands() {
        let c = CollectionRef {
            db: "shop".into(),
            coll: "users".into(),
        };
        let id = Bson::Int32(1);
        assert_eq!(
            change_to_shell(
                &c,
                &DocChange::Set {
                    id: id.clone(),
                    field: "a b".into(),
                    value: 2.into()
                }
            ),
            "db.users.updateOne({_id: 1}, {$set: {\"a b\": 2}})"
        );
        assert_eq!(
            change_to_shell(
                &c,
                &DocChange::Unset {
                    id: id.clone(),
                    field: "a".into()
                }
            ),
            "db.users.updateOne({_id: 1}, {$unset: {a: \"\"}})"
        );
        assert_eq!(
            change_to_shell(
                &c,
                &DocChange::Replace {
                    id: id.clone(),
                    doc: doc! { "_id": 1, "x": 1 }
                }
            ),
            "db.users.replaceOne({_id: 1}, {_id: 1, x: 1})"
        );
        assert_eq!(
            change_to_shell(&c, &DocChange::Insert(doc! { "x": 1 })),
            "db.users.insertOne({x: 1})"
        );
        let dotted = CollectionRef {
            db: "shop".into(),
            coll: "fs-x".into(),
        };
        assert_eq!(
            change_to_shell(&dotted, &DocChange::Delete { id }),
            "db.getCollection(\"fs-x\").deleteOne({_id: 1})"
        );
    }

    #[test]
    fn sampled_fields_are_dotted_paths() {
        let docs = vec![
            doc! { "_id": 1, "a": { "b": { "c": { "d": 1 } } }, "n": 1 },
            doc! { "_id": 2, "n": "x", "m": Bson::Null, "arr": [{ "z": 1 }] },
        ];
        let cols = fields_of(&docs);
        let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["_id", "a", "a.b", "a.b.c", "n", "m", "arr"]);
        let n = &cols[4];
        assert_eq!(n.type_name, "int|string");
        assert!(!n.nullable);
        assert!(cols[1].nullable);
        assert_eq!(cols[5].type_name, "null");
        assert!(cols[5].nullable);
        assert!(cols[0].is_primary_key);
        assert_eq!(cols[6].type_name, "array");
    }

    #[test]
    fn index_details() {
        let i = index_from_doc(&doc! {
            "v": 2, "key": { "a": 1, "b": -1, "loc": "2dsphere" }, "name": "ix",
            "unique": true, "expireAfterSeconds": 60, "partialFilterExpression": { "a": { "$gt": 1 } },
            "hidden": true,
        });
        assert_eq!(i.ttl, Some(60));
        assert!(i.unique && i.hidden && !i.sparse);
        assert_eq!(i.partial, Some(doc! { "a": { "$gt": 1 } }));
        let info = index_info(&i);
        assert_eq!(info.columns, ["a: 1", "b: -1", "loc: \"2dsphere\""]);
        assert!(info.unique && !info.primary);
        let id = index_from_doc(&doc! { "key": { "_id": 1 }, "name": "_id_" });
        let info = index_info(&id);
        assert!(info.primary && info.unique);
    }
}
