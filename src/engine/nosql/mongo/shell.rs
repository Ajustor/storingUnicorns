//! MongoDB shell syntax (plan 13, « Syntaxe Mongo »): a hand-written parser
//! (no JavaScript) for the console, the filter / sort / projection bars and
//! the JSON editor, and its exact inverse printer.

use std::ops::Range;

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, Document, Regex, Timestamp};

/// A syntax error, in French, at byte `offset` of the parsed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub offset: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (position {})", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

/// An update: a document (`{$set: …}`) or an aggregation pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateSpec {
    Document(Document),
    Pipeline(Vec<Document>),
}

/// One console command. `coll` is the collection name in the scope's
/// database.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoCommand {
    Use(String),
    ShowDbs,
    ShowCollections,
    Find {
        coll: String,
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
        /// `findOne`.
        one: bool,
    },
    Aggregate {
        coll: String,
        pipeline: Vec<Document>,
        options: Option<Document>,
    },
    /// `countDocuments`, `find(…).count()`.
    Count {
        coll: String,
        filter: Document,
    },
    EstimatedCount {
        coll: String,
    },
    Distinct {
        coll: String,
        field: String,
        filter: Document,
    },
    InsertOne {
        coll: String,
        doc: Document,
    },
    InsertMany {
        coll: String,
        docs: Vec<Document>,
        options: Option<Document>,
    },
    Update {
        coll: String,
        filter: Document,
        update: UpdateSpec,
        many: bool,
        upsert: bool,
    },
    ReplaceOne {
        coll: String,
        filter: Document,
        replacement: Document,
        upsert: bool,
    },
    Delete {
        coll: String,
        filter: Document,
        many: bool,
    },
    CreateIndex {
        coll: String,
        keys: Document,
        options: Option<Document>,
    },
    /// By name (string) or by keys (document).
    DropIndex {
        coll: String,
        index: Bson,
    },
    GetIndexes {
        coll: String,
    },
    CreateCollection {
        name: String,
        options: Option<Document>,
    },
    DropCollection {
        coll: String,
    },
    DropDatabase,
    GetCollectionNames,
    /// `db.runCommand` (`admin: false`) or `db.adminCommand` (`admin: true`).
    RunCommand {
        command: Document,
        admin: bool,
    },
}

/// A parsed unit and its byte range in the script.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    pub command: MongoCommand,
    pub span: Range<usize>,
}

/// Options accepted by `aggregate`.
const AGGREGATE_OPTIONS: &[&str] = &[
    "allowDiskUse",
    "maxTimeMS",
    "batchSize",
    "collation",
    "hint",
    "comment",
    "let",
    "bypassDocumentValidation",
    "readConcern",
    "writeConcern",
];
/// Options accepted by `createIndex`.
pub(crate) const INDEX_OPTIONS: &[&str] = &[
    "name",
    "unique",
    "sparse",
    "expireAfterSeconds",
    "partialFilterExpression",
    "hidden",
];

/// Keys that make an object an extended JSON value (`{$oid: "…"}`). The
/// legacy `$regex` form is left out: it is also a query operator.
const EXTJSON_KEYS: &[&str] = &[
    "$oid",
    "$date",
    "$numberInt",
    "$numberLong",
    "$numberDouble",
    "$numberDecimal",
    "$binary",
    "$uuid",
    "$timestamp",
    "$regularExpression",
    "$minKey",
    "$maxKey",
    "$symbol",
    "$code",
    "$dbPointer",
    "$undefined",
];

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A parsed value before conversion to BSON: objects stay raw so that
/// extended JSON objects can be handed to `bson` whole.
#[derive(Clone, Debug)]
enum Val {
    Obj(Vec<(String, Val)>, usize),
    Arr(Vec<Val>),
    Str(String),
    /// An integer literal (Int32 when it fits, else Int64).
    Int(i64),
    Num(f64),
    Bool(bool),
    Null,
    /// A constructor's value (`ObjectId("…")`, `/re/`…).
    B(Bson),
}

impl Val {
    fn into_bson(self) -> Result<Bson, ParseError> {
        Ok(match self {
            Val::Obj(fields, offset) => {
                if fields
                    .first()
                    .is_some_and(|(k, _)| EXTJSON_KEYS.contains(&k.as_str()))
                {
                    let json = Val::Obj(fields, offset).into_json();
                    return Bson::try_from(json).map_err(|e| ParseError {
                        message: format!("JSON étendu invalide : {e}"),
                        offset,
                    });
                }
                let mut doc = Document::new();
                for (k, v) in fields {
                    doc.insert(k, v.into_bson()?);
                }
                Bson::Document(doc)
            }
            Val::Arr(items) => Bson::Array(
                items
                    .into_iter()
                    .map(Val::into_bson)
                    .collect::<Result<_, _>>()?,
            ),
            Val::Str(s) => Bson::String(s),
            Val::Int(i) => int_bson(i),
            Val::Num(f) => Bson::Double(f),
            Val::Bool(b) => Bson::Boolean(b),
            Val::Null => Bson::Null,
            Val::B(b) => b,
        })
    }

    /// Plain JSON of the value (for extended JSON objects).
    fn into_json(self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Val::Obj(fields, _) => J::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| (k, v.into_json()))
                    .collect(),
            ),
            Val::Arr(items) => J::Array(items.into_iter().map(Val::into_json).collect()),
            Val::Str(s) => J::String(s),
            Val::Int(i) => J::from(i),
            Val::Num(f) => serde_json::Number::from_f64(f)
                .map_or_else(|| Bson::Double(f).into_canonical_extjson(), J::Number),
            Val::Bool(b) => J::Bool(b),
            Val::Null => J::Null,
            Val::B(b) => b.into_canonical_extjson(),
        }
    }
}

