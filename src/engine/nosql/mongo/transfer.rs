//! MongoDB export / import: JSON (array), JSON Lines and CSV (plan 13,
//! user decision 2). JSON files hold canonical extended JSON (types kept:
//! a small `NumberLong` stays a long, `1.0` a double; relaxed JSON, as
//! mongoexport writes by default, is read too); CSV goes through
//! `export_to_csv` / `parse_csv`.

use std::path::Path;

use anyhow::{Context, Result};
use bson::{Bson, Document};
use futures_util::TryStreamExt;

use super::flatten::is_object_id_hex;
use super::shell::{fmt_double, iso_date, parse_document, parse_iso_date};
use super::{mongo_error, CollectionRef, MongoClient};
use crate::engine::models::{Column, QueryResult, NULL_CELL};
use crate::engine::nosql::{ImportOptions, TransferFormat};
use crate::engine::ops::transfer::ImportStats;
use crate::engine::services::export_import::{
    export_to_csv_quoting_empty, parse_csv_marking_quoted_empty, QUOTED_EMPTY,
};

/// Documents per import batch.
const BATCH: usize = 1000;
/// Serialized bytes per import batch (well under the 16 MiB command limit).
const BATCH_BYTES: usize = 8 * 1024 * 1024;

fn relaxed_json(v: &Bson) -> String {
    v.clone().into_relaxed_extjson().to_string()
}

/// A document as canonical extended JSON (every BSON type kept).
pub(crate) fn canonical_json(d: &Document) -> serde_json::Value {
    Bson::Document(d.clone()).into_canonical_extjson()
}

/// One CSV cell: plain text for strings, numbers, booleans and dates,
/// relaxed extended JSON for the rest; the top-level `_id` ObjectId as its
/// hex string (read back as an ObjectId).
fn csv_cell(v: &Bson, top_id: bool) -> String {
    match v {
        Bson::Null => NULL_CELL.to_string(),
        Bson::String(s) => s.clone(),
        Bson::Int32(i) => i.to_string(),
        Bson::Int64(i) => i.to_string(),
        Bson::Double(f) if f.is_finite() => fmt_double(*f),
        Bson::Boolean(b) => b.to_string(),
        Bson::DateTime(d) => iso_date(*d).unwrap_or_else(|| relaxed_json(v)),
        Bson::ObjectId(oid) if top_id => oid.to_hex(),
        other => relaxed_json(other),
    }
}

/// CSV grid: nested fields as dotted paths (`a.b`), arrays (and empty
/// objects) as relaxed extended JSON, null and missing fields as
/// `NULL_CELL` (an empty field), an empty string as "" (written `""` by
/// `export_to_csv_quoting_empty`); `_id` first, then the paths by first
/// appearance.
pub fn flatten_paths(docs: &[Document]) -> QueryResult {
    fn walk(out: &mut Vec<(String, String)>, doc: &Document, prefix: &str) {
        for (k, v) in doc {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                Bson::Document(sub) if !sub.is_empty() => walk(out, sub, &path),
                _ => {
                    let cell = csv_cell(v, prefix.is_empty() && k == "_id");
                    out.push((path, cell));
                }
            }
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut index = std::collections::HashMap::new();
    let mut flat_docs = Vec::with_capacity(docs.len());
    for doc in docs {
        let mut cells = Vec::new();
        walk(&mut cells, doc, "");
        for (path, _) in &cells {
            if !index.contains_key(path) {
                index.insert(path.clone(), ());
                if path == "_id" {
                    order.insert(0, path.clone());
                } else {
                    order.push(path.clone());
                }
            }
        }
        flat_docs.push(
            cells
                .into_iter()
                .collect::<std::collections::HashMap<_, _>>(),
        );
    }
    QueryResult {
        columns: order
            .iter()
            .map(|name| Column {
                name: name.clone(),
                type_name: "string".into(),
                nullable: true,
                is_primary_key: name == "_id",
            })
            .collect(),
        rows: flat_docs
            .into_iter()
            .map(|cells| {
                order
                    .iter()
                    .map(|p| cells.get(p).cloned().unwrap_or_else(|| NULL_CELL.into()))
                    .collect()
            })
            .collect(),
        ..Default::default()
    }
}

/// Whether `s` is an integer without a leading zero (`007` stays text).
fn is_integer(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
}

/// Whether `s` is a decimal number (a fraction or an exponent).
fn is_decimal(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int, frac) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa, None),
    };
    let int_ok = is_integer(int);
    let frac_ok = frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()));
    let exp_ok = exp.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
    });
    int_ok && frac_ok && exp_ok && (frac.is_some() || exp.is_some())
}

