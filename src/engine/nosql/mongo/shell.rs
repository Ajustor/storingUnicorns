//! MongoDB shell syntax (plan 13, « Syntaxe Mongo »): a hand-written parser
//! (no JavaScript) for the console, the filter / sort / projection bars and
//! the JSON editor, and its exact inverse printer. Types set by Task 1,
//! bodies by Task 2.
#![allow(unused_variables)] // stubs (plan 13, Task 2)

use std::ops::Range;

use bson::{Bson, Document};

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

/// One value in shell syntax (relaxed JSON, constructors, extended JSON).
pub fn parse_value(text: &str) -> Result<Bson, ParseError> {
    Err(not_implemented())
}

/// A document in shell syntax; "" (blank) = `{}`.
pub fn parse_document(text: &str) -> Result<Document, ParseError> {
    Err(not_implemented())
}

/// Every unit of a script, parsed (or its error).
pub fn parse_script(text: &str) -> Vec<Result<Parsed, ParseError>> {
    Vec::new()
}

/// `value` in shell syntax, exact inverse of `parse_value`; indented when
/// `pretty`.
pub fn to_shell(value: &Bson, pretty: bool) -> String {
    String::new()
}

/// Why `cmd` must be confirmed (French), `None` when it is harmless.
pub fn danger(cmd: &MongoCommand) -> Option<String> {
    None
}

fn not_implemented() -> ParseError {
    ParseError {
        message: super::super::not_implemented(super::ENGINE).to_string(),
        offset: 0,
    }
}