fn int_bson(i: i64) -> Bson {
    match i32::try_from(i) {
        Ok(i) => Bson::Int32(i),
        Err(_) => Bson::Int64(i),
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// The parser of one text (a value, or a console unit); `base` is added
/// to the error offsets.
struct Parser<'a> {
    src: &'a str,
    pos: usize,
    base: usize,
}

type PResult<T> = Result<T, ParseError>;

impl<'a> Parser<'a> {
    fn new(src: &'a str, base: usize) -> Self {
        Self { src, pos: 0, base }
    }

    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes().get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.bytes().get(self.pos + n).copied()
    }

    fn err_at<T>(&self, offset: usize, message: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            message: message.into(),
            offset: self.base + offset,
        })
    }

    fn err<T>(&self, message: impl Into<String>) -> PResult<T> {
        self.err_at(self.pos, message)
    }

    /// Skip blanks and comments.
    fn skip_ws(&mut self) -> PResult<()> {
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_whitespace() => self.pos += 1,
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while self.peek().is_some_and(|c| c != b'\n') {
                        self.pos += 1;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.pos;
                    match self.src[self.pos + 2..].find("*/") {
                        Some(end) => self.pos += 2 + end + 2,
                        None => return self.err_at(start, "Commentaire non fermé"),
                    }
                }
                _ => {
                    // Non-ASCII blanks (no-break space…).
                    match self.src[self.pos..].chars().next() {
                        Some(c) if c.is_whitespace() => self.pos += c.len_utf8(),
                        _ => return Ok(()),
                    }
                }
            }
        }
    }

    fn at_end(&mut self) -> PResult<bool> {
        self.skip_ws()?;
        Ok(self.pos >= self.src.len())
    }

    fn eat(&mut self, c: u8) -> PResult<bool> {
        self.skip_ws()?;
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, c: u8) -> PResult<()> {
        if self.eat(c)? {
            Ok(())
        } else {
            self.err(format!("« {} » attendu", c as char))
        }
    }

    fn ident(&mut self) -> Option<&'a str> {
        let start = self.pos;
        if !self.peek().is_some_and(is_ident_start) {
            return None;
        }
        while self.peek().is_some_and(is_ident_char) {
            self.pos += 1;
        }
        Some(&self.src[start..self.pos])
    }

    fn value(&mut self) -> PResult<Val> {
        self.skip_ws()?;
        let start = self.pos;
        match self.peek() {
            None => self.err("Valeur attendue"),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(q @ (b'"' | b'\'')) => Ok(Val::Str(self.string(q)?)),
            Some(b'/') => self.regex(),
            Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') => self.number(),
            Some(c) if is_ident_start(c) => {
                let word = self.ident().unwrap_or_default();
                match word {
                    "true" => Ok(Val::Bool(true)),
                    "false" => Ok(Val::Bool(false)),
                    "null" => Ok(Val::Null),
                    "undefined" => Ok(Val::B(Bson::Undefined)),
                    "Infinity" => Ok(Val::Num(f64::INFINITY)),
                    "NaN" => Ok(Val::Num(f64::NAN)),
                    "new" => {
                        self.skip_ws()?;
                        let at = self.pos;
                        match self.ident() {
                            Some(name) => self.constructor(name, at),
                            None => self.err("Constructeur attendu après « new »"),
                        }
                    }
                    _ => {
                        self.skip_ws()?;
                        if self.peek() == Some(b'(') {
                            self.constructor(word, start)
                        } else {
                            self.err_at(start, format!("Valeur inconnue : {word}"))
                        }
                    }
                }
            }
            Some(_) => self.err("Valeur attendue"),
        }
    }

    fn object(&mut self) -> PResult<Val> {
        let start = self.pos;
        self.pos += 1;
        let mut fields: Vec<(String, Val)> = Vec::new();
        loop {
            self.skip_ws()?;
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Val::Obj(fields, self.base + start));
                }
                None => return self.err_at(start, "Objet non fermé : « } » attendu"),
                _ => {}
            }
            let key = match self.peek() {
                Some(q @ (b'"' | b'\'')) => self.string(q)?,
                Some(c) if is_ident_char(c) => {
                    let s = self.pos;
                    while self.peek().is_some_and(is_ident_char) {
                        self.pos += 1;
                    }
                    self.src[s..self.pos].to_string()
                }
                _ => return self.err("Nom de champ attendu"),
            };
            self.expect(b':')?;
            let value = self.value()?;
            fields.retain(|(k, _)| *k != key);
            fields.push((key, value));
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {}
                None => return self.err_at(start, "Objet non fermé : « } » attendu"),
                _ => return self.err("« , » ou « } » attendu"),
            }
        }
    }

    fn array(&mut self) -> PResult<Val> {
        let start = self.pos;
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_ws()?;
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Val::Arr(items));
                }
                None => return self.err_at(start, "Tableau non fermé : « ] » attendu"),
                _ => {}
            }
            items.push(self.value()?);
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {}
                None => return self.err_at(start, "Tableau non fermé : « ] » attendu"),
                _ => return self.err("« , » ou « ] » attendu"),
            }
        }
    }

    fn string(&mut self, quote: u8) -> PResult<String> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.src[self.pos..].chars().next() else {
                return self.err_at(start, "Chaîne non fermée");
            };
            if c == '\n' {
                return self.err_at(start, "Chaîne non fermée");
            }
            self.pos += c.len_utf8();
            if c as u32 == quote as u32 {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let esc_at = self.pos - 1;
            let Some(e) = self.src[self.pos..].chars().next() else {
                return self.err_at(start, "Chaîne non fermée");
            };
            self.pos += e.len_utf8();
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '0' if !self.peek().is_some_and(|c| c.is_ascii_digit()) => out.push('\0'),
                '\\' | '\'' | '"' | '/' => out.push(e),
                'x' => {
                    let code = self.hex_digits(2, esc_at)?;
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
                'u' => {
                    let hi = self.hex_digits(4, esc_at)?;
                    let ch = if (0xD800..0xDC00).contains(&hi)
                        && self.src[self.pos..].starts_with("\\u")
                    {
                        let save = self.pos;
                        self.pos += 2;
                        let lo = self.hex_digits(4, esc_at)?;
                        if (0xDC00..0xE000).contains(&lo) {
                            char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                        } else {
                            self.pos = save;
                            None
                        }
                    } else {
                        char::from_u32(hi)
                    };
                    out.push(ch.unwrap_or('\u{fffd}'));
                }
                _ => return self.err_at(esc_at, format!("Échappement inconnu : \\{e}")),
            }
        }
    }

    fn hex_digits(&mut self, n: usize, at: usize) -> PResult<u32> {
        let digits = self.src.get(self.pos..self.pos + n).unwrap_or("");
        match u32::from_str_radix(digits, 16) {
            Ok(v) if digits.len() == n && digits.bytes().all(|b| b.is_ascii_hexdigit()) => {
                self.pos += n;
                Ok(v)
            }
            _ => self.err_at(at, "Échappement hexadécimal invalide"),
        }
    }

    fn number(&mut self) -> PResult<Val> {
        let start = self.pos;
        let negative = match self.peek() {
            Some(b'-') => {
                self.pos += 1;
                true
            }
            Some(b'+') => {
                self.pos += 1;
                false
            }
            _ => false,
        };
        if self.src[self.pos..].starts_with("Infinity") {
            self.pos += "Infinity".len();
            return Ok(Val::Num(if negative {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }));
        }
        if self.src[self.pos..].starts_with("NaN") {
            self.pos += 3;
            return Ok(Val::Num(f64::NAN));
        }
        let rest = &self.src[self.pos..];
        if rest.starts_with("0x") || rest.starts_with("0X") {
            self.pos += 2;
            let s = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            let digits = &self.src[s..self.pos];
            return match i64::from_str_radix(digits, 16) {
                Ok(v) => Ok(Val::Int(if negative { -v } else { v })),
                Err(_) => self.err_at(start, "Nombre hexadécimal invalide"),
            };
        }
        let digits_start = self.pos;
        let mut float = false;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            float = true;
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if self.pos == digits_start || &self.src[digits_start..self.pos] == "." {
            return self.err_at(start, "Nombre invalide");
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                float = true;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        if self.peek().is_some_and(is_ident_char) {
            return self.err_at(start, "Nombre invalide");
        }
        let text = &self.src[digits_start..self.pos];
        if !float {
            if let Ok(v) = text.parse::<i64>() {
                return Ok(Val::Int(if negative { -v } else { v }));
            }
            // -9223372036854775808
            if negative {
                if let Ok(v) = format!("-{text}").parse::<i64>() {
                    return Ok(Val::Int(v));
                }
            }
        }
        match text.parse::<f64>() {
            Ok(v) => Ok(Val::Num(if negative { -v } else { v })),
            Err(_) => self.err_at(start, "Nombre invalide"),
        }
    }

    fn regex(&mut self) -> PResult<Val> {
        let start = self.pos;
        self.pos += 1;
        let mut in_class = false;
        let pattern_start = self.pos;
        loop {
            match self.peek() {
                None | Some(b'\n') => return self.err_at(start, "Expression régulière non fermée"),
                Some(b'\\') => {
                    self.pos += 1;
                    if let Some(c) = self.src[self.pos..].chars().next() {
                        if c == '\n' {
                            return self.err_at(start, "Expression régulière non fermée");
                        }
                        self.pos += c.len_utf8();
                    }
                }
                Some(b'[') => {
                    in_class = true;
                    self.pos += 1;
                }
                Some(b']') => {
                    in_class = false;
                    self.pos += 1;
                }
                Some(b'/') if !in_class => break,
                Some(_) => {
                    let c = self.src[self.pos..].chars().next().unwrap_or(' ');
                    self.pos += c.len_utf8();
                }
            }
        }
        let pattern = self.src[pattern_start..self.pos].to_string();
        self.pos += 1;
        let flags_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        let flags = &self.src[flags_start..self.pos];
        make_regex(&pattern, flags)
            .map(Val::B)
            .or_else(|m| self.err_at(start, m))
    }

    fn args(&mut self) -> PResult<Vec<(Val, usize)>> {
        self.expect(b'(')?;
        let open = self.pos - 1;
        let mut args = Vec::new();
        loop {
            self.skip_ws()?;
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    return Ok(args);
                }
                None => return self.err_at(open, "Parenthèse non fermée : « ) » attendu"),
                _ => {}
            }
            let at = self.pos;
            args.push((self.value()?, self.base + at));
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b')') => {}
                None => return self.err_at(open, "Parenthèse non fermée : « ) » attendu"),
                _ => return self.err("« , » ou « ) » attendu"),
            }
        }
    }

    fn constructor(&mut self, name: &str, at: usize) -> PResult<Val> {
        let args = self.args()?;
        let at = self.base + at;
        let fail = |m: String| -> PResult<Val> {
            Err(ParseError {
                message: m,
                offset: at,
            })
        };
        let arg_str = |i: usize| -> Option<String> {
            match args.get(i) {
                Some((Val::Str(s), _)) => Some(s.clone()),
                _ => None,
            }
        };
        let arg_int = |i: usize| -> Option<i64> {
            match args.get(i) {
                Some((v, _)) => val_int(v),
                None => None,
            }
        };
        let n = args.len();
        let value = match name {
            "ObjectId" => match (n, arg_str(0)) {
                (0, _) => Bson::ObjectId(bson::oid::ObjectId::new()),
                (1, Some(hex)) => match bson::oid::ObjectId::parse_str(&hex) {
                    Ok(oid) => Bson::ObjectId(oid),
                    Err(_) => return fail(format!("ObjectId invalide : « {hex} »")),
                },
                _ => return fail("ObjectId : une chaîne hexadécimale attendue".into()),
            },
            "ISODate" | "Date" => match (n, args.first()) {
                (0, _) => Bson::DateTime(bson::DateTime::now()),
                (1, Some((Val::Str(s), _))) => match parse_iso_date(s) {
                    Some(d) => Bson::DateTime(d),
                    None => return fail(format!("Date invalide : « {s} »")),
                },
                (1, Some((v, _))) if val_int(v).is_some() => {
                    Bson::DateTime(bson::DateTime::from_millis(val_int(v).unwrap_or(0)))
                }
                _ => {
                    return fail(format!(
                        "{name} : une date ISO 8601 ou des millisecondes attendues"
                    ))
                }
            },
            "NumberLong" => match (n, arg_int(0), arg_str(0)) {
                (1, Some(v), _) => Bson::Int64(v),
                (1, _, Some(s)) => match s.trim().parse::<i64>() {
                    Ok(v) => Bson::Int64(v),
                    Err(_) => return fail(format!("NumberLong invalide : « {s} »")),
                },
                _ => return fail("NumberLong : un entier attendu".into()),
            },
            "NumberInt" => {
                let v = match (n, arg_int(0), arg_str(0)) {
                    (1, Some(v), _) => Some(v),
                    (1, _, Some(s)) => s.trim().parse::<i64>().ok(),
                    _ => None,
                };
                match v.and_then(|v| i32::try_from(v).ok()) {
                    Some(v) => Bson::Int32(v),
                    None => return fail("NumberInt : un entier 32 bits attendu".into()),
                }
            }
            "NumberDecimal" | "Decimal128" => {
                let text = match args.first() {
                    Some((Val::Str(s), _)) if n == 1 => s.clone(),
                    Some((Val::Int(i), _)) if n == 1 => i.to_string(),
                    Some((Val::Num(f), _)) if n == 1 => f.to_string(),
                    _ => return fail(format!("{name} : un nombre décimal attendu")),
                };
                match text.trim().parse::<bson::Decimal128>() {
                    Ok(d) => Bson::Decimal128(d),
                    Err(_) => return fail(format!("Nombre décimal invalide : « {text} »")),
                }
            }
            "UUID" => match (n, arg_str(0)) {
                (0, _) => Bson::Binary(Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes: bson::uuid::Uuid::new().bytes().to_vec(),
                }),
                (1, Some(s)) => match parse_uuid(&s) {
                    Some(bytes) => Bson::Binary(Binary {
                        subtype: BinarySubtype::Uuid,
                        bytes,
                    }),
                    None => return fail(format!("UUID invalide : « {s} »")),
                },
                _ => return fail("UUID : une chaîne attendue".into()),
            },
            "BinData" => match (n, arg_int(0), arg_str(1)) {
                (2, Some(sub), Some(b64)) if (0..=255).contains(&sub) => {
                    use base64::Engine as _;
                    match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
                        Ok(bytes) => Bson::Binary(Binary {
                            subtype: BinarySubtype::from(sub as u8),
                            bytes,
                        }),
                        Err(_) => return fail("BinData : base64 invalide".into()),
                    }
                }
                _ => return fail("BinData : (sous-type, \"base64\") attendus".into()),
            },
            "Timestamp" => {
                let (t, i) = match args.as_slice() {
                    [(a, _), (b, _)] => (val_int(a), val_int(b)),
                    [(Val::Obj(fields, _), _)] => {
                        let get = |k: &str| {
                            fields
                                .iter()
                                .find(|(f, _)| f == k)
                                .and_then(|(_, v)| val_int(v))
                        };
                        (get("t"), get("i"))
                    }
                    _ => (None, None),
                };
                match (
                    t.and_then(|t| u32::try_from(t).ok()),
                    i.and_then(|i| u32::try_from(i).ok()),
                ) {
                    (Some(time), Some(increment)) => Bson::Timestamp(Timestamp { time, increment }),
                    _ => return fail("Timestamp : (secondes, incrément) attendus".into()),
                }
            }
            "MinKey" if n == 0 => Bson::MinKey,
            "MaxKey" if n == 0 => Bson::MaxKey,
            "RegExp" => match (n, arg_str(0), arg_str(1)) {
                (1, Some(p), _) => return make_regex(&p, "").map(Val::B).or_else(fail),
                (2, Some(p), Some(f)) => return make_regex(&p, &f).map(Val::B).or_else(fail),
                _ => return fail("RegExp : (\"motif\", \"options\") attendus".into()),
            },
            "MinKey" | "MaxKey" => return fail(format!("{name} : aucun argument attendu")),
            _ => return fail(format!("Constructeur inconnu : {name}")),
        };
        Ok(Val::B(value))
    }
}

