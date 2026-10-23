//! Redis export / import: JSON (`[{key, type, ttl, value}]`), JSON Lines and
//! CSV (`key,type,ttl,value`).

use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::{json, Map, Value as Json};

use super::{
    cli::format_score, display_bytes, parse_display, RedisClient, RedisType, RedisValue,
    StreamEntry,
};
use crate::engine::models::{Column, QueryResult, NULL_CELL};
use crate::engine::nosql::{ImportOptions, TransferFormat};
use crate::engine::ops::transfer::ImportStats;
use crate::engine::services::export_import::{export_to_csv, parse_csv};

const B64: &str = "$base64";

/// Bytes as JSON: a string when valid UTF-8, else `{"$base64": …}`.
fn bytes_json(b: &[u8]) -> Json {
    match std::str::from_utf8(b) {
        Ok(s) => Json::String(s.to_string()),
        Err(_) => json!({ B64: base64::engine::general_purpose::STANDARD.encode(b) }),
    }
}

fn bytes_from_json(j: &Json) -> Result<Vec<u8>, String> {
    match j {
        Json::String(s) => Ok(s.as_bytes().to_vec()),
        Json::Object(o) if o.len() == 1 && o.contains_key(B64) => {
            let text = o[B64].as_str().ok_or("$base64 : chaîne attendue")?;
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|e| format!("$base64 invalide : {e}"))
        }
        // Numbers and booleans read as their text (hand-written files).
        Json::Number(n) => Ok(n.to_string().into_bytes()),
        Json::Bool(b) => Ok(b.to_string().into_bytes()),
        other => Err(format!("chaîne attendue, trouvé {other}")),
    }
}

/// Field / value pairs: an object when every field is UTF-8 and unique,
/// else `[[field, value], …]`.
fn pairs_json(pairs: &[(Vec<u8>, Vec<u8>)]) -> Json {
    let mut map = Map::new();
    for (f, v) in pairs {
        match std::str::from_utf8(f) {
            Ok(f) if !map.contains_key(f) => {
                map.insert(f.to_string(), bytes_json(v));
            }
            _ => {
                return Json::Array(
                    pairs
                        .iter()
                        .map(|(f, v)| json!([bytes_json(f), bytes_json(v)]))
                        .collect(),
                )
            }
        }
    }
    Json::Object(map)
}

fn pairs_from_json(j: &Json) -> Result<super::Pairs, String> {
    match j {
        Json::Object(o) => o
            .iter()
            .map(|(k, v)| Ok((k.as_bytes().to_vec(), bytes_from_json(v)?)))
            .collect(),
        Json::Array(items) => items
            .iter()
            .map(|p| match p.as_array().map(Vec::as_slice) {
                Some([f, v]) => Ok((bytes_from_json(f)?, bytes_from_json(v)?)),
                _ => Err("paire [champ, valeur] attendue".to_string()),
            })
            .collect(),
        other => Err(format!("objet attendu, trouvé {other}")),
    }
}

fn score_json(s: f64) -> Json {
    serde_json::Number::from_f64(s)
        .map(Json::Number)
        .unwrap_or_else(|| Json::String(format_score(s)))
}

fn score_from_json(j: &Json) -> Result<f64, String> {
    match j {
        Json::Number(n) => n.as_f64().ok_or_else(|| format!("score invalide : {n}")),
        Json::String(s) => super::parse_score(s),
        other => Err(format!("score invalide : {other}")),
    }
}

