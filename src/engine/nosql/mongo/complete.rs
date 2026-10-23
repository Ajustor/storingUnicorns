//! Completion of the MongoDB console and bars (plan 13): collections after
//! `db.`, methods after `db.coll.`, cursor methods after `find(…).`, and in
//! documents the sampled fields and the operators the position allows
//! (query, update, aggregation stage).
//!
//! Like the SQL completion, a suggestion replaces the word (letters,
//! digits, `_`) ending at the cursor: after a typed `$`, the inserted text
//! of a `$` item leaves its `$` out.

use crate::engine::nosql::CompletionContext;
use crate::engine::sql::completion::{Completions, Kind, Suggestion, TableRef};

const DB_METHODS: &[(&str, &str)] = &[
    ("getCollection", "collection par son nom"),
    ("createCollection", "créer une collection"),
    ("dropDatabase", "supprimer la base"),
    ("getCollectionNames", "liste des collections"),
    ("runCommand", "commande serveur"),
    ("adminCommand", "commande d'administration"),
];

const COLL_METHODS: &[(&str, &str)] = &[
    ("find", "documents"),
    ("findOne", "un document"),
    ("aggregate", "pipeline d'agrégation"),
    ("countDocuments", "nombre de documents"),
    ("estimatedDocumentCount", "nombre estimé"),
    ("distinct", "valeurs distinctes"),
    ("insertOne", "insérer un document"),
    ("insertMany", "insérer des documents"),
    ("updateOne", "modifier un document"),
    ("updateMany", "modifier des documents"),
    ("replaceOne", "remplacer un document"),
    ("deleteOne", "supprimer un document"),
    ("deleteMany", "supprimer des documents"),
    ("createIndex", "créer un index"),
    ("dropIndex", "supprimer un index"),
    ("getIndexes", "liste des index"),
    ("drop", "supprimer la collection"),
];

const CURSOR_METHODS: &[(&str, &str)] = &[
    ("sort", "tri"),
    ("limit", "nombre maximal"),
    ("skip", "documents ignorés"),
    ("projection", "champs retournés"),
    ("count", "nombre de documents"),
    ("toArray", "tableau"),
    ("pretty", "affichage"),
];

const QUERY_TOP_OPERATORS: &[&str] = &["$and", "$or", "$nor", "$expr", "$text", "$comment"];
const FIELD_OPERATORS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$regex",
    "$options",
    "$elemMatch",
    "$size",
    "$all",
    "$not",
    "$mod",
];
const UPDATE_OPERATORS: &[&str] = &[
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$rename",
    "$push",
    "$addToSet",
    "$pull",
    "$pullAll",
    "$pop",
    "$currentDate",
    "$setOnInsert",
];
pub(crate) const STAGES: &[&str] = &[
    "$match",
    "$project",
    "$group",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$lookup",
    "$addFields",
    "$set",
    "$unset",
    "$count",
    "$facet",
    "$replaceRoot",
    "$sample",
    "$out",
    "$merge",
];

/// A token of the text before the cursor.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Dot,
    /// A call closed at depth 0: method and first string argument.
    Called(String, Option<String>),
    /// `[…]` closed at depth 0, with its first string.
    Bracketed(Option<String>),
    Other,
}

#[derive(Debug)]
enum Frame {
    Call {
        method: String,
        arg: usize,
        first_str: Option<String>,
    },
    Obj {
        key_pos: bool,
        key: Option<String>,
    },
    Arr {
        first_str: Option<String>,
    },
}