fn val_int(v: &Val) -> Option<i64> {
    match v {
        Val::Int(i) => Some(*i),
        Val::Num(f) if f.fract() == 0.0 && f.abs() < 9.2e18 => Some(*f as i64),
        Val::B(Bson::Int32(i)) => Some(i64::from(*i)),
        Val::B(Bson::Int64(i)) => Some(*i),
        _ => None,
    }
}

fn make_regex(pattern: &str, flags: &str) -> Result<Bson, String> {
    if let Some(c) = flags.chars().find(|c| !"imxsul".contains(*c)) {
        return Err(format!("Option d'expression régulière inconnue : {c}"));
    }
    let mut chars: Vec<char> = flags.chars().collect();
    chars.sort_unstable();
    chars.dedup();
    let options: String = chars.into_iter().collect();
    let pattern = bson::raw::CString::try_from(pattern.to_string())
        .map_err(|_| "Expression régulière invalide (caractère nul)".to_string())?;
    let options = bson::raw::CString::try_from(options)
        .map_err(|_| "Expression régulière invalide".to_string())?;
    Ok(Bson::RegularExpression(Regex { pattern, options }))
}

fn parse_uuid(s: &str) -> Option<Vec<u8>> {
    let hex: String = s.trim().chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    decode_hex(&hex)
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An ISO 8601 date (`2024-01-02T03:04:05.678Z`, an offset, no zone =
/// UTC, or a date alone).
pub(crate) fn parse_iso_date(s: &str) -> Option<bson::DateTime> {
    use chrono::{NaiveDate, NaiveDateTime};
    let s = s.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(bson::DateTime::from_millis(d.timestamp_millis()));
    }
    for f in [
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%dT%H:%M%#z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
    ] {
        if let Ok(d) = chrono::DateTime::parse_from_str(s, f) {
            return Some(bson::DateTime::from_millis(d.timestamp_millis()));
        }
    }
    let naive = s.strip_suffix('Z').unwrap_or(s);
    for f in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(d) = NaiveDateTime::parse_from_str(naive, f) {
            return Some(bson::DateTime::from_millis(d.and_utc().timestamp_millis()));
        }
    }
    let d = NaiveDate::parse_from_str(naive, "%Y-%m-%d").ok()?;
    Some(bson::DateTime::from_millis(
        d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis(),
    ))
}