/// `YYYY-MM-DD…`: worth trying as a date.
fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// The value of a CSV cell (see `csv_row_to_doc`).
fn infer(column: &str, cell: &str, opts: &ImportOptions) -> Bson {
    if column == "_id" && is_object_id_hex(cell) {
        if let Ok(oid) = bson::oid::ObjectId::parse_str(cell) {
            return Bson::ObjectId(oid);
        }
    }
    if opts.keep_strings {
        return Bson::String(cell.to_string());
    }
    match cell {
        "true" => return Bson::Boolean(true),
        "false" => return Bson::Boolean(false),
        "null" => return Bson::Null,
        _ => {}
    }
    if is_integer(cell) {
        if let Ok(i) = cell.parse::<i64>() {
            return match i32::try_from(i) {
                Ok(i) => Bson::Int32(i),
                Err(_) => Bson::Int64(i),
            };
        }
    } else if is_decimal(cell) {
        if let Ok(f) = cell.parse::<f64>() {
            return Bson::Double(f);
        }
    } else if cell.starts_with(['[', '{']) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(cell) {
            if let Ok(b) = Bson::try_from(v) {
                return b;
            }
        }
    } else if opts.parse_dates && looks_like_date(cell) {
        if let Some(d) = parse_iso_date(cell) {
            return Bson::DateTime(d);
        }
    }
    Bson::String(cell.to_string())
}

/// Put `value` at the dotted `path` of `doc`; `Err` on a conflict.
fn insert_path(doc: &mut Document, path: &[&str], value: Bson) -> Result<(), ()> {
    match path {
        [] => Err(()),
        [leaf] => {
            if doc.contains_key(*leaf) {
                return Err(());
            }
            doc.insert(*leaf, value);
            Ok(())
        }
        [head, rest @ ..] => {
            if !doc.contains_key(*head) {
                doc.insert(*head, Document::new());
            }
            match doc.get_mut(*head) {
                Some(Bson::Document(sub)) => insert_path(sub, rest, value),
                _ => Err(()),
            }
        }
    }
}

/// A CSV row as a document: dotted paths → nested objects; an empty field
/// = absent, `""` (`QUOTED_EMPTY`) = an empty string; inference (unless `keep_strings`): integer → Int32/Int64,
/// decimal → Double, true/false → Bool, `null` → Null, `[`/`{` valid JSON
/// (extended JSON) → array/object, ISO 8601 → Date when `parse_dates`,
/// else a string; an `_id` of 24 hex digits → ObjectId.
pub fn csv_row_to_doc(
    columns: &[String],
    row: &[String],
    opts: &ImportOptions,
) -> Result<Document, String> {
    if row.len() != columns.len() {
        return Err(format!(
            "{} champs pour {} colonnes",
            row.len(),
            columns.len()
        ));
    }
    let mut doc = Document::new();
    for (column, cell) in columns.iter().zip(row) {
        if cell.is_empty() {
            continue;
        }
        let value = if cell == QUOTED_EMPTY {
            Bson::String(String::new())
        } else {
            infer(column, cell, opts)
        };
        let segments: Vec<&str> = column.split('.').collect();
        let path = if segments.iter().any(|s| s.is_empty()) {
            vec![column.as_str()]
        } else {
            segments
        };
        insert_path(&mut doc, &path, value)
            .map_err(|_| format!("conflit de chemins sur la colonne « {column} »"))?;
    }
    Ok(doc)
}