/// Scan `text` (before the cursor): open brackets and the depth-0 tokens.
fn scan(text: &str) -> (Vec<Frame>, Vec<Tok>) {
    let b = text.as_bytes();
    let mut stack: Vec<Frame> = Vec::new();
    let mut top: Vec<Tok> = Vec::new();
    // Last token at the current level (the method before a `(`, the key
    // before a `:`).
    let mut last: Option<Tok> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            continue;
        }
        let tok = if c == b'"' || c == b'\'' {
            let start = i + 1;
            i += 1;
            while i < b.len() && b[i] != c && b[i] != b'\n' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            let s = text.get(start..i.min(b.len())).unwrap_or("").to_string();
            i = (i + 1).min(b.len());
            Tok::Str(s)
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                i += 1;
            }
            Tok::Ident(text[start..i].to_string())
        } else {
            i += 1;
            match c {
                b'.' => Tok::Dot,
                b'(' => {
                    let method = match &last {
                        Some(Tok::Ident(m)) => m.clone(),
                        _ => String::new(),
                    };
                    // The method becomes part of the `Called` token.
                    if stack.is_empty() && matches!(top.last(), Some(Tok::Ident(_))) {
                        top.pop();
                    }
                    stack.push(Frame::Call {
                        method,
                        arg: 0,
                        first_str: None,
                    });
                    last = None;
                    continue;
                }
                b'{' => {
                    stack.push(Frame::Obj {
                        key_pos: true,
                        key: None,
                    });
                    last = None;
                    continue;
                }
                b'[' => {
                    stack.push(Frame::Arr { first_str: None });
                    last = None;
                    continue;
                }
                b')' | b'}' | b']' => {
                    let frame = stack.pop();
                    let closed = match frame {
                        Some(Frame::Call {
                            method, first_str, ..
                        }) => Tok::Called(method, first_str),
                        Some(Frame::Arr { first_str }) => Tok::Bracketed(first_str),
                        _ => Tok::Other,
                    };
                    if stack.is_empty() {
                        top.push(closed.clone());
                    }
                    last = Some(closed);
                    continue;
                }
                b',' => {
                    match stack.last_mut() {
                        Some(Frame::Call { arg, .. }) => *arg += 1,
                        Some(Frame::Obj { key_pos, key }) => {
                            *key_pos = true;
                            *key = None;
                        }
                        _ => {}
                    }
                    last = None;
                    continue;
                }
                b':' => {
                    if let Some(Frame::Obj { key_pos, key }) = stack.last_mut() {
                        *key_pos = false;
                        *key = match &last {
                            Some(Tok::Ident(k) | Tok::Str(k)) => Some(k.clone()),
                            _ => None,
                        };
                    }
                    last = None;
                    continue;
                }
                b';' if stack.is_empty() => {
                    top.clear();
                    last = None;
                    continue;
                }
                _ => Tok::Other,
            }
        };
        if let Tok::Str(s) = &tok {
            match stack.last_mut() {
                Some(Frame::Call {
                    first_str, arg: 0, ..
                })
                | Some(Frame::Arr { first_str }) => {
                    first_str.get_or_insert_with(|| s.clone());
                }
                _ => {}
            }
        }
        if stack.is_empty() {
            top.push(tok.clone());
        }
        last = Some(tok);
    }
    (stack, top)
}

/// The collection named by depth-0 tokens starting with `db`, and the
/// position right after it.
fn collection_of(top: &[Tok]) -> Option<(String, usize)> {
    if top.first() != Some(&Tok::Ident("db".into())) {
        return None;
    }
    match top.get(1)? {
        Tok::Bracketed(Some(name)) => Some((name.clone(), 2)),
        Tok::Dot => match top.get(2)? {
            Tok::Called(m, Some(name)) if m == "getCollection" => Some((name.clone(), 3)),
            Tok::Ident(first) => {
                let mut name = first.clone();
                let mut i = 3;
                while let (Some(Tok::Dot), Some(Tok::Ident(seg))) = (top.get(i), top.get(i + 1)) {
                    name.push('.');
                    name.push_str(seg);
                    i += 2;
                }
                Some((name, i))
            }
            _ => None,
        },
        _ => None,
    }
}

/// What the object under the cursor holds.
#[derive(Debug, PartialEq)]
enum Role {
    /// Query: fields and query operators at the top, field operators
    /// under a field.
    Filter,
    Update,
    Pipeline,
    /// Fields only (projection, sort, index keys, inserted documents).
    Fields,
}

/// Which document an argument is.
fn arg_role(method: &str, arg: usize) -> Option<Role> {
    Some(match (method, arg) {
        (
            "find" | "findOne" | "countDocuments" | "count" | "deleteOne" | "deleteMany"
            | "updateOne" | "updateMany" | "replaceOne",
            0,
        )
        | ("distinct", 1) => Role::Filter,
        ("updateOne" | "updateMany", 1) => Role::Update,
        ("aggregate", 0) => Role::Pipeline,
        ("find" | "findOne", 1)
        | ("sort" | "projection" | "createIndex" | "insertOne" | "insertMany", 0)
        | ("replaceOne", 1) => Role::Fields,
        _ => return None,
    })
}