/// `2024-01-02T03:04:05.678Z`; `None` outside years 0–9999.
pub(crate) fn iso_date(d: bson::DateTime) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp_millis(d.timestamp_millis())?;
    use chrono::Datelike;
    (0..=9999)
        .contains(&dt.year())
        .then(|| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// One value in shell syntax (relaxed JSON, constructors, extended JSON).
pub fn parse_value(text: &str) -> Result<Bson, ParseError> {
    let mut p = Parser::new(text, 0);
    let v = p.value()?;
    if !p.at_end()? {
        return p.err("Texte inattendu après la valeur");
    }
    v.into_bson()
}

/// A document in shell syntax; "" (blank) = `{}`.
pub fn parse_document(text: &str) -> Result<Document, ParseError> {
    let mut p = Parser::new(text, 0);
    if p.at_end()? {
        return Ok(Document::new());
    }
    let start = p.pos;
    match parse_value(text)? {
        Bson::Document(d) => Ok(d),
        _ => Err(ParseError {
            message: "Document attendu : { … }".into(),
            offset: start,
        }),
    }
}

// ---------------------------------------------------------------------------
// Printing
// ---------------------------------------------------------------------------

/// `value` in shell syntax, exact inverse of `parse_value`; indented when
/// `pretty`.
pub fn to_shell(value: &Bson, pretty: bool) -> String {
    let mut out = String::new();
    write_value(&mut out, value, pretty, 0);
    out
}

/// A document key: bare when it is an identifier, else quoted.
pub(crate) fn shell_key(key: &str) -> String {
    let b = key.as_bytes();
    if !b.is_empty() && is_ident_start(b[0]) && b.iter().all(|c| is_ident_char(*c)) {
        key.to_string()
    } else {
        quote(key)
    }
}

/// A double-quoted string literal.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(crate) fn fmt_double(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        format!("{f:?}")
    }
}

/// Whether `/pattern/` reads back as `pattern` (else `RegExp("…")`).
fn regex_literal_safe(pattern: &str) -> bool {
    if pattern.is_empty() || pattern.starts_with('*') {
        return false;
    }
    let mut in_class = false;
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => return false,
            '\\' => match chars.next() {
                None | Some('\n') | Some('\r') => return false,
                _ => {}
            },
            '[' => in_class = true,
            ']' => in_class = false,
            '/' => return false,
            _ => {}
        }
    }
    !in_class
}

fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Bson, pretty: bool, depth: usize) {
    match value {
        Bson::Double(f) => out.push_str(&fmt_double(*f)),
        Bson::String(s) => out.push_str(&quote(s)),
        Bson::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    newline(out, depth + 1);
                }
                write_value(out, item, pretty, depth + 1);
            }
            if pretty {
                newline(out, depth);
            }
            out.push(']');
        }
        Bson::Document(doc) => write_document(out, doc, pretty, depth),
        Bson::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Bson::Null => out.push_str("null"),
        Bson::RegularExpression(r) => {
            let (p, o) = (r.pattern.as_str(), r.options.as_str());
            if regex_literal_safe(p) {
                out.push_str(&format!("/{p}/{o}"));
            } else if o.is_empty() {
                out.push_str(&format!("RegExp({})", quote(p)));
            } else {
                out.push_str(&format!("RegExp({}, {})", quote(p), quote(o)));
            }
        }
        Bson::Int32(i) => out.push_str(&i.to_string()),
        Bson::Int64(i) => {
            if i.unsigned_abs() <= 1 << 53 {
                out.push_str(&format!("NumberLong({i})"));
            } else {
                out.push_str(&format!("NumberLong(\"{i}\")"));
            }
        }
        Bson::Timestamp(t) => out.push_str(&format!("Timestamp({}, {})", t.time, t.increment)),
        Bson::Binary(b) => {
            if b.subtype == BinarySubtype::Uuid && b.bytes.len() == 16 {
                let h = encode_hex(&b.bytes);
                out.push_str(&format!(
                    "UUID(\"{}-{}-{}-{}-{}\")",
                    &h[0..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..32]
                ));
            } else {
                use base64::Engine as _;
                let sub: u8 = b.subtype.into();
                out.push_str(&format!(
                    "BinData({sub}, \"{}\")",
                    base64::engine::general_purpose::STANDARD.encode(&b.bytes)
                ));
            }
        }
        Bson::ObjectId(oid) => out.push_str(&format!("ObjectId(\"{}\")", oid.to_hex())),
        Bson::DateTime(d) => match iso_date(*d) {
            Some(iso) => out.push_str(&format!("ISODate(\"{iso}\")")),
            None => out.push_str(&format!("new Date({})", d.timestamp_millis())),
        },
        Bson::Decimal128(d) => out.push_str(&format!("NumberDecimal(\"{d}\")")),
        Bson::MinKey => out.push_str("MinKey()"),
        Bson::MaxKey => out.push_str("MaxKey()"),
        Bson::JavaScriptCode(code) => out.push_str(&format!("{{$code: {}}}", quote(code))),
        Bson::JavaScriptCodeWithScope(c) => {
            out.push_str(&format!("{{$code: {}, $scope: ", quote(&c.code)));
            write_document(out, &c.scope, false, depth);
            out.push('}');
        }
        Bson::Symbol(s) => out.push_str(&format!("{{$symbol: {}}}", quote(s))),
        Bson::Undefined => out.push_str("undefined"),
        // DbPointer's fields are private: its extended JSON is valid shell.
        other => out.push_str(&other.clone().into_canonical_extjson().to_string()),
    }
}

fn write_document(out: &mut String, doc: &Document, pretty: bool, depth: usize) {
    if doc.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push('{');
    for (i, (k, v)) in doc.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if pretty {
            newline(out, depth + 1);
        } else if i > 0 {
            out.push(' ');
        }
        out.push_str(&shell_key(k));
        out.push_str(": ");
        write_value(out, v, pretty, depth + 1);
    }
    if pretty {
        newline(out, depth);
    }
    out.push('}');
}

/// A document in shell syntax (compact).
pub(crate) fn doc_shell(doc: &Document) -> String {
    let mut out = String::new();
    write_document(&mut out, doc, false, 0);
    out
}

// ---------------------------------------------------------------------------
// Scripts
// ---------------------------------------------------------------------------

/// Byte ranges of the units of a script: split on `;` and on line breaks
/// at depth 0, unless the next line starts with `.` (a chained call).
/// Comments and blanks around a unit are left out; empty units skipped.
pub fn units(text: &str) -> Vec<Range<usize>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut depth: usize = 0;
    let mut start: Option<usize> = None;
    let mut end = 0;
    // Last significant byte: tells a regex from a division.
    let mut prev: Option<u8> = None;
    let mut i = 0;
    let push = |out: &mut Vec<Range<usize>>, start: &mut Option<usize>, end: usize| {
        if let Some(s) = start.take() {
            out.push(s..end);
        }
    };
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                if depth == 0 && start.is_some() && !continues(b, i + 1) {
                    push(&mut out, &mut start, end);
                    prev = None;
                }
                i += 1;
                continue;
            }
            c if c.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                continue;
            }
            b';' if depth == 0 => {
                push(&mut out, &mut start, end);
                prev = None;
                i += 1;
                continue;
            }
            _ => {}
        }
        let tok_start = i;
        match c {
            b'"' | b'\'' => {
                i += 1;
                while i < b.len() && b[i] != c && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                // The closing delimiter (an unclosed one stops at the line break).
                if i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if prev.is_none_or(|p| b"(,=:[!&|?{};+-*%<>~^".contains(&p)) => {
                i += 1;
                let mut class = false;
                while i < b.len() && b[i] != b'\n' {
                    match b[i] {
                        b'\\' => i += 1,
                        b'[' => class = true,
                        b']' => class = false,
                        b'/' if !class => break,
                        _ => {}
                    }
                    i += 1;
                }
                // The closing delimiter (an unclosed one stops at the line break).
                if i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => {
                i += 1;
                while i < b.len() && !text.is_char_boundary(i) {
                    i += 1;
                }
            }
        }
        start.get_or_insert(tok_start);
        end = i.min(b.len());
        prev = Some(c);
    }
    push(&mut out, &mut start, end);
    out
}

/// Whether the text from `i` (after a line break) continues the unit: its
/// first significant character is `.`.
fn continues(b: &[u8], mut i: usize) -> bool {
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let rest = &b[i + 2..];
                i = rest
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2);
            }
            b'.' => return !b.get(i + 1).is_some_and(|c| c.is_ascii_digit()),
            _ => return false,
        }
    }
    false
}

/// Every unit of a script, parsed (or its error).
pub fn parse_script(text: &str) -> Vec<Result<Parsed, ParseError>> {
    units(text)
        .into_iter()
        .map(|span| {
            parse_command(&text[span.clone()], span.start).map(|command| Parsed { command, span })
        })
        .collect()
}

/// Parse one unit; error offsets are shifted by `base`.
pub(crate) fn parse_command(unit: &str, base: usize) -> Result<MongoCommand, ParseError> {
    let mut p = Parser::new(unit, base);
    p.skip_ws()?;
    let at = p.pos;
    let Some(word) = p.ident() else {
        return p.err("Commande attendue : use, show ou db…");
    };
    let cmd = match word {
        "use" => {
            p.skip_ws()?;
            let name = match p.peek() {
                Some(q @ (b'"' | b'\'')) => p.string(q)?,
                _ => {
                    let s = p.pos;
                    while p
                        .peek()
                        .is_some_and(|c| !c.is_ascii_whitespace() && c != b';')
                    {
                        p.pos += 1;
                    }
                    unit[s..p.pos].to_string()
                }
            };
            if name.is_empty() {
                return p.err("Nom de base attendu après « use »");
            }
            MongoCommand::Use(name)
        }
        "show" => {
            p.skip_ws()?;
            let what_at = p.pos;
            match p.ident() {
                Some("dbs" | "databases") => MongoCommand::ShowDbs,
                Some("collections" | "tables") => MongoCommand::ShowCollections,
                _ => return p.err_at(what_at, "« show dbs » ou « show collections » attendu"),
            }
        }
        "db" => db_chain(&mut p)?,
        _ => {
            return p.err_at(
                at,
                format!("Commande inconnue : {word} (attendu : use, show ou db…)"),
            )
        }
    };
    if !p.at_end()? {
        return p.err("Texte inattendu après la commande");
    }
    Ok(cmd)
}