/// The documents of a JSON, JSON Lines or CSV file; per-record errors go to
/// the second list (« ligne n : … »).
pub(crate) fn read_documents(
    content: &str,
    format: TransferFormat,
    opts: &ImportOptions,
) -> Result<(Vec<Document>, Vec<String>)> {
    let to_doc = |v: serde_json::Value| -> Result<Document, String> {
        match Bson::try_from(v) {
            Ok(Bson::Document(d)) => Ok(d),
            Ok(_) => Err("document attendu".into()),
            Err(e) => Err(format!("JSON étendu invalide : {e}")),
        }
    };
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut docs = Vec::new();
    let mut errors = Vec::new();
    match format {
        TransferFormat::Json => {
            let value: serde_json::Value =
                serde_json::from_str(content).context("Fichier JSON invalide")?;
            let items = match value {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            };
            for (i, item) in items.into_iter().enumerate() {
                match to_doc(item) {
                    Ok(d) => docs.push(d),
                    Err(e) => errors.push(format!("élément {} : {e}", i + 1)),
                }
            }
        }
        TransferFormat::JsonLines => {
            for (i, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<serde_json::Value>(line)
                    .map_err(|e| format!("JSON invalide : {e}"))
                    .and_then(to_doc)
                {
                    Ok(d) => docs.push(d),
                    Err(e) => errors.push(format!("ligne {} : {e}", i + 1)),
                }
            }
        }
        TransferFormat::Csv => {
            let (columns, rows) =
                parse_csv_marking_quoted_empty(content).map_err(|e| anyhow::anyhow!(e))?;
            for (i, row) in rows.iter().enumerate() {
                match csv_row_to_doc(&columns, row, opts) {
                    Ok(d) => docs.push(d),
                    Err(e) => errors.push(format!("ligne {} : {e}", i + 2)),
                }
            }
        }
    }
    Ok((docs, errors))
}

/// The file content of `docs`.
pub(crate) fn write_documents(docs: &[Document], format: TransferFormat) -> String {
    let json = |d: &Document| canonical_json(d).to_string();
    match format {
        TransferFormat::Json => {
            if docs.is_empty() {
                return "[]\n".into();
            }
            let items: Vec<String> = docs.iter().map(|d| format!("  {}", json(d))).collect();
            format!("[\n{}\n]\n", items.join(",\n"))
        }
        TransferFormat::JsonLines => docs.iter().map(|d| json(d) + "\n").collect(),
        TransferFormat::Csv => export_to_csv_quoting_empty(&flatten_paths(docs)),
    }
}