/// A value as JSON (binary as `{"$base64": …}`): string → string; list /
/// set → array; zset → `[{member, score}]`; hash → object (pairs when a
/// field is binary); stream → `[{id, fields}]`.
pub fn value_to_json(v: &RedisValue) -> Json {
    match v {
        RedisValue::String(b) => bytes_json(b),
        RedisValue::List(items) | RedisValue::Set(items) => {
            Json::Array(items.iter().map(|i| bytes_json(i)).collect())
        }
        RedisValue::ZSet(members) => Json::Array(
            members
                .iter()
                .map(|(m, s)| json!({ "member": bytes_json(m), "score": score_json(*s) }))
                .collect(),
        ),
        RedisValue::Hash(pairs) => pairs_json(pairs),
        RedisValue::Stream(entries) => Json::Array(
            entries
                .iter()
                .map(|e| json!({ "id": e.id, "fields": pairs_json(&e.fields) }))
                .collect(),
        ),
    }
}

pub fn value_from_json(kind: &RedisType, j: &Json) -> Result<RedisValue, String> {
    let array = || {
        j.as_array()
            .ok_or_else(|| format!("tableau attendu pour le type {}", kind.name()))
    };
    Ok(match kind {
        RedisType::String => RedisValue::String(bytes_from_json(j)?),
        RedisType::List => RedisValue::List(
            array()?
                .iter()
                .map(bytes_from_json)
                .collect::<Result<_, _>>()?,
        ),
        RedisType::Set => RedisValue::Set(
            array()?
                .iter()
                .map(bytes_from_json)
                .collect::<Result<_, _>>()?,
        ),
        RedisType::ZSet => RedisValue::ZSet(
            array()?
                .iter()
                .map(|m| match m {
                    Json::Object(o) => Ok((
                        bytes_from_json(o.get("member").ok_or("membre manquant")?)?,
                        score_from_json(o.get("score").ok_or("score manquant")?)?,
                    )),
                    Json::Array(p) if p.len() == 2 => {
                        Ok((bytes_from_json(&p[0])?, score_from_json(&p[1])?))
                    }
                    other => Err(format!("{{member, score}} attendu, trouvé {other}")),
                })
                .collect::<Result<_, String>>()?,
        ),
        RedisType::Hash => RedisValue::Hash(pairs_from_json(j)?),
        RedisType::Stream => RedisValue::Stream(
            array()?
                .iter()
                .map(|e| {
                    let id = e
                        .get("id")
                        .and_then(Json::as_str)
                        .ok_or("id d'entrée manquant")?
                        .to_string();
                    let fields = pairs_from_json(e.get("fields").ok_or("champs manquants")?)?;
                    Ok(StreamEntry { id, fields })
                })
                .collect::<Result<_, String>>()?,
        ),
        RedisType::Other(name) => return Err(format!("Type non pris en charge : {name}")),
    })
}

/// One exported key.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub key: Vec<u8>,
    pub kind: RedisType,
    pub ttl: Option<i64>,
    pub value: RedisValue,
}

pub(crate) fn entry_json(e: &Entry) -> Json {
    json!({
        "key": bytes_json(&e.key),
        "type": e.kind.name(),
        "ttl": e.ttl,
        "value": value_to_json(&e.value),
    })
}

pub(crate) fn entry_from_json(j: &Json) -> Result<Entry, String> {
    let key = bytes_from_json(j.get("key").ok_or("champ « key » manquant")?)?;
    let kind = RedisType::from_name(
        j.get("type")
            .and_then(Json::as_str)
            .ok_or("champ « type » manquant")?,
    );
    let ttl = match j.get("ttl") {
        None | Some(Json::Null) => None,
        Some(t) => Some(t.as_i64().ok_or("ttl : entier attendu")?),
    };
    let value = value_from_json(&kind, j.get("value").ok_or("champ « value » manquant")?)?;
    Ok(Entry {
        key,
        kind,
        ttl,
        value,
    })
}

pub(crate) const CSV_COLUMNS: [&str; 4] = ["key", "type", "ttl", "value"];

/// `key,type,ttl,value`: the key and a string's value as `display_bytes`
/// text, the other values as compact JSON; an empty ttl = persistent.
pub(crate) fn csv_row(e: &Entry) -> Vec<String> {
    vec![
        display_bytes(&e.key),
        e.kind.name().to_string(),
        e.ttl
            .map_or_else(|| NULL_CELL.to_string(), |t| t.to_string()),
        match &e.value {
            RedisValue::String(b) => display_bytes(b),
            other => value_to_json(other).to_string(),
        },
    ]
}