/// Arguments of a call, converted, with their offsets.
struct Args {
    name: String,
    items: Vec<(Val, usize)>,
    at: usize,
}

impl Args {
    fn fail<T>(&self, offset: usize, message: impl Into<String>) -> PResult<T> {
        Err(ParseError {
            message: message.into(),
            offset,
        })
    }

    fn at_most(&self, n: usize) -> PResult<()> {
        if self.items.len() > n {
            let offset = self.items[n].1;
            return self.fail(
                offset,
                format!(
                    "{} : au plus {n} argument{} attendu{}",
                    self.name,
                    plural(n),
                    plural(n)
                ),
            );
        }
        Ok(())
    }

    fn bson(&self, i: usize) -> PResult<Option<(Bson, usize)>> {
        match self.items.get(i) {
            Some((v, at)) => Ok(Some((v.clone().into_bson()?, *at))),
            None => Ok(None),
        }
    }

    fn doc(&self, i: usize, what: &str) -> PResult<Option<Document>> {
        match self.bson(i)? {
            None => Ok(None),
            Some((Bson::Document(d), _)) => Ok(Some(d)),
            Some((_, at)) => self.fail(at, format!("{} : {what} attendu (document)", self.name)),
        }
    }

    fn required_doc(&self, i: usize, what: &str) -> PResult<Document> {
        match self.doc(i, what)? {
            Some(d) => Ok(d),
            None => self.fail(self.at, format!("{} : {what} attendu", self.name)),
        }
    }

    fn docs(&self, i: usize, what: &str) -> PResult<Vec<Document>> {
        match self.bson(i)? {
            Some((Bson::Array(items), at)) => items
                .into_iter()
                .map(|v| match v {
                    Bson::Document(d) => Ok(d),
                    _ => self.fail(at, format!("{} : {what} : documents attendus", self.name)),
                })
                .collect(),
            Some((_, at)) => self.fail(at, format!("{} : {what} attendu (tableau)", self.name)),
            None => self.fail(self.at, format!("{} : {what} attendu", self.name)),
        }
    }

    fn string(&self, i: usize, what: &str) -> PResult<String> {
        match self.items.get(i) {
            Some((Val::Str(s), _)) => Ok(s.clone()),
            Some((_, at)) => self.fail(*at, format!("{} : {what} attendu (chaîne)", self.name)),
            None => self.fail(self.at, format!("{} : {what} attendu", self.name)),
        }
    }

    fn int(&self, i: usize) -> PResult<i64> {
        match self.items.get(i) {
            Some((v, at)) => val_int(v).map_or_else(
                || self.fail(*at, format!("{} : entier attendu", self.name)),
                Ok,
            ),
            None => self.fail(self.at, format!("{} : entier attendu", self.name)),
        }
    }

    /// The options document, its keys checked against `known`.
    fn options(&self, i: usize, known: &[&str]) -> PResult<Option<Document>> {
        let Some(opts) = self.doc(i, "options")? else {
            return Ok(None);
        };
        if let Some(k) = opts.keys().find(|k| !known.contains(&k.as_str())) {
            let at = self.items[i].1;
            return self.fail(
                at,
                format!("{} : option non prise en charge : {k}", self.name),
            );
        }
        Ok(Some(opts))
    }