/// Write the documents matching `filter` (shell syntax) to `path`; returns
/// their number. `progress(done, total, collection)`.
pub async fn export_collection(
    c: &MongoClient,
    coll: &CollectionRef,
    filter: &str,
    path: &Path,
    format: TransferFormat,
    mut progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<usize> {
    let filter = parse_document(filter).map_err(|e| anyhow::anyhow!("Filtre : {e}"))?;
    let name = coll.to_string();
    let collection = c.coll(coll);
    let total = if filter.is_empty() {
        collection.estimated_document_count().await
    } else {
        collection.count_documents(filter.clone()).await
    }
    .map_err(mongo_error)? as usize;
    progress(0, total, &name);
    let mut cursor = collection.find(filter).await.map_err(mongo_error)?;
    let mut docs = Vec::new();
    while let Some(doc) = cursor.try_next().await.map_err(mongo_error)? {
        docs.push(doc);
        if docs.len().is_multiple_of(BATCH) {
            progress(docs.len(), total.max(docs.len()), &name);
        }
    }
    std::fs::write(path, write_documents(&docs, format))
        .with_context(|| format!("Écriture de {} impossible", path.display()))?;
    progress(docs.len(), docs.len(), &name);
    Ok(docs.len())
}

/// Read `path` into `coll` (batches of 1000, unordered). With `upsert`,
/// documents with an `_id` replace the stored one (or are inserted);
/// without, an existing `_id` is an error of that document.
pub async fn import_collection(
    c: &MongoClient,
    coll: &CollectionRef,
    path: &Path,
    format: TransferFormat,
    opts: &ImportOptions,
    mut progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<ImportStats> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Lecture de {} impossible", path.display()))?;
    let (docs, errors) = read_documents(&content, format, opts)?;
    let name = coll.to_string();
    let mut stats = ImportStats {
        total: docs.len() + errors.len(),
        errors,
        ..Default::default()
    };
    let total = docs.len();
    progress(0, total, &name);
    let db = c.client.database(&coll.db);
    let mut done = 0;
    for batch in batches(docs) {
        let n = batch.len();
        let (replace, insert): (Vec<Document>, Vec<Document>) = batch
            .into_iter()
            .partition(|d| opts.upsert && d.contains_key("_id"));
        if !insert.is_empty() {
            let reply = db
                .run_command(bson::doc! {
                    "insert": &coll.coll,
                    "documents": insert.iter().cloned().map(Bson::Document).collect::<Vec<_>>(),
                    "ordered": false,
                })
                .await
                .map_err(mongo_error)?;
            stats.inserted += count(&reply, "n");
            write_errors(&reply, &insert, done, &mut stats.errors);
        }
        if !replace.is_empty() {
            let updates: Vec<Bson> = replace
                .iter()
                .map(|d| {
                    Bson::Document(bson::doc! {
                        "q": { "_id": d.get("_id").cloned().unwrap_or(Bson::Null) },
                        "u": d.clone(),
                        "upsert": true,
                    })
                })
                .collect();
            let reply = db
                .run_command(bson::doc! {
                    "update": &coll.coll,
                    "updates": updates,
                    "ordered": false,
                })
                .await
                .map_err(mongo_error)?;
            let upserted = reply.get_array("upserted").map_or(0, |a| a.len());
            stats.inserted += upserted;
            stats.updated += count(&reply, "n").saturating_sub(upserted);
            write_errors(&reply, &replace, done, &mut stats.errors);
        }
        done += n;
        progress(done, total, &name);
    }
    Ok(stats)
}

fn count(reply: &Document, key: &str) -> usize {
    match reply.get(key) {
        Some(Bson::Int32(n)) => *n as usize,
        Some(Bson::Int64(n)) => *n as usize,
        Some(Bson::Double(n)) => *n as usize,
        _ => 0,
    }
}

/// The `writeErrors` of a write reply, with the `_id` of each document.
fn write_errors(reply: &Document, docs: &[Document], offset: usize, out: &mut Vec<String>) {
    let Ok(errors) = reply.get_array("writeErrors") else {
        return;
    };
    for e in errors {
        let Bson::Document(e) = e else { continue };
        let index = count(e, "index");
        let id = docs
            .get(index)
            .and_then(|d| d.get("_id"))
            .map(|id| format!(" (_id {})", super::shell::to_shell(id, false)))
            .unwrap_or_default();
        let msg = e.get_str("errmsg").unwrap_or("erreur d'écriture");
        out.push(format!("document {}{id} : {msg}", offset + index + 1));
    }
}

/// Split `docs` in batches of at most `BATCH` documents and `BATCH_BYTES`.
fn batches(docs: Vec<Document>) -> Vec<Vec<Document>> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0;
    for d in docs {
        let size = d.to_vec().map_or(0, |v| v.len());
        if !current.is_empty() && (current.len() >= BATCH || bytes + size > BATCH_BYTES) {
            out.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += size;
        current.push(d);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{doc, oid::ObjectId};

    fn opts() -> ImportOptions {
        ImportOptions::default()
    }

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn json_exports_keep_every_number_type() {
        let d = doc! {
            "_id": 1,
            "long": 5_i64,
            "int": 5_i32,
            "double": 1.0,
            "dec": Bson::Decimal128("1.50".parse().unwrap()),
            "date": bson::DateTime::from_millis(1_700_000_000_123),
            "nested": { "l": [2_i64, 3_i32] },
        };
        for format in [TransferFormat::Json, TransferFormat::JsonLines] {
            let text = write_documents(std::slice::from_ref(&d), format);
            assert!(text.contains("$numberLong"), "{text}");
            let (docs, errors) = read_documents(&text, format, &opts()).unwrap();
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(docs, [d.clone()], "{format:?}");
        }
        // Relaxed files (mongoexport's default) still read.
        let relaxed = "{\"a\": 1, \"b\": 1.5, \"c\": {\"$date\": \"2024-01-02T03:04:05Z\"}}\n";
        let (docs, _) = read_documents(relaxed, TransferFormat::JsonLines, &opts()).unwrap();
        assert_eq!(docs[0].get("a"), Some(&Bson::Int32(1)));
        assert!(matches!(docs[0].get("c"), Some(Bson::DateTime(_))));
    }

    #[test]
    fn flatten_paths_uses_dotted_paths_and_json() {
        let oid = ObjectId::from_bytes([1; 12]);
        let docs = vec![
            doc! { "name": "a", "_id": oid, "addr": { "city": "P", "geo": { "lat": 1.5 } }, "tags": ["x", 1] },
            doc! { "_id": 2, "n": Bson::Null, "ref": oid, "e": {}, "d": bson::DateTime::from_millis(0) },
        ];
        let r = flatten_paths(&docs);
        let names: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "_id",
                "name",
                "addr.city",
                "addr.geo.lat",
                "tags",
                "n",
                "ref",
                "e",
                "d"
            ]
        );
        assert_eq!(
            r.rows[0][..5],
            [
                oid.to_hex(),
                "a".into(),
                "P".into(),
                "1.5".into(),
                "[\"x\",1]".into()
            ]
        );
        assert_eq!(r.rows[0][5], NULL_CELL, "missing");
        assert_eq!(r.rows[1][0], "2");
        assert_eq!(r.rows[1][5], NULL_CELL);
        assert_eq!(r.rows[1][6], format!("{{\"$oid\":\"{}\"}}", oid.to_hex()));
        assert_eq!(r.rows[1][7], "{}");
        assert_eq!(r.rows[1][8], "1970-01-01T00:00:00.000Z");
        let csv = export_to_csv_quoting_empty(&r);
        assert!(csv.starts_with("_id,name,addr.city,"));
        assert!(csv.contains(",,"), "null and missing are empty: {csv}");
        assert!(!csv.contains(",\"\","), "no empty string here: {csv}");
    }

    #[test]
    fn csv_keeps_empty_strings_apart_from_null_and_missing() {
        let docs = vec![
            doc! { "_id": 1, "note": "", "n": Bson::Null, "x": 1 },
            doc! { "_id": 2, "x": 2 },
        ];
        let csv = write_documents(&docs, TransferFormat::Csv);
        assert_eq!(csv, "_id,note,n,x\n1,\"\",,1\n2,,,2\n");
        let (back, errors) = read_documents(&csv, TransferFormat::Csv, &opts()).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        // The empty string survives; null and missing both come back missing.
        assert_eq!(
            back,
            [
                doc! { "_id": 1, "note": "", "x": 1 },
                doc! { "_id": 2, "x": 2 }
            ]
        );
        // Even with every value kept as text.
        let text = ImportOptions {
            keep_strings: true,
            ..opts()
        };
        let (back, _) = read_documents(&csv, TransferFormat::Csv, &text).unwrap();
        assert_eq!(back[0].get("note"), Some(&Bson::String(String::new())));
        assert!(!back[1].contains_key("note"));
    }

    #[test]
    fn csv_inference() {
        let cols = row(&[
            "_id", "i", "l", "d", "t", "f", "n", "a", "o", "s", "z", "date", "e", "bad",
        ]);
        let r = row(&[
            "65a1b2c3d4e5f60718293a4b",
            "-12",
            "3000000000",
            "1.5e2",
            "true",
            "false",
            "null",
            "[1,\"x\"]",
            "{\"$oid\":\"65a1b2c3d4e5f60718293a4b\"}",
            "hello",
            "007",
            "2024-01-02T03:04:05.678Z",
            "",
            "[oops",
        ]);
        let d = csv_row_to_doc(&cols, &r, &opts()).unwrap();
        let oid = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
        assert_eq!(
            d,
            doc! {
                "_id": oid, "i": -12, "l": 3_000_000_000_i64, "d": 150.0, "t": true, "f": false,
                "n": Bson::Null, "a": [1, "x"], "o": oid, "s": "hello", "z": "007",
                "date": "2024-01-02T03:04:05.678Z", "bad": "[oops",
            }
        );
        let dated = csv_row_to_doc(
            &cols,
            &r,
            &ImportOptions {
                parse_dates: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(
            dated.get("date"),
            Some(&Bson::DateTime(bson::DateTime::from_millis(
                1_704_164_645_678
            )))
        );
        let strings = csv_row_to_doc(
            &cols,
            &r,
            &ImportOptions {
                keep_strings: true,
                parse_dates: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(strings.get("_id"), Some(&Bson::ObjectId(oid)));
        assert_eq!(strings.get("i"), Some(&Bson::String("-12".into())));
        assert_eq!(
            strings.get("date"),
            Some(&Bson::String("2024-01-02T03:04:05.678Z".into()))
        );
        assert!(!strings.contains_key("e"));
    }

    #[test]
    fn csv_paths_build_nested_objects() {
        let cols = row(&["a.b", "a.c", "x"]);
        assert_eq!(
            csv_row_to_doc(&cols, &row(&["1", "2", "3"]), &opts()).unwrap(),
            doc! { "a": { "b": 1, "c": 2 }, "x": 3 }
        );
        let conflict = row(&["a", "a.b"]);
        assert!(csv_row_to_doc(&conflict, &row(&["1", "2"]), &opts())
            .unwrap_err()
            .contains("conflit"));
        let conflict = row(&["a.b", "a"]);
        assert!(csv_row_to_doc(&conflict, &row(&["1", "2"]), &opts()).is_err());
        // No conflict when one side is empty.
        assert_eq!(
            csv_row_to_doc(&conflict, &row(&["1", ""]), &opts()).unwrap(),
            doc! { "a": { "b": 1 } }
        );
        assert!(csv_row_to_doc(&cols, &row(&["1"]), &opts()).is_err());
    }

    #[test]
    fn files_round_trip_in_every_format() {
        let oid = ObjectId::from_bytes([7; 12]);
        let docs = vec![
            doc! { "_id": oid, "name": "a,b \"q\"", "n": 1, "x": 2.5, "sub": { "k": true }, "tags": [1, 2] },
            doc! { "_id": 2, "when": bson::DateTime::from_millis(1_704_164_645_678), "big": Bson::Int64(5_000_000_000) },
        ];
        for format in [TransferFormat::Json, TransferFormat::JsonLines] {
            let text = write_documents(&docs, format);
            let (back, errors) = read_documents(&text, format, &opts()).unwrap();
            assert!(errors.is_empty());
            assert_eq!(back, docs, "{format:?}");
        }
        let text = write_documents(&docs, TransferFormat::Csv);
        let (back, errors) = read_documents(
            &text,
            TransferFormat::Csv,
            &ImportOptions {
                parse_dates: true,
                ..opts()
            },
        )
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(back, docs);
        assert_eq!(write_documents(&[], TransferFormat::Json), "[]\n");
    }

    #[test]
    fn bad_records_are_reported() {
        let (docs, errors) = read_documents(
            "{\"a\":1}\n[1]\nnope\n\n",
            TransferFormat::JsonLines,
            &opts(),
        )
        .unwrap();
        assert_eq!(docs, vec![doc! { "a": 1 }]);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].starts_with("ligne 2"));
        assert!(read_documents("[", TransferFormat::Json, &opts()).is_err());
        let (docs, _) = read_documents("{\"a\": 1}", TransferFormat::Json, &opts()).unwrap();
        assert_eq!(docs.len(), 1);
    }

    #[test]
    fn batches_cut_at_1000() {
        let docs: Vec<Document> = (0..2500).map(|i| doc! { "i": i }).collect();
        let sizes: Vec<usize> = batches(docs).iter().map(Vec::len).collect();
        assert_eq!(sizes, [1000, 1000, 500]);
    }
}