pub(crate) fn entry_from_csv(columns: &[String], row: &[String]) -> Result<Entry, String> {
    let get = |name: &str| {
        columns
            .iter()
            .position(|c| c.trim().eq_ignore_ascii_case(name))
            .and_then(|i| row.get(i))
            .ok_or_else(|| format!("colonne « {name} » manquante"))
    };
    let key = parse_display(get("key")?)?;
    let kind = RedisType::from_name(get("type")?.trim());
    let ttl = match get("ttl")?.trim() {
        "" => None,
        t => Some(t.parse().map_err(|_| format!("ttl invalide : {t}"))?),
    };
    let text = get("value")?;
    let value = match kind {
        RedisType::String => RedisValue::String(parse_display(text)?),
        _ => {
            let j: Json =
                serde_json::from_str(text).map_err(|e| format!("valeur JSON invalide : {e}"))?;
            value_from_json(&kind, &j)?
        }
    };
    Ok(Entry {
        key,
        kind,
        ttl,
        value,
    })
}

fn csv_text(entries: &[Entry]) -> String {
    let result = QueryResult {
        columns: CSV_COLUMNS
            .iter()
            .map(|c| Column {
                name: c.to_string(),
                type_name: "text".into(),
                nullable: true,
                is_primary_key: false,
            })
            .collect(),
        rows: entries.iter().map(csv_row).collect(),
        ..Default::default()
    };
    export_to_csv(&result)
}

/// Entries of a file's content.
pub(crate) fn parse_entries(
    content: &str,
    format: TransferFormat,
) -> Result<Vec<Result<Entry, String>>, String> {
    let content = content.trim_start_matches('\u{feff}');
    Ok(match format {
        TransferFormat::Json => {
            let j: Json =
                serde_json::from_str(content).map_err(|e| format!("JSON invalide : {e}"))?;
            j.as_array()
                .ok_or("JSON : tableau d'objets {key, type, ttl, value} attendu")?
                .iter()
                .map(entry_from_json)
                .collect()
        }
        TransferFormat::JsonLines => content
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(n, l)| {
                serde_json::from_str::<Json>(l)
                    .map_err(|e| format!("ligne {} : JSON invalide : {e}", n + 1))
                    .and_then(|j| entry_from_json(&j))
            })
            .collect(),
        TransferFormat::Csv => {
            let (columns, rows) = parse_csv(content)?;
            rows.iter().map(|r| entry_from_csv(&columns, r)).collect()
        }
    })
}

pub(crate) fn format_entries(entries: &[Entry], format: TransferFormat) -> String {
    match format {
        TransferFormat::Json => {
            let all: Vec<Json> = entries.iter().map(entry_json).collect();
            serde_json::to_string_pretty(&all).unwrap_or_default() + "\n"
        }
        TransferFormat::JsonLines => entries
            .iter()
            .map(|e| entry_json(e).to_string() + "\n")
            .collect(),
        TransferFormat::Csv => csv_text(entries),
    }
}