/// What to suggest at a key position inside a call's argument.
#[derive(Debug, PartialEq)]
enum KeyKind {
    Fields,
    FieldsAndQuery,
    FieldOperators,
    UpdateOperators,
    Stages,
    None,
}

/// The kind of key expected in the innermost object, from the steps from
/// the call's argument down to it: `Some(key)` = the value of `key`,
/// `None` = an element of an array.
fn key_kind(role: Role, steps: &[Option<String>]) -> KeyKind {
    fn filter(steps: &[Option<String>]) -> KeyKind {
        // Between `$or:` and its element object.
        let mut logical = false;
        let mut kind = KeyKind::FieldsAndQuery;
        for s in steps {
            kind = match (kind, logical, s.as_deref()) {
                (KeyKind::FieldsAndQuery, false, Some("$and" | "$or" | "$nor")) => {
                    logical = true;
                    KeyKind::FieldsAndQuery
                }
                (KeyKind::FieldsAndQuery, true, None) => {
                    logical = false;
                    KeyKind::FieldsAndQuery
                }
                (KeyKind::FieldsAndQuery, false, Some(k)) if !k.starts_with('$') => {
                    KeyKind::FieldOperators
                }
                (KeyKind::FieldOperators, false, Some("$not")) => KeyKind::FieldOperators,
                (KeyKind::FieldOperators, false, Some("$elemMatch")) => KeyKind::FieldsAndQuery,
                _ => return KeyKind::None,
            };
        }
        if logical {
            KeyKind::None
        } else {
            kind
        }
    }
    match role {
        Role::Filter => filter(steps),
        Role::Fields => match steps {
            // `{…}`, or an element of `insertMany([{…}])`.
            [] | [None] => KeyKind::Fields,
            _ => KeyKind::None,
        },
        Role::Update => match steps {
            [] => KeyKind::UpdateOperators,
            [Some(op)] if op.starts_with('$') => KeyKind::Fields,
            _ => KeyKind::None,
        },
        Role::Pipeline => match steps {
            [None] => KeyKind::Stages,
            [None, Some(stage), rest @ ..] => match stage.as_str() {
                "$match" => filter(rest),
                "$sort" | "$project" | "$addFields" | "$set" if rest.is_empty() => KeyKind::Fields,
                _ => KeyKind::None,
            },
            _ => KeyKind::None,
        },
    }
}

fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_' || b[0] == b'$')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'$')
}

struct Builder {
    word: String,
    dollar: bool,
    out: Completions,
}

impl Builder {
    fn matches(&self, label: &str) -> bool {
        let typed = if self.dollar {
            format!("${}", self.word)
        } else {
            self.word.clone()
        };
        label.to_lowercase().starts_with(&typed.to_lowercase())
    }

    fn push(&mut self, label: &str, detail: &str, insert: String, kind: Kind) {
        if self.matches(label) && !self.out.items.iter().any(|s| s.label == label) {
            self.out.items.push(Suggestion {
                label: label.to_string(),
                detail: detail.to_string(),
                insert,
                kind,
            });
        }
    }

    fn plain(&mut self, items: &[(&str, &str)], kind: Kind) {
        if self.dollar {
            return;
        }
        for (label, detail) in items {
            self.push(label, detail, label.to_string(), kind);
        }
    }

    fn operators(&mut self, items: &[&str], detail: &str) {
        for op in items {
            let insert = if self.dollar {
                op.trim_start_matches('$').to_string()
            } else {
                op.to_string()
            };
            self.push(op, detail, insert, Kind::Keyword);
        }
    }

    fn fields(&mut self, cx: &CompletionContext, coll: Option<&str>) {
        if self.dollar {
            return;
        }
        let Some(coll) = coll else { return };
        if cx.scope.database.is_empty() {
            return;
        }
        let table = TableRef {
            schema: Some(cx.scope.database.clone()),
            name: coll.to_string(),
            alias: None,
        };
        match (cx.columns)(&table) {
            Some(columns) => {
                for c in columns {
                    let insert = if is_ident(&c.name) {
                        c.name.clone()
                    } else {
                        super::shell::quote(&c.name)
                    };
                    let detail = if c.primary_key {
                        format!("PK {}", c.type_name)
                    } else {
                        c.type_name.clone()
                    };
                    self.push(&c.name, &detail, insert, Kind::Column);
                }
            }
            None => {
                if !self.out.missing.contains(&table) {
                    self.out.missing.push(table);
                }
            }
        }
    }