    fn upsert(&self, i: usize) -> PResult<bool> {
        let Some(opts) = self.options(i, &["upsert"])? else {
            return Ok(false);
        };
        match opts.get("upsert") {
            None => Ok(false),
            Some(Bson::Boolean(b)) => Ok(*b),
            Some(_) => self.fail(
                self.items[i].1,
                format!("{} : upsert : booléen attendu", self.name),
            ),
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n > 1 {
        "s"
    } else {
        ""
    }
}

fn call(p: &mut Parser, name: &str) -> PResult<Args> {
    let at = p.base + p.pos;
    let items = p.args()?;
    Ok(Args {
        name: name.to_string(),
        items,
        at,
    })
}

const DB_METHODS: &[&str] = &[
    "getCollection",
    "createCollection",
    "dropDatabase",
    "getCollectionNames",
    "runCommand",
    "adminCommand",
];

fn db_chain(p: &mut Parser) -> PResult<MongoCommand> {
    p.skip_ws()?;
    let coll = match p.peek() {
        Some(b'[') => {
            p.pos += 1;
            p.skip_ws()?;
            let name = match p.peek() {
                Some(q @ (b'"' | b'\'')) => p.string(q)?,
                _ => return p.err("Nom de collection attendu (chaîne)"),
            };
            p.expect(b']')?;
            name
        }
        Some(b'.') => {
            p.pos += 1;
            p.skip_ws()?;
            let at = p.pos;
            let Some(name) = p.ident() else {
                return p.err("Nom de collection ou méthode attendu après « db. »");
            };
            p.skip_ws()?;
            if p.peek() == Some(b'(') {
                let args = call(p, &format!("db.{name}"))?;
                return db_method(p, name, args, at);
            }
            let mut coll = name.to_string();
            // `db.fs.chunks.find()`: dotted collection names.
            loop {
                let save = p.pos;
                if !p.eat(b'.')? {
                    break;
                }
                p.skip_ws()?;
                let Some(seg) = p.ident() else {
                    p.pos = save;
                    break;
                };
                p.skip_ws()?;
                if p.peek() == Some(b'(') {
                    p.pos = save;
                    break;
                }
                coll.push('.');
                coll.push_str(seg);
            }
            coll
        }
        _ => return p.err("« . » attendu après « db »"),
    };
    coll_methods(p, coll)
}

fn db_method(p: &mut Parser, name: &str, args: Args, at: usize) -> PResult<MongoCommand> {
    Ok(match name {
        "getCollection" => {
            args.at_most(1)?;
            let coll = args.string(0, "nom de collection")?;
            return coll_methods(p, coll);
        }
        "createCollection" => {
            args.at_most(2)?;
            MongoCommand::CreateCollection {
                name: args.string(0, "nom de collection")?,
                options: args.doc(1, "options")?,
            }
        }
        "dropDatabase" => {
            args.at_most(0)?;
            MongoCommand::DropDatabase
        }
        "getCollectionNames" => {
            args.at_most(0)?;
            MongoCommand::GetCollectionNames
        }
        "runCommand" | "adminCommand" => {
            args.at_most(1)?;
            MongoCommand::RunCommand {
                command: args.required_doc(0, "commande")?,
                admin: name == "adminCommand",
            }
        }
        _ => {
            return p.err_at(
                at,
                format!("Méthode non prise en charge : db.{name}() (utiliser db.runCommand)"),
            )
        }
    })
}

fn coll_methods(p: &mut Parser, coll: String) -> PResult<MongoCommand> {
    p.expect(b'.')?;
    p.skip_ws()?;
    let at = p.pos;
    let Some(method) = p.ident() else {
        return p.err("Méthode attendue");
    };
    let args = call(p, method)?;
    let mut cmd = match method {
        "find" | "findOne" => {
            args.at_most(2)?;
            MongoCommand::Find {
                coll,
                filter: args.doc(0, "filtre")?.unwrap_or_default(),
                projection: args.doc(1, "projection")?,
                sort: None,
                skip: None,
                limit: None,
                one: method == "findOne",
            }
        }
        "aggregate" => {
            args.at_most(2)?;
            MongoCommand::Aggregate {
                coll,
                pipeline: args.docs(0, "pipeline")?,
                options: args.options(1, AGGREGATE_OPTIONS)?,
            }
        }
        "countDocuments" | "count" => {
            args.at_most(1)?;
            MongoCommand::Count {
                coll,
                filter: args.doc(0, "filtre")?.unwrap_or_default(),
            }
        }
        "estimatedDocumentCount" => {
            args.at_most(0)?;
            MongoCommand::EstimatedCount { coll }
        }
        "distinct" => {
            args.at_most(2)?;
            MongoCommand::Distinct {
                coll,
                field: args.string(0, "champ")?,
                filter: args.doc(1, "filtre")?.unwrap_or_default(),
            }
        }
        "insertOne" => {
            args.at_most(1)?;
            MongoCommand::InsertOne {
                coll,
                doc: args.required_doc(0, "document")?,
            }
        }
        "insertMany" => {
            args.at_most(2)?;
            MongoCommand::InsertMany {
                coll,
                docs: args.docs(0, "documents")?,
                options: args.options(1, &["ordered"])?,
            }
        }
        "updateOne" | "updateMany" => {
            args.at_most(3)?;
            let filter = args.required_doc(0, "filtre")?;
            let update = match args.bson(1)? {
                Some((Bson::Document(d), _)) => UpdateSpec::Document(d),
                Some((Bson::Array(_), _)) => UpdateSpec::Pipeline(args.docs(1, "pipeline")?),
                Some((_, at)) => {
                    return args.fail(
                        at,
                        format!("{method} : mise à jour attendue (document ou pipeline)"),
                    )
                }
                None => return args.fail(args.at, format!("{method} : mise à jour attendue")),
            };
            MongoCommand::Update {
                coll,
                filter,
                update,
                many: method == "updateMany",
                upsert: args.upsert(2)?,
            }
        }
        "replaceOne" => {
            args.at_most(3)?;
            MongoCommand::ReplaceOne {
                coll,
                filter: args.required_doc(0, "filtre")?,
                replacement: args.required_doc(1, "document")?,
                upsert: args.upsert(2)?,
            }
        }
        "deleteOne" | "deleteMany" => {
            args.at_most(1)?;
            MongoCommand::Delete {
                coll,
                filter: args.required_doc(0, "filtre")?,
                many: method == "deleteMany",
            }
        }
        "createIndex" => {
            args.at_most(2)?;
            MongoCommand::CreateIndex {
                coll,
                keys: args.required_doc(0, "clés")?,
                options: args.options(1, INDEX_OPTIONS)?,
            }
        }
        "dropIndex" => {
            args.at_most(1)?;
            let index = match args.bson(0)? {
                Some((v @ (Bson::String(_) | Bson::Document(_)), _)) => v,
                Some((_, at)) => return args.fail(at, "dropIndex : nom ou clés attendus"),
                None => return args.fail(args.at, "dropIndex : nom ou clés attendus"),
            };
            MongoCommand::DropIndex { coll, index }
        }
        "getIndexes" => {
            args.at_most(0)?;
            MongoCommand::GetIndexes { coll }
        }
        "drop" => {
            args.at_most(0)?;
            MongoCommand::DropCollection { coll }
        }
        _ => {
            return p.err_at(
                at,
                format!("Méthode non prise en charge : {method}() (utiliser db.runCommand)"),
            )
        }
    };
    // Chained modifiers.
    loop {
        let save = p.pos;
        if !p.eat(b'.')? {
            break;
        }
        p.skip_ws()?;
        let at = p.pos;
        let Some(m) = p.ident() else {
            p.pos = save;
            return p.err("Méthode attendue après « . »");
        };
        let args = call(p, m)?;
        let find = matches!(&cmd, MongoCommand::Find { one: false, .. });
        let mut replaced = None;
        match (m, &mut cmd) {
            ("toArray" | "pretty", _) => args.at_most(0)?,
            ("sort", MongoCommand::Find { sort, .. }) if find => {
                args.at_most(1)?;
                *sort = Some(args.required_doc(0, "tri")?);
            }
            ("projection", MongoCommand::Find { projection, .. }) if find => {
                args.at_most(1)?;
                *projection = Some(args.required_doc(0, "projection")?);
            }
            ("limit", MongoCommand::Find { limit, .. }) if find => {
                args.at_most(1)?;
                *limit = Some(args.int(0)?);
            }
            ("skip", MongoCommand::Find { skip, .. }) if find => {
                args.at_most(1)?;
                let n = args.int(0)?;
                *skip = Some(
                    u64::try_from(n)
                        .or_else(|_| args.fail(args.at, "skip : entier positif attendu"))?,
                );
            }
            ("count", MongoCommand::Find { coll, filter, .. }) if find => {
                args.at_most(0)?;
                replaced = Some(MongoCommand::Count {
                    coll: std::mem::take(coll),
                    filter: std::mem::take(filter),
                });
            }
            _ => {
                return p.err_at(at, format!("Méthode non prise en charge ici : .{m}()"));
            }
        }
        if let Some(r) = replaced {
            cmd = r;
        }
    }
    Ok(cmd)
}

/// Why `cmd` must be confirmed (French), `None` when it is harmless.
pub fn danger(cmd: &MongoCommand) -> Option<String> {
    match cmd {
        MongoCommand::Delete {
            coll,
            filter,
            many: true,
        } if filter.is_empty() => Some(format!(
            "Supprime tous les documents de la collection {coll}"
        )),
        MongoCommand::Update {
            coll,
            filter,
            many: true,
            ..
        } if filter.is_empty() => Some(format!(
            "Modifie tous les documents de la collection {coll}"
        )),
        MongoCommand::DropCollection { coll } => {
            Some(format!("Supprime la collection {coll} et ses index"))
        }
        MongoCommand::DropDatabase => Some("Supprime la base courante".into()),
        MongoCommand::Aggregate { coll, pipeline, .. } => writing_stage(pipeline).map(|stage| {
            format!(
                "L'étape {stage} de l'agrégation sur {coll} écrit dans une collection                  (la remplace ou la modifie)"
            )
        }),
        MongoCommand::DropIndex {
            coll,
            index: Bson::String(name),
        } if name == "*" => Some(format!("Supprime tous les index de la collection {coll}")),
        MongoCommand::RunCommand { command, .. } => {
            let first = command.keys().next()?;
            [
                "drop",
                "dropDatabase",
                "dropIndexes",
                "shutdown",
                "renameCollection",
            ]
            .contains(&first.as_str())
            .then(|| format!("Commande destructive : {first}"))
        }
        _ => None,
    }
}

/// The first `$out` / `$merge` stage of a pipeline (they write the results
/// to a collection, replacing or changing it).
pub fn writing_stage(pipeline: &[Document]) -> Option<&'static str> {
    pipeline.iter().find_map(|stage| {
        ["$out", "$merge"]
            .into_iter()
            .find(|s| stage.contains_key(*s))
    })
}

// ---------------------------------------------------------------------------
// Commands back to text
// ---------------------------------------------------------------------------

/// Whether `name` can follow `db.` as is.
fn bare_collection(name: &str) -> bool {
    !DB_METHODS.contains(&name)
        && name.split('.').all(|seg| {
            let b = seg.as_bytes();
            !b.is_empty() && is_ident_start(b[0]) && b.iter().all(|c| is_ident_char(*c))
        })
}

/// `db.users` / `db.getCollection("fs-chunks")`.
pub(crate) fn coll_expr(coll: &str) -> String {
    if bare_collection(coll) {
        format!("db.{coll}")
    } else {
        format!("db.getCollection({})", quote(coll))
    }
}

/// The arguments of a call: compact when short, else indented.
fn call_args(args: &[Bson]) -> String {
    let compact: Vec<String> = args.iter().map(|a| to_shell(a, false)).collect();
    let joined = compact.join(", ");
    if joined.len() <= 80 && !joined.contains('\n') {
        return joined;
    }
    args.iter()
        .map(|a| to_shell(a, true))
        .collect::<Vec<_>>()
        .join(", ")
}

fn docs_bson(docs: &[Document]) -> Bson {
    Bson::Array(docs.iter().cloned().map(Bson::Document).collect())
}

/// `cmd` in shell syntax (parses back to `cmd`).
pub fn command_to_shell(cmd: &MongoCommand) -> String {
    use MongoCommand as C;
    let d = |doc: &Document| Bson::Document(doc.clone());
    let method = |coll: &str, name: &str, args: &[Bson]| {
        format!("{}.{name}({})", coll_expr(coll), call_args(args))
    };
    match cmd {
        C::Use(db) => {
            if db
                .chars()
                .all(|c| !c.is_whitespace() && c != ';' && c != '"' && c != '\'')
            {
                format!("use {db}")
            } else {
                format!("use {}", quote(db))
            }
        }
        C::ShowDbs => "show dbs".into(),
        C::ShowCollections => "show collections".into(),
        C::Find {
            coll,
            filter,
            projection,
            sort,
            skip,
            limit,
            one,
        } => {
            let mut args = vec![d(filter)];
            if let Some(p) = projection {
                args.push(d(p));
            }
            let name = if *one { "findOne" } else { "find" };
            let mut out = method(coll, name, &args);
            if let Some(s) = sort {
                out.push_str(&format!(".sort({})", to_shell(&d(s), false)));
            }
            if let Some(s) = skip {
                out.push_str(&format!(".skip({s})"));
            }
            if let Some(l) = limit {
                out.push_str(&format!(".limit({l})"));
            }
            out
        }
        C::Aggregate {
            coll,
            pipeline,
            options,
        } => {
            let mut args = vec![docs_bson(pipeline)];
            if let Some(o) = options {
                args.push(d(o));
            }
            method(coll, "aggregate", &args)
        }
        C::Count { coll, filter } => method(coll, "countDocuments", &[d(filter)]),
        C::EstimatedCount { coll } => method(coll, "estimatedDocumentCount", &[]),
        C::Distinct {
            coll,
            field,
            filter,
        } => {
            let mut args = vec![Bson::String(field.clone())];
            if !filter.is_empty() {
                args.push(d(filter));
            }
            method(coll, "distinct", &args)
        }
        C::InsertOne { coll, doc } => method(coll, "insertOne", &[d(doc)]),
        C::InsertMany {
            coll,
            docs,
            options,
        } => {
            let mut args = vec![docs_bson(docs)];
            if let Some(o) = options {
                args.push(d(o));
            }
            method(coll, "insertMany", &args)
        }
        C::Update {
            coll,
            filter,
            update,
            many,
            upsert,
        } => {
            let mut args = vec![
                d(filter),
                match update {
                    UpdateSpec::Document(u) => d(u),
                    UpdateSpec::Pipeline(p) => docs_bson(p),
                },
            ];
            if *upsert {
                args.push(Bson::Document(bson::doc! { "upsert": true }));
            }
            method(coll, if *many { "updateMany" } else { "updateOne" }, &args)
        }
        C::ReplaceOne {
            coll,
            filter,
            replacement,
            upsert,
        } => {
            let mut args = vec![d(filter), d(replacement)];
            if *upsert {
                args.push(Bson::Document(bson::doc! { "upsert": true }));
            }
            method(coll, "replaceOne", &args)
        }
        C::Delete { coll, filter, many } => method(
            coll,
            if *many { "deleteMany" } else { "deleteOne" },
            &[d(filter)],
        ),
        C::CreateIndex {
            coll,
            keys,
            options,
        } => {
            let mut args = vec![d(keys)];
            if let Some(o) = options {
                args.push(d(o));
            }
            method(coll, "createIndex", &args)
        }
        C::DropIndex { coll, index } => method(coll, "dropIndex", std::slice::from_ref(index)),
        C::GetIndexes { coll } => method(coll, "getIndexes", &[]),
        C::CreateCollection { name, options } => {
            let mut args = vec![Bson::String(name.clone())];
            if let Some(o) = options {
                args.push(d(o));
            }
            format!("db.createCollection({})", call_args(&args))
        }
        C::DropCollection { coll } => method(coll, "drop", &[]),
        C::DropDatabase => "db.dropDatabase()".into(),
        C::GetCollectionNames => "db.getCollectionNames()".into(),
        C::RunCommand { command, admin } => format!(
            "db.{}({})",
            if *admin { "adminCommand" } else { "runCommand" },
            call_args(&[d(command)])
        ),
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