/// Write the keys of `db` matching `pattern` to `path`; returns their
/// number. Keys of module types are skipped.
pub async fn export_keys(
    c: &RedisClient,
    db: u8,
    pattern: &str,
    path: &Path,
    format: TransferFormat,
    mut progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<usize> {
    let keys = super::scan_all(c, db, pattern).await?;
    let total = keys.len();
    let mut entries = Vec::with_capacity(total);
    for (n, key) in keys.iter().enumerate() {
        progress(n, total, &display_bytes(key));
        let Some(info) = super::key_info(c, db, key).await? else {
            continue; // expired meanwhile
        };
        if matches!(info.kind, RedisType::Other(_)) {
            continue;
        }
        let Some(value) = super::read_all(c, db, key).await? else {
            continue;
        };
        entries.push(Entry {
            key: key.clone(),
            kind: info.kind,
            ttl: info.ttl,
            value,
        });
    }
    progress(total, total, "");
    std::fs::write(path, format_entries(&entries, format))
        .with_context(|| format!("Écriture de {} impossible", path.display()))?;
    Ok(entries.len())
}

/// Create the keys of `path` in `db`; existing keys are replaced when
/// `opts.upsert`, else reported as errors.
pub async fn import_keys(
    c: &RedisClient,
    db: u8,
    path: &Path,
    format: TransferFormat,
    opts: &ImportOptions,
    mut progress: impl FnMut(usize, usize, &str) + Send,
) -> Result<ImportStats> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Lecture de {} impossible", path.display()))?;
    let entries = parse_entries(&content, format).map_err(anyhow::Error::msg)?;
    let total = entries.len();
    let mut stats = ImportStats {
        total,
        ..Default::default()
    };
    for (n, entry) in entries.into_iter().enumerate() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                stats.errors.push(format!("entrée {} : {e}", n + 1));
                continue;
            }
        };
        let name = display_bytes(&entry.key);
        progress(n, total, &name);
        match super::write_key(c, db, &entry.key, &entry.value, entry.ttl, opts.upsert).await {
            Ok(true) => stats.updated += 1,
            Ok(false) => stats.inserted += 1,
            Err(e) => stats.errors.push(format!("{name} : {e}")),
        }
    }
    progress(total, total, "");
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_values() -> Vec<(RedisType, RedisValue)> {
        vec![
            (RedisType::String, RedisValue::String(b"hello".to_vec())),
            (RedisType::String, RedisValue::String(vec![0xff, 0, 1])),
            (
                RedisType::List,
                RedisValue::List(vec![b"a".to_vec(), vec![0xfe], b"".to_vec()]),
            ),
            (
                RedisType::Set,
                RedisValue::Set(vec![b"x".to_vec(), b"y z".to_vec()]),
            ),
            (
                RedisType::ZSet,
                RedisValue::ZSet(vec![
                    (b"m".to_vec(), 1.5),
                    (vec![0x80], f64::INFINITY),
                    (b"n".to_vec(), f64::NEG_INFINITY),
                ]),
            ),
            (
                RedisType::Hash,
                RedisValue::Hash(vec![
                    (b"f".to_vec(), b"v".to_vec()),
                    (b"g".to_vec(), vec![0xff]),
                ]),
            ),
            (
                RedisType::Hash,
                RedisValue::Hash(vec![(vec![0xff], b"binary field".to_vec())]),
            ),
            (
                RedisType::Stream,
                RedisValue::Stream(vec![StreamEntry {
                    id: "1-0".into(),
                    fields: vec![(b"a".to_vec(), b"1".to_vec())],
                }]),
            ),
        ]
    }

    #[test]
    fn json_round_trip_per_type() {
        for (kind, value) in all_values() {
            let j = value_to_json(&value);
            // Through text, as in a file.
            let j: Json = serde_json::from_str(&j.to_string()).unwrap();
            assert_eq!(value_from_json(&kind, &j).unwrap(), value, "{j}");
        }
    }

    #[test]
    fn json_shapes() {
        assert_eq!(
            value_to_json(&RedisValue::String(vec![0xff])),
            json!({"$base64": "/w=="})
        );
        assert_eq!(
            value_to_json(&RedisValue::ZSet(vec![(b"m".to_vec(), 2.0)])),
            json!([{"member": "m", "score": 2.0}])
        );
        assert_eq!(
            value_to_json(&RedisValue::Hash(vec![(b"f".to_vec(), b"v".to_vec())])),
            json!({"f": "v"})
        );
        // Duplicate fields can't be an object.
        assert!(value_to_json(&RedisValue::Hash(vec![
            (b"f".to_vec(), b"1".to_vec()),
            (b"f".to_vec(), b"2".to_vec())
        ]))
        .is_array());
    }

    #[test]
    fn json_errors() {
        assert!(value_from_json(&RedisType::List, &json!("x")).is_err());
        assert!(value_from_json(&RedisType::ZSet, &json!([{"member": "m"}])).is_err());
        assert!(value_from_json(&RedisType::String, &json!({"$base64": "!!"})).is_err());
        assert!(value_from_json(&RedisType::Other("ReJSON-RL".into()), &json!(1)).is_err());
        // Hand-written: numbers, [member, score] pairs, "inf".
        assert_eq!(
            value_from_json(&RedisType::List, &json!([1, true])).unwrap(),
            RedisValue::List(vec![b"1".to_vec(), b"true".to_vec()])
        );
        assert_eq!(
            value_from_json(&RedisType::ZSet, &json!([["m", "inf"]])).unwrap(),
            RedisValue::ZSet(vec![(b"m".to_vec(), f64::INFINITY)])
        );
    }

    fn entry(key: &[u8], kind: RedisType, ttl: Option<i64>, value: RedisValue) -> Entry {
        Entry {
            key: key.to_vec(),
            kind,
            ttl,
            value,
        }
    }

    #[test]
    fn csv_rows() {
        let s = entry(
            b"su:s",
            RedisType::String,
            None,
            RedisValue::String(b"a,b".to_vec()),
        );
        assert_eq!(csv_row(&s), ["su:s", "string", NULL_CELL, "a,b"]);
        let l = entry(
            b"su:l",
            RedisType::List,
            Some(60),
            RedisValue::List(vec![b"x".to_vec()]),
        );
        assert_eq!(csv_row(&l), ["su:l", "list", "60", r#"["x"]"#]);
        let bin = entry(
            &[0xff],
            RedisType::String,
            None,
            RedisValue::String(b"\n".to_vec()),
        );
        assert_eq!(csv_row(&bin), [r#""\xff""#, "string", NULL_CELL, r#""\n""#]);
        let text = csv_text(&[s.clone(), l.clone(), bin.clone()]);
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("key,type,ttl,value"));
        assert_eq!(lines.next(), Some("su:s,string,,\"a,b\""));
        assert_eq!(lines.next(), Some("su:l,list,60,\"[\"\"x\"\"]\""));
        let back: Vec<Entry> = parse_entries(&text, TransferFormat::Csv)
            .unwrap()
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!(back, [s, l, bin]);
    }

    #[test]
    fn csv_errors_are_per_row() {
        let text = "key,type,ttl,value\nk,list,,notjson\nk2,string,x,v\nk3,string,,ok\n";
        let entries = parse_entries(text, TransferFormat::Csv).unwrap();
        assert!(entries[0].is_err());
        assert!(entries[1].is_err());
        assert!(entries[2].is_ok());
        assert!(parse_entries("a,b\n", TransferFormat::Csv)
            .unwrap()
            .is_empty());
        assert!(parse_entries("a,b\n1,2\n", TransferFormat::Csv).unwrap()[0].is_err());
    }

    #[test]
    fn files_round_trip_in_every_format() {
        let entries: Vec<Entry> = all_values()
            .into_iter()
            .enumerate()
            .map(|(i, (kind, value))| {
                entry(format!("su_it:{i}").as_bytes(), kind, Some(i as i64), value)
            })
            .chain([entry(
                &[b's', 0xff],
                RedisType::String,
                None,
                RedisValue::String(b"\"quoted\"".to_vec()),
            )])
            .collect();
        for format in TransferFormat::ALL {
            let text = format_entries(&entries, format);
            let back: Vec<Entry> = parse_entries(&text, format)
                .unwrap()
                .into_iter()
                .map(Result::unwrap)
                .collect();
            assert_eq!(back, entries, "{format:?}\n{text}");
        }
    }
}