    fn collections(&mut self, cx: &CompletionContext, dotted_prefix: Option<&str>) {
        if self.dollar {
            return;
        }
        let Some(schema) = cx.schemas.iter().find(|s| s.name == cx.scope.database) else {
            return;
        };
        for coll in &schema.tables {
            match dotted_prefix {
                // `db.fs.` → `chunks` of `fs.chunks`.
                Some(prefix) => {
                    if let Some(rest) = coll.strip_prefix(prefix) {
                        if is_ident(&rest.replace('.', "_")) {
                            self.push(rest, "collection", rest.to_string(), Kind::Table);
                        }
                    }
                }
                None => {
                    let insert = if coll.split('.').all(is_ident) {
                        coll.clone()
                    } else {
                        format!("getCollection({})", super::shell::quote(coll))
                    };
                    self.push(coll, "collection", insert, Kind::Table);
                }
            }
        }
    }
}

/// Suggestions at `cursor` of a console script or a filter bar.
pub fn complete(text: &str, cursor: usize, cx: &CompletionContext) -> Completions {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let before = &text[..cursor];
    let word_start = before
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c.is_alphanumeric() || c == '_')
        .last()
        .map_or(cursor, |(i, _)| i);
    let word = before[word_start..].to_string();
    let dollar = before[..word_start].ends_with('$');
    let prefix_end = if dollar { word_start - 1 } else { word_start };
    // The unit under the cursor.
    // The last unit goes on to the cursor unless a separator (`;`, a line
    // break at depth 0) comes between.
    let unit_start = super::shell::units(&text[..prefix_end])
        .last()
        .filter(|r| {
            let between = &text[r.end..prefix_end];
            !scan(&text[r.start..r.end]).0.is_empty()
                || (!between.contains(';') && !between.contains('\n'))
        })
        .map_or(prefix_end, |r| r.start);
    let unit = &text[unit_start..prefix_end];
    let mut b = Builder {
        word,
        dollar,
        out: Completions::default(),
    };
    // Inside a string or a comment: nothing.
    if in_string_or_comment(unit) {
        return b.out;
    }
    let (stack, top) = scan(unit);
    if stack.is_empty() {
        statement(&mut b, &top, cx);
    } else {
        document(&mut b, &stack, &top, cx);
    }
    b.out
}

/// Whether the end of `text` is inside a string literal or a comment.
fn in_string_or_comment(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                if i >= b.len() {
                    return true;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => match text[i..].find('\n') {
                Some(n) => i += n,
                None => return true,
            },
            b'/' if b.get(i + 1) == Some(&b'*') => match text[i + 2..].find("*/") {
                Some(n) => i += 2 + n + 1,
                None => return true,
            },
            _ => {}
        }
        i += 1;
    }
    false
}

/// Completion at the statement level (no open bracket).
fn statement(b: &mut Builder, top: &[Tok], cx: &CompletionContext) {
    let ident = |s: &str| Tok::Ident(s.into());
    match top {
        [] => {
            b.plain(
                &[
                    ("db", "base courante"),
                    ("use", "changer de base"),
                    ("show", "dbs / collections"),
                ],
                Kind::Keyword,
            );
            return;
        }
        [t] if *t == ident("use") => {
            if !b.dollar {
                for s in cx.schemas {
                    b.push(&s.name, "base", s.name.clone(), Kind::Schema);
                }
            }
            return;
        }
        [t] if *t == ident("show") => {
            b.plain(
                &[("dbs", "bases"), ("collections", "collections")],
                Kind::Keyword,
            );
            return;
        }
        [t, Tok::Dot] if *t == ident("db") => {
            b.collections(cx, None);
            b.plain(DB_METHODS, Kind::Keyword);
            return;
        }
        _ => {}
    }
    if top.last() != Some(&Tok::Dot) {
        return;
    }
    let body = &top[..top.len() - 1];
    let Some((coll, end)) = collection_of(body) else {
        return;
    };
    let chain = &body[end..];
    if chain.is_empty() {
        // `db.fs.` may also continue a dotted collection name.
        if matches!(body.get(2), Some(Tok::Ident(_))) {
            b.collections(cx, Some(&format!("{coll}.")));
        }
        b.plain(COLL_METHODS, Kind::Keyword);
        return;
    }
    // `db.c.find(…).sort(…).` → cursor methods.
    let mut calls = chain.iter().filter(|t| **t != Tok::Dot);
    if let Some(Tok::Called(first, _)) = calls.next() {
        let cursor_ok = first == "find"
            && calls.all(|t| matches!(t, Tok::Called(m, _) if ["sort", "limit", "skip", "projection"].contains(&m.as_str())));
        if cursor_ok {
            b.plain(CURSOR_METHODS, Kind::Keyword);
        }
    }
}

/// Completion inside a call's arguments.
fn document(b: &mut Builder, stack: &[Frame], top: &[Tok], cx: &CompletionContext) {
    // The innermost call, and the brackets opened inside it.
    let Some(call_at) = stack.iter().rposition(|f| matches!(f, Frame::Call { .. })) else {
        return;
    };
    let Frame::Call { method, arg, .. } = &stack[call_at] else {
        return;
    };
    let inner = &stack[call_at + 1..];
    let Some(Frame::Obj { key_pos: true, .. }) = inner.last() else {
        return;
    };
    // The collection: from the statement when the call is at depth 0.
    let coll = if call_at == 0 {
        // `db.users.` + the open call.
        let body = match top {
            [rest @ .., Tok::Dot] => rest,
            _ => top,
        };
        collection_of(body).map(|(c, _)| c)
    } else {
        None
    };
    let Some(role) = arg_role(method, *arg) else {
        return;
    };
    match key_kind(role, &steps(inner)) {
        KeyKind::Fields => b.fields(cx, coll.as_deref()),
        KeyKind::FieldsAndQuery => {
            b.fields(cx, coll.as_deref());
            b.operators(QUERY_TOP_OPERATORS, "opérateur");
        }
        KeyKind::FieldOperators => b.operators(FIELD_OPERATORS, "opérateur"),
        KeyKind::UpdateOperators => b.operators(UPDATE_OPERATORS, "mise à jour"),
        KeyKind::Stages => b.operators(STAGES, "étape"),
        KeyKind::None => {}
    }
}

/// The steps from a call's argument (`inner[0]`) down to the innermost
/// bracket: `Some(key)` for the value of a key, `None` for an array element.
fn steps(inner: &[Frame]) -> Vec<Option<String>> {
    inner
        .windows(2)
        .map(|w| match &w[0] {
            Frame::Obj { key, .. } => Some(key.clone().unwrap_or_default()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::SchemaInfo;
    use crate::engine::nosql::NoSqlScope;
    use crate::engine::sql::completion::ColumnInfo;

    fn schemas() -> Vec<SchemaInfo> {
        vec![
            SchemaInfo {
                name: "shop".into(),
                tables: vec!["users".into(), "fs.chunks".into(), "my-coll".into()],
                expanded: false,
            },
            SchemaInfo {
                name: "admin".into(),
                tables: vec![],
                expanded: false,
            },
        ]
    }

    fn columns(t: &TableRef) -> Option<Vec<ColumnInfo>> {
        (t.schema.as_deref() == Some("shop") && t.name == "users").then(|| {
            ["_id", "name", "age", "address.city"]
                .iter()
                .map(|n| ColumnInfo {
                    name: n.to_string(),
                    type_name: "string".into(),
                    primary_key: *n == "_id",
                    foreign_key: None,
                })
                .collect()
        })
    }

    /// Completions at `|` in `text`.
    fn run(text: &str) -> Completions {
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        let scope = NoSqlScope {
            database: "shop".into(),
        };
        let schemas = schemas();
        let cx = CompletionContext {
            scope: &scope,
            schemas: &schemas,
            columns: &columns,
            keys: &[],
        };
        complete(&text, cursor, &cx)
    }

    fn labels(text: &str) -> Vec<String> {
        run(text).items.into_iter().map(|s| s.label).collect()
    }

    fn has(text: &str, label: &str) -> bool {
        labels(text).iter().any(|l| l == label)
    }

    #[test]
    fn after_db_dot_collections_and_methods() {
        let l = labels("db.|");
        assert!(l.contains(&"users".to_string()));
        assert!(l.contains(&"getCollection".to_string()));
        let items = run("db.my|").items;
        assert_eq!(items[0].label, "my-coll");
        assert_eq!(items[0].insert, "getCollection(\"my-coll\")");
        assert!(!has("db.u|", "getCollection"));
        assert!(has("db.u|", "users"));
        assert!(has("use shop\ndb.|", "users"));
    }

    #[test]
    fn after_a_collection_its_methods() {
        assert!(has("db.users.|", "find"));
        assert!(has("db.users.f|", "findOne"));
        assert!(!has("db.users.f|", "drop"));
        assert!(has("db.getCollection(\"my-coll\").|", "aggregate"));
        assert!(has("db[\"users\"].|", "insertOne"));
        assert!(has("db.fs.|", "chunks"));
        assert!(has("db.fs.|", "find"));
        assert!(has("db.users.find({}).|", "sort"));
        assert!(has("db.users.find({}).sort({a: 1}).|", "limit"));
        assert!(labels("db.users.insertOne({}).|").is_empty());
    }

    #[test]
    fn statement_start_use_and_show() {
        assert_eq!(labels("|"), ["db", "use", "show"]);
        assert!(has("use |", "shop"));
        assert!(has("use a\nuse |", "admin"));
        assert!(has("show |", "collections"));
    }

    #[test]
    fn filter_keys_are_fields_and_operators() {
        let l = labels("db.users.find({|");
        assert!(l.contains(&"name".to_string()) && l.contains(&"$or".to_string()));
        assert!(l.contains(&"address.city".to_string()));
        let city = run("db.users.find({addr|").items;
        assert_eq!(city[0].insert, "\"address.city\"");
        assert_eq!(labels("db.users.find({a|"), ["age", "address.city"]);
        assert!(has("db.users.find({name: \"x\", |", "age"));
        // Under a field: field operators.
        let ops = labels("db.users.find({age: {|");
        assert!(ops.contains(&"$gt".to_string()) && !ops.contains(&"name".to_string()));
        let g = run("db.users.find({age: {$g|").items;
        assert_eq!(
            g.iter().map(|s| s.insert.as_str()).collect::<Vec<_>>(),
            ["gt", "gte"]
        );
        // In $or elements: fields again.
        assert!(has("db.users.find({$or: [{|", "name"));
        assert!(has("db.users.deleteMany({|", "age"));
        assert!(has("db.users.countDocuments({|", "age"));
        // Values: nothing.
        assert!(labels("db.users.find({age: |").is_empty());
        assert!(labels("db.users.find({name: \"a|").is_empty());
    }

    #[test]
    fn update_documents_get_update_operators_then_fields() {
        let l = labels("db.users.updateOne({}, {|");
        assert!(l.contains(&"$set".to_string()) && !l.contains(&"name".to_string()));
        assert!(has("db.users.updateMany({}, {$set: {|", "name"));
        let ins = run("db.users.updateOne({}, {$|").items;
        assert!(ins.iter().any(|s| s.label == "$inc" && s.insert == "inc"));
    }

    #[test]
    fn aggregation_stages_and_match_fields() {
        let l = labels("db.users.aggregate([{|");
        assert!(l.contains(&"$match".to_string()) && l.contains(&"$group".to_string()));
        assert!(has("db.users.aggregate([{$match: {}}, {|", "$lookup"));
        assert!(has("db.users.aggregate([{$match: {|", "age"));
        assert!(has("db.users.aggregate([{$match: {age: {|", "$lte"));
        assert!(has("db.users.aggregate([{$sort: {|", "name"));
    }

    #[test]
    fn other_documents_get_fields() {
        assert!(has("db.users.find({}, {|", "name"));
        assert!(has("db.users.find().sort({|", "age"));
        assert!(has("db.users.createIndex({|", "age"));
        assert!(has("db.users.insertMany([{|", "name"));
    }

    #[test]
    fn unloaded_fields_are_reported_missing() {
        let out = run("db.orders.find({|");
        assert_eq!(
            out.missing,
            [TableRef {
                schema: Some("shop".into()),
                name: "orders".into(),
                alias: None
            }]
        );
        assert!(out.items.iter().all(|s| s.label.starts_with('$')));
    }
}
