//! Context-aware SQL completion: the columns of the tables a statement
//! references, chosen by the clause under the cursor (UI-agnostic, no I/O).
//!
//! Suggestions replace the token ending at the cursor: the trailing word
//! (letters, digits, `_`), or an identifier from its typed opening quote.
//! Quote characters inside string literals and comments are not taken for
//! opening quotes.

use crate::engine::db::utils::split_qualified;
use crate::engine::sql::lexer::{is_keyword, keywords};
use crate::engine::sql::statements::quote_ident;

/// A table referenced by a statement, names unquoted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableRef {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
}

/// A column of a table, as loaded from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub type_name: String,
    pub primary_key: bool,
    /// Table the column references, when it is part of a foreign key.
    pub foreign_key: Option<String>,
}

/// What a suggestion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Column,
    Table,
    Schema,
    Keyword,
}

/// One completion item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Shown, e.g. "email".
    pub label: String,
    /// Shown dimmed, e.g. "varchar(255)" / "PK int" / "table".
    pub detail: String,
    /// Inserted text, quoted for the engine when needed.
    pub insert: String,
    pub kind: Kind,
}

/// What the completion knows about the connection.
pub struct Context<'a> {
    /// Identifier quotes of the engine (see `statements::quote_chars`).
    pub quotes: (char, char),
    /// Known tables ("schema.table" or "table").
    pub tables: &'a [String],
    /// Columns of a table; `None` when not loaded yet.
    pub columns: &'a dyn Fn(&TableRef) -> Option<Vec<ColumnInfo>>,
    /// Columns of the last results (the old behaviour, when no table is cited).
    pub fallback_columns: &'a [String],
}

/// Suggestions, plus the tables whose columns are needed but not loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completions {
    pub items: Vec<Suggestion>,
    /// Tables to load (their columns would add suggestions).
    pub missing: Vec<TableRef>,
}

/// The single-table bars of the table view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarMode {
    Where,
    OrderBy,
}

const WHERE_KEYWORDS: &[&str] = &[
    "AND",
    "OR",
    "NOT",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "IN",
    "BETWEEN",
];
const ORDER_BY_KEYWORDS: &[&str] = &["ASC", "DESC", "NULLS FIRST", "NULLS LAST"];

/// Keywords after which the cursor is among columns.
const COLUMN_CLAUSES: &[&str] = &[
    "SELECT",
    "WHERE",
    "ON",
    "BY",
    "HAVING",
    "SET",
    "AND",
    "OR",
    "NOT",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "DISTINCT",
    "RETURNING",
];
/// Keywords after which the cursor is at a table name.
const TABLE_CLAUSES: &[&str] = &["FROM", "JOIN", "UPDATE", "INTO", "TABLE"];
/// Keywords after which neither columns nor tables are expected.
const OTHER_CLAUSES: &[&str] = &[
    "VALUES",
    "LIMIT",
    "OFFSET",
    "FETCH",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "MINUS",
    "WITH",
];
/// Keywords that only matter right before the cursor (an alias, a USING
/// list); further back they are walked over.
const LOCAL_CLAUSES: &[&str] = &["AS", "USING"];
/// Set operations: each branch has its own tables.
const SET_OPERATIONS: &[&str] = &["UNION", "INTERSECT", "EXCEPT", "MINUS"];
/// Words that end a table reference (never an alias), besides keywords.
const NOT_ALIAS: &[&str] = &[
    "FETCH",
    "WINDOW",
    "RETURNING",
    "OUTPUT",
    "QUALIFY",
    "MINUS",
    "STRAIGHT_JOIN",
    "LATERAL",
    "TABLESAMPLE",
    "FOR",
    "PARTITION",
    "APPLY",
    "ONLY",
    "GO",
];
/// Reserved words (in some engine) missing from the lexer's keywords: a
/// name equal to one of them is quoted.
const RESERVED: &[&str] = &[
    "user",
    "current_user",
    "session_user",
    "window",
    "limit",
    "offset",
    "order",
    "group",
    "table",
    "column",
    "check",
    "default",
    "primary",
    "references",
    "analyse",
    "analyze",
    "end",
];

// ---------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, Copy, PartialEq)]
enum T {
    /// Keyword or bare identifier.
    Word,
    /// Quoted identifier, quotes included.
    Quoted,
    /// String literal.
    Str,
    Num,
    Comment,
    Punct(char),
}

#[derive(Debug, Clone, Copy)]
struct Tok {
    kind: T,
    start: usize,
    end: usize,
    /// Unterminated (a line comment always is: it runs to its newline).
    open: bool,
}

impl Tok {
    fn is_name(&self) -> bool {
        matches!(self.kind, T::Word | T::Quoted)
    }
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '@' | '#')
}

fn scan(s: &str, i: usize, keep: impl Fn(char) -> bool) -> usize {
    s[i..]
        .char_indices()
        .find(|&(_, c)| !keep(c))
        .map_or(s.len(), |(p, _)| i + p)
}

/// The dollar-quote opening `s` (`$$` or `$tag$`, the tag an identifier not
/// starting with a digit, so `$1` is a parameter).
fn dollar_tag(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('$')?;
    let len = rest.find('$')?;
    let tag = &rest[..len];
    let mut chars = tag.chars();
    let valid = chars.next().is_none_or(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_');
    valid.then(|| &s[..len + 2])
}

/// Tokens of `s` with byte spans, comments included, whitespace dropped.
fn lex(s: &str) -> Vec<Tok> {
    let n = s.len();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(c) = s[i..].chars().next() {
        let start = i;
        let next = s[i + c.len_utf8()..].chars().next();
        let mut open = false;
        let kind = if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        } else if c == '-' && next == Some('-') {
            i = s[i..].find('\n').map_or(n, |p| i + p);
            open = true;
            T::Comment
        } else if c == '/' && next == Some('*') {
            match s[i + 2..].find("*/") {
                Some(p) => i += 2 + p + 2,
                None => {
                    i = n;
                    open = true;
                }
            }
            T::Comment
        } else if let Some(tag) = dollar_tag(&s[i..]) {
            // PostgreSQL dollar quoting: `$$ … $$`, `$tag$ … $tag$`.
            let body = i + tag.len();
            match s[body..].find(tag) {
                Some(p) => i = body + p + tag.len(),
                None => {
                    i = n;
                    open = true;
                }
            }
            T::Str
        } else if matches!(c, '\'' | '"' | '`' | '[') {
            let close = if c == '[' { ']' } else { c };
            i += 1;
            loop {
                match s[i..].find(close) {
                    None => {
                        i = n;
                        open = true;
                        break;
                    }
                    Some(p) => {
                        i += p + 1;
                        if s[i..].starts_with(close) {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                }
            }
            if c == '\'' {
                T::Str
            } else {
                T::Quoted
            }
        } else if c.is_ascii_digit() {
            i = scan(s, i, |c| c.is_alphanumeric() || c == '.');
            T::Num
        } else if word_char(c) {
            i = scan(s, i, word_char);
            T::Word
        } else {
            i += c.len_utf8();
            T::Punct(c)
        };
        out.push(Tok {
            kind,
            start,
            end: i,
            open,
        });
    }
    out
}

/// The token being typed at the end of `before`: (start, prefix, opening
/// quote typed). The prefix is the trailing word (letters, digits, `_`),
/// or the text after an opening identifier quote. `None` inside a string
/// literal or a comment.
fn current_token(before: &str) -> Option<(usize, String, Option<char>)> {
    let end = before.len();
    match lex(before).last() {
        Some(t) if t.open && t.end == end && matches!(t.kind, T::Str | T::Comment) => None,
        Some(t) if t.open && t.kind == T::Quoted => {
            let q = before[t.start..].chars().next();
            Some((t.start, before[t.start + 1..].to_string(), q))
        }
        Some(t) if t.end == end && matches!(t.kind, T::Word | T::Num) => {
            let start = before[t.start..]
                .char_indices()
                .rev()
                .take_while(|&(_, c)| c.is_alphanumeric() || c == '_')
                .last()
                .map_or(end, |(p, _)| t.start + p);
            Some((start, before[start..].to_string(), None))
        }
        _ => Some((end, String::new(), None)),
    }
}

/// Byte offset where the token completed at `cursor` starts: what a chosen
/// suggestion's `insert` replaces, up to the cursor. Same rule as
/// [`complete`]: the trailing word, or from a typed opening identifier
/// quote, quotes inside literals and comments ignored. Inside a literal or
/// a comment, the cursor itself (nothing to replace).
pub fn token_start(text: &str, cursor: usize) -> usize {
    let cursor = floor_boundary(text, cursor);
    current_token(&text[..cursor]).map_or(cursor, |(start, _, _)| start)
}

/// Largest char boundary of `text` not after `cursor`.
fn floor_boundary(text: &str, cursor: usize) -> usize {
    let mut c = cursor.min(text.len());
    while !text.is_char_boundary(c) {
        c -= 1;
    }
    c
}

/// Byte range of the statement under the cursor: between the `;` around it
/// outside literals, comments and quoted identifiers (the console's
/// `split_statements` boundaries, plus quoted identifiers holding a `;`).
fn statement_at(text: &str, cursor: usize) -> (usize, usize) {
    let mut start = 0;
    for t in lex(text) {
        if t.kind == T::Punct(';') {
            if t.end <= cursor {
                start = t.end;
            } else {
                return (start, t.start);
            }
        }
    }
    (start, text.len())
}

/// Whether `name` must be quoted to be used as is with these quotes.
fn needs_quotes(name: &str, (q0, _): (char, char)) -> bool {
    let mut chars = name.chars();
    let plain = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    // PostgreSQL folds bare names to lower case.
    !plain
        || is_keyword(name)
        || RESERVED.iter().any(|r| r.eq_ignore_ascii_case(name))
        || (q0 == '"' && name.chars().any(|c| c.is_ascii_uppercase()))
}

/// Quote pair matching an opening quote typed by the user.
fn typed_pair(q: char) -> (char, char) {
    if q == '[' {
        ('[', ']')
    } else {
        (q, q)
    }
}

/// `name` as inserted: in the typed quotes if any, else quoted when needed.
fn ident(name: &str, typed: Option<char>, quotes: (char, char)) -> String {
    match typed {
        Some(q) => quote_ident(name, typed_pair(q)),
        None if needs_quotes(name, quotes) => quote_ident(name, quotes),
        None => name.to_string(),
    }
}

fn starts_with_ci(s: &str, prefix: &str) -> bool {
    prefix.is_empty() || s.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// `PK int`, `FK → users int`, `PK FK → users int`, `int`.
fn column_detail(c: &ColumnInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if c.primary_key {
        parts.push("PK".to_string());
    }
    if let Some(table) = &c.foreign_key {
        parts.push(format!("FK → {table}"));
    }
    if !c.type_name.is_empty() {
        parts.push(c.type_name.clone());
    }
    parts.join(" ")
}

// ---------------------------------------------------------------------------
// Statement analysis

/// An item of one nesting level: a token, or a whole parenthesised group.
#[derive(Debug, Clone, Copy)]
enum Item {
    Tok(usize),
    /// Index of the `(` token.
    Group(usize),
}

/// A table reference as written: quoted names match known tables exactly,
/// bare ones ignoring case.
#[derive(Debug, Clone)]
struct Ref {
    table: TableRef,
    quoted: bool,
}

/// Where the cursor is, clause-wise.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Clause {
    Column,
    /// At a table name position (right after the keyword or a list comma)
    /// or `false` after a table (alias, next clause).
    Table(bool),
    Other,
    None,
}

struct Stmt<'t> {
    text: &'t str,
    /// Tokens without comments.
    toks: Vec<Tok>,
    /// For a `(`, its matching `)`; for a `)`, its `(`.
    pair: Vec<Option<usize>>,
}

impl<'t> Stmt<'t> {
    fn new(text: &'t str) -> Self {
        let toks: Vec<Tok> = lex(text)
            .into_iter()
            .filter(|t| t.kind != T::Comment)
            .collect();
        let mut pair = vec![None; toks.len()];
        let mut stack = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            match t.kind {
                T::Punct('(') => stack.push(i),
                T::Punct(')') => {
                    if let Some(o) = stack.pop() {
                        pair[o] = Some(i);
                        pair[i] = Some(o);
                    }
                }
                _ => {}
            }
        }
        Stmt { text, toks, pair }
    }

    fn raw(&self, i: usize) -> &'t str {
        &self.text[self.toks[i].start..self.toks[i].end]
    }

    /// Whether token `i` is the keyword `kw` (upper case).
    fn is(&self, i: usize, kw: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == T::Word) && self.raw(i).eq_ignore_ascii_case(kw)
    }

    fn is_any(&self, i: usize, kws: &[&str]) -> bool {
        kws.iter().any(|kw| self.is(i, kw))
    }

    fn is_punct(&self, i: usize, c: char) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == T::Punct(c))
    }

    /// Unquoted text of the name token `i`.
    fn name(&self, i: usize) -> String {
        if self.toks[i].kind == T::Quoted {
            split_qualified(self.raw(i)).1
        } else {
            self.raw(i).to_string()
        }
    }

    /// Whether the `(` at `i` opens a subquery.
    fn is_subquery(&self, i: usize) -> bool {
        self.is_any(i + 1, &["SELECT", "WITH"])
    }

    /// The `(` tokens enclosing `cursor`, innermost first.
    fn enclosing(&self, cursor: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.toks.len())
            .filter(|&i| {
                self.toks[i].kind == T::Punct('(')
                    && self.toks[i].end <= cursor
                    && self.pair[i].is_none_or(|c| self.toks[c].start >= cursor)
            })
            .collect();
        v.reverse();
        v
    }

    /// The subquery `(` holding the cursor, `None` at the top level.
    fn scope_opener(&self, cursor: usize) -> Option<usize> {
        self.enclosing(cursor)
            .into_iter()
            .find(|&o| self.is_subquery(o))
    }

    /// Tables of the levels enclosing the one of `cursor`, innermost first.
    fn outer_scopes(&self, cursor: usize) -> Vec<Vec<Ref>> {
        let mut openers: Vec<Option<usize>> = self
            .enclosing(cursor)
            .into_iter()
            .filter(|&o| self.is_subquery(o))
            .map(Some)
            .collect();
        if openers.is_empty() {
            return Vec::new();
        }
        openers.remove(0);
        openers.push(None);
        openers
            .into_iter()
            .map(|o| self.without_ctes(self.table_refs(&self.branch(o, cursor))))
            .collect()
    }

    /// Items of the level just inside `opener` (the whole statement for `None`).
    fn level(&self, opener: Option<usize>) -> Vec<Item> {
        let (mut i, end) = match opener {
            Some(o) => (o + 1, self.pair[o].unwrap_or(self.toks.len())),
            None => (0, self.toks.len()),
        };
        let mut items = Vec::new();
        while i < end {
            if self.toks[i].kind == T::Punct('(') {
                items.push(Item::Group(i));
                i = self.pair[i].map_or(end, |c| c + 1);
            } else {
                items.push(Item::Tok(i));
                i += 1;
            }
        }
        items
    }

    /// Names defined by `WITH name [(cols)] AS (…)` anywhere in the statement.
    fn cte_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for i in 0..self.toks.len() {
            if !self.toks[i].is_name() || i == 0 {
                continue;
            }
            let after_with =
                self.is_any(i - 1, &["WITH", "RECURSIVE"]) || self.is_punct(i - 1, ',');
            let mut j = i + 1;
            if self.is_punct(j, '(') {
                match self.pair[j] {
                    Some(c) => j = c + 1,
                    None => continue,
                }
            }
            if after_with && self.is(j, "AS") && self.is_punct(j + 1, '(') {
                names.push(self.name(i));
            }
        }
        names
    }

    /// A possibly qualified name starting at token `i`, and the index after
    /// it.
    fn qualified_at(&self, i: usize) -> Option<(Ref, usize)> {
        if !self.toks.get(i)?.is_name() {
            return None;
        }
        let mut last = i;
        while self.is_punct(last + 1, '.') && self.toks.get(last + 2).is_some_and(Tok::is_name) {
            last += 2;
        }
        let raw = &self.text[self.toks[i].start..self.toks[last].end];
        let (schema, name) = split_qualified(raw);
        let quoted = (i..=last).any(|k| self.toks[k].kind == T::Quoted);
        let table = TableRef {
            schema,
            name,
            alias: None,
        };
        Some((Ref { table, quoted }, last + 1))
    }

    fn ends_ref(&self, i: usize) -> bool {
        self.toks[i].kind == T::Word && (is_keyword(self.raw(i)) || self.is_any(i, NOT_ALIAS))
    }

    /// Table references of a level.
    fn table_refs(&self, items: &[Item]) -> Vec<Ref> {
        let tok = |k: usize| match items.get(k) {
            Some(Item::Tok(i)) => Some(*i),
            _ => None,
        };
        let is_kw = |k: usize, kws: &[&str]| tok(k).is_some_and(|i| self.is_any(i, kws));
        let mut refs = Vec::new();
        for k in 0..items.len() {
            let (commas, calls) = if is_kw(k, &["FROM", "UPDATE"]) {
                (true, true)
            } else if is_kw(k, &["JOIN"]) {
                (false, true)
            } else if is_kw(k, &["INTO"])
                && k > 0
                && is_kw(k - 1, &["INSERT", "REPLACE", "IGNORE", "MERGE"])
            {
                (false, false)
            } else {
                continue;
            };
            let mut j = k + 1;
            loop {
                let mut table = None;
                match items.get(j) {
                    // Derived table: no known columns.
                    Some(Item::Group(_)) => j += 1,
                    Some(Item::Tok(i)) if !self.ends_ref(*i) => {
                        let Some((r, next)) = self.qualified_at(*i) else {
                            break;
                        };
                        // Skip the items of the name.
                        while tok(j).is_some_and(|t| t < next) {
                            j += 1;
                        }
                        if calls && matches!(items.get(j), Some(Item::Group(_))) {
                            j += 1; // table function
                        } else {
                            table = Some(r);
                        }
                    }
                    _ => break,
                }
                // Optional alias.
                if is_kw(j, &["AS"]) {
                    j += 1;
                }
                let alias = tok(j).filter(|&i| self.toks[i].is_name() && !self.ends_ref(i));
                if let Some(i) = alias {
                    j += 1;
                    if let Some(t) = table.as_mut() {
                        t.table.alias = Some(self.name(i));
                    }
                }
                refs.extend(table);
                if commas && tok(j).is_some_and(|i| self.is_punct(i, ',')) {
                    j += 1;
                } else {
                    break;
                }
            }
        }
        refs
    }

    /// `refs` without the CTE names.
    fn without_ctes(&self, mut refs: Vec<Ref>) -> Vec<Ref> {
        let ctes = self.cte_names();
        refs.retain(|r| {
            r.table.schema.is_some() || !ctes.iter().any(|c| c.eq_ignore_ascii_case(&r.table.name))
        });
        refs
    }

    /// Items of the level inside `opener`, only the branch of a set
    /// operation (UNION, INTERSECT, EXCEPT, MINUS) holding `cursor`.
    fn branch(&self, opener: Option<usize>, cursor: usize) -> Vec<Item> {
        let mut items = self.level(opener);
        let mut start = 0;
        for (k, item) in items.iter().enumerate() {
            if let Item::Tok(i) = *item {
                if self.is_any(i, SET_OPERATIONS) {
                    if self.toks[i].end <= cursor {
                        start = k + 1;
                    } else {
                        items.truncate(k);
                        break;
                    }
                }
            }
        }
        items.drain(..start.min(items.len()));
        items
    }

    /// Tables of the level of `cursor`, CTE names left out.
    fn tables_in_scope(&self, cursor: usize) -> Vec<Ref> {
        self.without_ctes(self.table_refs(&self.branch(self.scope_opener(cursor), cursor)))
    }

    /// Clause governing the position before token `before` (exclusive),
    /// looking back to the start of the scope.
    fn clause(&self, before: usize, opener: Option<usize>) -> Clause {
        let lo = opener.map_or(0, |o| o + 1);
        let mut k = before;
        let mut prev = None;
        while k > lo {
            k -= 1;
            let first = *prev.get_or_insert(k) == k;
            match self.toks[k].kind {
                T::Punct(')') => {
                    if let Some(o) = self.pair[k] {
                        if o < lo {
                            break;
                        }
                        k = o;
                    }
                }
                T::Word if self.is_any(k, COLUMN_CLAUSES) => return Clause::Column,
                T::Word if self.is_any(k, TABLE_CLAUSES) => {
                    let after_comma = self.is_any(k, &["FROM", "UPDATE"])
                        && prev.is_some_and(|p| self.is_punct(p, ','));
                    return Clause::Table(first || after_comma);
                }
                // Further back, AS / USING are walked over.
                T::Word if first && self.is_any(k, LOCAL_CLAUSES) => return Clause::Other,
                T::Word if self.is_any(k, OTHER_CLAUSES) => return Clause::Other,
                _ => {}
            }
        }
        Clause::None
    }

    /// `JOIN t USING (…|`: the tables joined so far, the one of the JOIN
    /// last.
    fn using_tables(&self, cursor: usize) -> Option<Vec<Ref>> {
        let o = *self.enclosing(cursor).first()?;
        let using = o.checked_sub(1).filter(|&k| self.is(k, "USING"))?;
        let items: Vec<Item> = self
            .branch(self.scope_opener(cursor), cursor)
            .into_iter()
            .take_while(|it| match it {
                Item::Tok(i) | Item::Group(i) => *i < using,
            })
            .collect();
        Some(self.without_ctes(self.table_refs(&items)))
    }

    /// `INSERT INTO t (…|`: the table whose column list holds the cursor.
    fn insert_columns_table(&self, cursor: usize) -> Option<Ref> {
        let o = *self.enclosing(cursor).first()?;
        // Walk back over `name(.name)*` to the INTO.
        let mut k = o.checked_sub(1)?;
        if !self.toks[k].is_name() {
            return None;
        }
        while k >= 2 && self.is_punct(k - 1, '.') && self.toks[k - 2].is_name() {
            k -= 2;
        }
        if k == 0 || !self.is(k - 1, "INTO") {
            return None;
        }
        self.qualified_at(k).map(|(r, _)| r)
    }
}

// ---------------------------------------------------------------------------
// Suggestions

struct Builder<'c, 'a> {
    cx: &'c Context<'a>,
    prefix: String,
    typed: Option<char>,
    out: Completions,
}

impl Builder<'_, '_> {
    /// `r` as known to the connection: the known table it names (its
    /// schema filled in) when exactly one matches, else as written.
    fn canonical(&self, r: &Ref) -> TableRef {
        let t = &r.table;
        let eq = |a: &str, b: &str| {
            if r.quoted {
                a == b
            } else {
                a.eq_ignore_ascii_case(b)
            }
        };
        let mut found = self
            .cx
            .tables
            .iter()
            .map(|f| split_qualified(f))
            .filter(|(s, n)| {
                eq(n, &t.name)
                    && t.schema
                        .as_deref()
                        .is_none_or(|ts| s.as_deref().is_some_and(|s| eq(s, ts)))
            });
        match (found.next(), found.next()) {
            (Some((schema, name)), None) => TableRef {
                schema: schema.or_else(|| t.schema.clone()),
                name,
                alias: t.alias.clone(),
            },
            _ => t.clone(),
        }
    }

    fn canonical_all(&self, refs: &[Ref]) -> Vec<TableRef> {
        refs.iter().map(|r| self.canonical(r)).collect()
    }

    /// Columns of `table`, or `None` (and the table noted missing).
    fn load(&mut self, table: &TableRef) -> Option<Vec<ColumnInfo>> {
        let cols = (self.cx.columns)(table);
        if cols.is_none() && !self.out.missing.contains(table) {
            self.out.missing.push(table.clone());
        }
        cols
    }

    fn column(&mut self, col: &ColumnInfo, qualifier: Option<&str>) {
        if !starts_with_ci(&col.name, &self.prefix) {
            return;
        }
        let name = ident(&col.name, self.typed, self.cx.quotes);
        let (label, insert) = match qualifier {
            Some(q) => {
                let q = ident(q, None, self.cx.quotes);
                (format!("{q}.{}", col.name), format!("{q}.{name}"))
            }
            None => (col.name.clone(), name),
        };
        self.out.items.push(Suggestion {
            label,
            detail: column_detail(col),
            insert,
            kind: Kind::Column,
        });
    }

    /// Columns of `table` only.
    fn columns_of(&mut self, table: &TableRef) {
        for col in self.load(table).unwrap_or_default() {
            self.column(&col, None);
        }
    }

    /// `USING (…)` list: the columns of the joined (last) table also found
    /// in one of the others.
    fn shared_columns(&mut self, tables: &[TableRef]) {
        let Some((joined, others)) = tables.split_last() else {
            return;
        };
        let cols = self.load(joined);
        let others: Vec<Vec<ColumnInfo>> = others.iter().filter_map(|t| self.load(t)).collect();
        for col in cols.unwrap_or_default() {
            let shared = others
                .iter()
                .any(|cs| cs.iter().any(|c| c.name.eq_ignore_ascii_case(&col.name)));
            if shared || tables.len() == 1 {
                self.column(&col, None);
            }
        }
    }

    /// Columns of several tables; a name found in several of them is
    /// prefixed with its table's alias (or name).
    fn columns_of_all(&mut self, tables: &[TableRef]) {
        let loaded: Vec<(TableRef, Vec<ColumnInfo>)> = tables
            .iter()
            .filter_map(|t| self.load(t).map(|c| (t.clone(), c)))
            .collect();
        let count = |name: &str| {
            loaded
                .iter()
                .filter(|(_, cols)| cols.iter().any(|c| c.name.eq_ignore_ascii_case(name)))
                .count()
        };
        for (table, cols) in &loaded {
            let qualifier = table.alias.as_deref().unwrap_or(&table.name);
            for col in cols {
                let q = (count(&col.name) > 1).then_some(qualifier);
                self.column(col, q);
            }
        }
    }

    fn fallback_columns(&mut self) {
        for name in self.cx.fallback_columns {
            if starts_with_ci(name, &self.prefix) {
                self.out.items.push(Suggestion {
                    label: name.clone(),
                    detail: String::new(),
                    insert: ident(name, self.typed, self.cx.quotes),
                    kind: Kind::Column,
                });
            }
        }
    }

    fn keywords(&mut self, list: &[&str]) {
        if self.typed.is_some() {
            return;
        }
        for kw in list {
            if starts_with_ci(kw, &self.prefix) {
                self.out.items.push(Suggestion {
                    label: kw.to_string(),
                    detail: "keyword".to_string(),
                    insert: kw.to_string(),
                    kind: Kind::Keyword,
                });
            }
        }
    }

    /// Known tables, only those of `schema` when given (then the table part
    /// alone is inserted); otherwise the matching schemas follow.
    fn tables(&mut self, schema: Option<&str>) {
        let quotes = self.cx.quotes;
        let mut schemas: Vec<String> = Vec::new();
        for full in self.cx.tables {
            let (t_schema, t_name) = split_qualified(full);
            let insert = match (schema, &t_schema) {
                (Some(want), Some(s)) if s.eq_ignore_ascii_case(want) => {
                    if !starts_with_ci(&t_name, &self.prefix) {
                        continue;
                    }
                    ident(&t_name, self.typed, quotes)
                }
                (Some(_), _) => continue,
                (None, _) => {
                    if !starts_with_ci(full, &self.prefix) && !starts_with_ci(&t_name, &self.prefix)
                    {
                        continue;
                    }
                    let name = ident(&t_name, self.typed, quotes);
                    match &t_schema {
                        Some(s) => format!("{}.{name}", ident(s, self.typed, quotes)),
                        None => name,
                    }
                }
            };
            self.out.items.push(Suggestion {
                label: full.clone(),
                detail: "table".to_string(),
                insert,
                kind: Kind::Table,
            });
        }
        if schema.is_some() {
            return;
        }
        for full in self.cx.tables {
            if let (Some(s), _) = split_qualified(full) {
                if starts_with_ci(&s, &self.prefix) && !schemas.contains(&s) {
                    schemas.push(s);
                }
            }
        }
        for s in schemas {
            self.out.items.push(Suggestion {
                insert: ident(&s, self.typed, quotes),
                label: s,
                detail: "schema".to_string(),
                kind: Kind::Schema,
            });
        }
    }

    /// The table a `q.` (or `s.t.`) qualifier names: an alias or table in
    /// scope, else a known table.
    fn resolve(&self, parts: &[String], scopes: &[Vec<TableRef>]) -> Option<TableRef> {
        let (schema, name) = match parts {
            [name] => (None, name),
            [.., schema, name] => (Some(schema), name),
            [] => return None,
        };
        let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        let schema_ok =
            |t: &Option<String>| schema.is_none_or(|s| t.as_deref().is_some_and(|ts| eq(ts, s)));
        // The level of the cursor first, then the enclosing ones (a
        // correlated subquery sees the tables of its outer queries).
        for scope in scopes {
            if schema.is_none() {
                if let Some(t) = scope
                    .iter()
                    .find(|t| t.alias.as_deref().is_some_and(|a| eq(a, name)))
                {
                    return Some(t.clone());
                }
            }
            if let Some(t) = scope
                .iter()
                .find(|t| eq(&t.name, name) && schema_ok(&t.schema))
            {
                return Some(t.clone());
            }
        }
        self.cx.tables.iter().find_map(|full| {
            let (s, n) = split_qualified(full);
            (eq(&n, name) && schema_ok(&s)).then_some(TableRef {
                schema: s,
                name: n,
                alias: None,
            })
        })
    }
}

/// Suggestions for the cursor in `text` (a script: only the statement under
/// the cursor counts).
pub fn complete(text: &str, cursor: usize, cx: &Context) -> Completions {
    let cursor = floor_boundary(text, cursor);
    let Some((tstart, prefix, typed)) = current_token(&text[..cursor]) else {
        return Completions::default();
    };
    // An identifier whose opening quote is typed would swallow the rest of
    // the script: analyse the text without it (same byte offsets).
    let blanked;
    let text = if typed.is_some() {
        blanked = format!(
            "{}{}{}",
            &text[..tstart],
            " ".repeat(cursor - tstart),
            &text[cursor..]
        );
        blanked.as_str()
    } else {
        text
    };
    let (s, e) = statement_at(text, cursor);
    let stmt = Stmt::new(&text[s..e]);
    let cur = cursor - s;
    let tstart = tstart.max(s) - s;
    let mut b = Builder {
        cx,
        prefix,
        typed,
        out: Completions::default(),
    };

    // Tokens before the current one, then a `q.` / `s.t.` qualifier.
    let before = stmt.toks.iter().take_while(|t| t.end <= tstart).count();
    let mut parts = Vec::new();
    let mut chain = before;
    if before >= 2 && stmt.is_punct(before - 1, '.') && stmt.toks[before - 1].end == tstart {
        let mut k = before - 1;
        while k >= 1 && stmt.is_punct(k, '.') && stmt.toks[k - 1].is_name() {
            parts.push(stmt.name(k - 1));
            k -= 1;
            chain = k;
            if k == 0 {
                break;
            }
            k -= 1;
        }
        parts.reverse();
    }

    let opener = stmt.scope_opener(cur);
    let scope = b.canonical_all(&stmt.tables_in_scope(cur));
    let clause = stmt.clause(chain, opener);

    if !parts.is_empty() {
        let mut scopes = vec![scope];
        scopes.extend(stmt.outer_scopes(cur).iter().map(|s| b.canonical_all(s)));
        let table = if matches!(clause, Clause::Table(_)) {
            None
        } else {
            b.resolve(&parts, &scopes)
        };
        match table {
            Some(t) => b.columns_of(&t),
            None => b.tables(parts.last().map(String::as_str)),
        }
        return b.out;
    }
    if let Some(r) = stmt.insert_columns_table(cur) {
        let t = b.canonical(&r);
        b.columns_of(&t);
        return b.out;
    }
    if let Some(refs) = stmt.using_tables(cur) {
        let tables = b.canonical_all(&refs);
        b.shared_columns(&tables);
        return b.out;
    }
    match clause {
        Clause::Table(true) => b.tables(None),
        Clause::Column if !scope.is_empty() => {
            b.columns_of_all(&scope);
            b.keywords(keywords());
        }
        Clause::Table(false) | Clause::Other if !scope.is_empty() => b.keywords(keywords()),
        _ => {
            b.fallback_columns();
            b.keywords(keywords());
        }
    }
    b.out
}

/// Tables referenced by the statement at `cursor` (FROM, JOIN, UPDATE,
/// INSERT INTO, DELETE FROM; commas; optional AS; quoted / qualified names;
/// only the nesting level of the cursor for subqueries).
// Part of the module's API; the frontends ask `complete`, which uses it.
#[allow(dead_code)]
pub fn tables_in_scope(text: &str, cursor: usize) -> Vec<TableRef> {
    let cursor = floor_boundary(text, cursor);
    let (s, e) = statement_at(text, cursor);
    Stmt::new(&text[s..e])
        .tables_in_scope(cursor - s)
        .into_iter()
        .map(|r| r.table)
        .collect()
}

/// For a single-table context (data view filter / order bar).
pub fn complete_for_table(
    fragment: &str,
    cursor: usize,
    mode: BarMode,
    table: &TableRef,
    cx: &Context,
) -> Completions {
    let cur = floor_boundary(fragment, cursor);
    let Some((_, prefix, typed)) = current_token(&fragment[..cur]) else {
        return Completions::default();
    };
    let mut b = Builder {
        cx,
        prefix,
        typed,
        out: Completions::default(),
    };
    b.columns_of(table);
    b.keywords(match mode {
        BarMode::Where => WHERE_KEYWORDS,
        BarMode::OrderBy => ORDER_BY_KEYWORDS,
    });
    b.out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: (char, char) = ('"', '"');
    const MYSQL: (char, char) = ('`', '`');
    const MSSQL: (char, char) = ('[', ']');

    fn col(name: &str, ty: &str, pk: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            type_name: ty.into(),
            primary_key: pk,
            foreign_key: None,
        }
    }

    fn columns(t: &TableRef) -> Option<Vec<ColumnInfo>> {
        match t.name.as_str() {
            "users" => Some(vec![
                col("id", "int", true),
                col("email", "varchar(255)", false),
                col("name", "text", false),
            ]),
            "orders" => Some(vec![
                col("id", "int", true),
                col("user_id", "int", false),
                col("total", "numeric", false),
            ]),
            "Order Items" => Some(vec![
                col("CreatedAt", "timestamp", false),
                col("order", "int", false),
            ]),
            _ => None,
        }
    }

    fn tables() -> Vec<String> {
        vec![
            "public.users".into(),
            "public.orders".into(),
            "sales.Order Items".into(),
            "logs".into(),
        ]
    }

    /// Text without its `|` marker, and the marker's byte offset.
    fn at(s: &str) -> (String, usize) {
        let c = s.find('|').expect("cursor marker");
        (s.replacen('|', "", 1), c)
    }

    fn run_q(s: &str, quotes: (char, char), fallback: &[String]) -> Completions {
        let (text, c) = at(s);
        let tables = tables();
        let cx = Context {
            quotes,
            tables: &tables,
            columns: &columns,
            fallback_columns: fallback,
        };
        complete(&text, c, &cx)
    }

    fn run(s: &str) -> Completions {
        run_q(s, PG, &[])
    }

    fn labels(c: &Completions, kind: Kind) -> Vec<String> {
        c.items
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.label.clone())
            .collect()
    }

    fn inserts(c: &Completions, kind: Kind) -> Vec<String> {
        c.items
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.insert.clone())
            .collect()
    }

    fn scope(s: &str) -> Vec<(Option<String>, String, Option<String>)> {
        let (text, c) = at(s);
        tables_in_scope(&text, c)
            .into_iter()
            .map(|t| (t.schema, t.name, t.alias))
            .collect()
    }

    fn t(
        schema: Option<&str>,
        name: &str,
        alias: Option<&str>,
    ) -> (Option<String>, String, Option<String>) {
        (schema.map(Into::into), name.into(), alias.map(Into::into))
    }

    fn tref(name: &str) -> TableRef {
        TableRef {
            schema: None,
            name: name.into(),
            alias: None,
        }
    }

    // --- tables_in_scope ---------------------------------------------------

    #[test]
    fn scope_alias_with_and_without_as() {
        assert_eq!(
            scope("SELECT | FROM users u"),
            vec![t(None, "users", Some("u"))]
        );
        assert_eq!(
            scope("SELECT | FROM users AS u WHERE 1=1"),
            vec![t(None, "users", Some("u"))]
        );
        assert_eq!(
            scope("SELECT | FROM users WHERE id = 1"),
            vec![t(None, "users", None)]
        );
    }

    #[test]
    fn scope_multiple_joins() {
        assert_eq!(
            scope("SELECT | FROM users u JOIN orders o ON o.user_id = u.id LEFT OUTER JOIN items AS i ON i.order_id = o.id"),
            vec![t(None, "users", Some("u")), t(None, "orders", Some("o")), t(None, "items", Some("i"))]
        );
    }

    #[test]
    fn scope_comma_list() {
        assert_eq!(
            scope("SELECT | FROM users, orders o, items AS i WHERE 1"),
            vec![
                t(None, "users", None),
                t(None, "orders", Some("o")),
                t(None, "items", Some("i"))
            ]
        );
    }

    #[test]
    fn scope_qualified_and_quoted_names() {
        assert_eq!(
            scope("SELECT | FROM public.users"),
            vec![t(Some("public"), "users", None)]
        );
        assert_eq!(
            scope(r#"SELECT | FROM "public"."Users" "U""#),
            vec![t(Some("public"), "Users", Some("U"))]
        );
        assert_eq!(
            scope("SELECT | FROM `db`.`my table` m"),
            vec![t(Some("db"), "my table", Some("m"))]
        );
        assert_eq!(
            scope("SELECT | FROM [dbo].[Order Items] AS [oi]"),
            vec![t(Some("dbo"), "Order Items", Some("oi"))]
        );
        assert_eq!(
            scope("SELECT | FROM db.dbo.users"),
            vec![t(Some("dbo"), "users", None)]
        );
    }

    #[test]
    fn scope_update_insert_delete() {
        assert_eq!(scope("UPDATE users SET |"), vec![t(None, "users", None)]);
        assert_eq!(
            scope("UPDATE users u SET |"),
            vec![t(None, "users", Some("u"))]
        );
        assert_eq!(
            scope("INSERT INTO public.orders (|"),
            vec![t(Some("public"), "orders", None)]
        );
        assert_eq!(
            scope("INSERT INTO orders VALUES (|"),
            vec![t(None, "orders", None)]
        );
        assert_eq!(
            scope("DELETE FROM users WHERE |"),
            vec![t(None, "users", None)]
        );
    }

    #[test]
    fn scope_subquery_only_the_cursor_level() {
        let sql = "SELECT * FROM users u WHERE u.id IN (SELECT | FROM orders o) AND u.name = ''";
        assert_eq!(scope(sql), vec![t(None, "orders", Some("o"))]);
        let sql = "SELECT | FROM users u WHERE u.id IN (SELECT user_id FROM orders o)";
        assert_eq!(scope(sql), vec![t(None, "users", Some("u"))]);
        // Unclosed subquery being typed.
        assert_eq!(
            scope("SELECT * FROM users WHERE id IN (SELECT | FROM orders"),
            vec![t(None, "orders", None)]
        );
        // A non-subquery group (function call, IN list) is transparent.
        assert_eq!(
            scope("SELECT count(|) FROM users"),
            vec![t(None, "users", None)]
        );
        // A derived table brings nothing, but its alias is not a table.
        assert_eq!(
            scope("SELECT | FROM (SELECT 1) d, users"),
            vec![t(None, "users", None)]
        );
    }

    #[test]
    fn scope_cte_does_not_break() {
        let sql = "WITH recent AS (SELECT * FROM orders) SELECT | FROM recent r JOIN users u ON u.id = r.user_id";
        assert_eq!(scope(sql), vec![t(None, "users", Some("u"))]);
        let sql = "WITH recent AS (SELECT | FROM orders) SELECT * FROM recent";
        assert_eq!(scope(sql), vec![t(None, "orders", None)]);
        let sql = "WITH RECURSIVE a (n) AS (SELECT 1), b AS (SELECT 2) SELECT | FROM a, b, users";
        assert_eq!(scope(sql), vec![t(None, "users", None)]);
    }

    #[test]
    fn scope_multi_statement_script() {
        let sql = "SELECT * FROM users;\nSELECT | FROM orders;\nSELECT * FROM logs";
        assert_eq!(scope(sql), vec![t(None, "orders", None)]);
        assert_eq!(
            scope("SELECT | FROM users; SELECT * FROM orders"),
            vec![t(None, "users", None)]
        );
        assert!(scope("SELECT * FROM users; |").is_empty());
    }

    #[test]
    fn scope_ignores_table_functions_and_select_into() {
        assert_eq!(
            scope("SELECT | FROM generate_series(1, 3) g, users"),
            vec![t(None, "users", None)]
        );
        assert_eq!(
            scope("SELECT | INTO newtab FROM users"),
            vec![t(None, "users", None)]
        );
        // Words in strings and comments are not keywords.
        assert_eq!(
            scope("SELECT 'from x', | -- from y\nFROM users /* join z */"),
            vec![t(None, "users", None)]
        );
    }

    // --- complete: the rules table ----------------------------------------

    #[test]
    fn after_alias_dot_only_that_table() {
        let c = run("SELECT u.| FROM users u JOIN orders o ON o.user_id = u.id");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        assert_eq!(c.items.len(), 3, "no keywords after a qualifier");
        let c = run("SELECT o.u| FROM users u JOIN orders o ON o.user_id = u.id");
        assert_eq!(labels(&c, Kind::Column), ["user_id"]);
        assert_eq!(c.items[0].insert, "user_id");
    }

    #[test]
    fn after_table_dot_and_quoted_qualifier() {
        let c = run("SELECT orders.| FROM orders");
        assert_eq!(labels(&c, Kind::Column), ["id", "user_id", "total"]);
        let c = run(r#"SELECT "o".t| FROM orders "o""#);
        assert_eq!(labels(&c, Kind::Column), ["total"]);
        let c = run("SELECT public.users.e| FROM public.users");
        assert_eq!(labels(&c, Kind::Column), ["email"]);
        // A known table not yet in FROM.
        let c = run("SELECT users.|");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
    }

    #[test]
    fn column_clauses_list_columns_then_keywords() {
        for sql in [
            "SELECT | FROM users",
            "SELECT id, | FROM users",
            "SELECT * FROM users WHERE |",
            "SELECT * FROM users WHERE id = 1 AND |",
            "SELECT * FROM users WHERE id = 1 OR |",
            "SELECT * FROM users u JOIN logs l ON |",
            "SELECT * FROM users GROUP BY |",
            "SELECT * FROM users ORDER BY |",
            "SELECT * FROM users GROUP BY id HAVING |",
            "UPDATE users SET |",
            "UPDATE users SET name = 'x', | WHERE id = 1",
            "SELECT count(|) FROM users",
            "SELECT * FROM users WHERE id IN (1, |)",
        ] {
            let c = run(sql);
            let cols = labels(&c, Kind::Column);
            assert_eq!(cols, ["id", "email", "name"], "{sql}");
            let first_kw = c.items.iter().position(|s| s.kind == Kind::Keyword);
            assert_eq!(first_kw, Some(3), "{sql}: columns before keywords");
            assert!(
                labels(&c, Kind::Keyword).contains(&"SELECT".to_string()),
                "{sql}"
            );
        }
    }

    #[test]
    fn prefix_filter_is_case_insensitive() {
        let c = run("SELECT EM| FROM users");
        assert_eq!(labels(&c, Kind::Column), ["email"]);
        let c = run("SELECT * FROM users WHERE n|");
        assert_eq!(labels(&c, Kind::Column), ["name"]);
        assert!(labels(&c, Kind::Keyword).iter().all(|k| k.starts_with('N')));
        assert!(labels(&c, Kind::Keyword).contains(&"NOT".to_string()));
    }

    #[test]
    fn ambiguous_columns_are_prefixed() {
        let c = run("SELECT | FROM users u JOIN orders o ON o.user_id = u.id");
        assert_eq!(
            labels(&c, Kind::Column),
            ["u.id", "email", "name", "o.id", "user_id", "total"]
        );
        assert_eq!(inserts(&c, Kind::Column)[0], "u.id");
        // Without aliases, the table name qualifies.
        let c = run("SELECT i| FROM users, orders");
        assert_eq!(labels(&c, Kind::Column), ["users.id", "orders.id"]);
        // The same table twice.
        let c = run("SELECT e| FROM users a JOIN users b ON a.id = b.id");
        assert_eq!(labels(&c, Kind::Column), ["a.email", "b.email"]);
    }

    #[test]
    fn primary_key_is_flagged() {
        let c = run("SELECT | FROM users");
        assert_eq!(c.items[0].detail, "PK int");
        assert_eq!(c.items[1].detail, "varchar(255)");
        let fk = |pk, ty: &str| ColumnInfo {
            foreign_key: Some("users".into()),
            ..col("user_id", ty, pk)
        };
        assert_eq!(column_detail(&fk(false, "int")), "FK → users int");
        assert_eq!(column_detail(&fk(true, "int")), "PK FK → users int");
        assert_eq!(column_detail(&fk(false, "")), "FK → users");
        assert_eq!(column_detail(&col("id", "", true)), "PK");
        assert_eq!(
            c.items
                .iter()
                .find(|s| s.kind == Kind::Keyword)
                .unwrap()
                .detail,
            "keyword"
        );
    }

    #[test]
    fn insert_column_list() {
        let c = run("INSERT INTO orders (|");
        assert_eq!(labels(&c, Kind::Column), ["id", "user_id", "total"]);
        assert_eq!(c.items.len(), 3);
        let c = run("INSERT INTO public.orders (id, t|) VALUES (1, 2)");
        assert_eq!(labels(&c, Kind::Column), ["total"]);
        assert_eq!(c.items.len(), 1);
    }

    #[test]
    fn table_clauses_list_tables() {
        for sql in [
            "SELECT * FROM |",
            "SELECT * FROM users u JOIN |",
            "SELECT * FROM users u LEFT JOIN |",
            "SELECT * FROM users, |",
            "UPDATE |",
            "INSERT INTO |",
            "DELETE FROM |",
        ] {
            let c = run(sql);
            assert_eq!(
                labels(&c, Kind::Table),
                ["public.users", "public.orders", "sales.Order Items", "logs"],
                "{sql}"
            );
            assert_eq!(labels(&c, Kind::Schema), ["public", "sales"], "{sql}");
            assert!(labels(&c, Kind::Column).is_empty(), "{sql}");
        }
        let c = run("SELECT * FROM o|");
        assert_eq!(
            labels(&c, Kind::Table),
            ["public.orders", "sales.Order Items"]
        );
        let c = run("SELECT * FROM pub|");
        assert_eq!(labels(&c, Kind::Table), ["public.users", "public.orders"]);
        assert_eq!(labels(&c, Kind::Schema), ["public"]);
    }

    #[test]
    fn table_inserts_are_quoted_for_the_engine() {
        let c = run("SELECT * FROM |");
        assert_eq!(
            inserts(&c, Kind::Table),
            [
                "public.users",
                "public.orders",
                r#"sales."Order Items""#,
                "logs"
            ]
        );
        let c = run_q("SELECT * FROM |", MSSQL, &[]);
        assert_eq!(inserts(&c, Kind::Table)[2], "sales.[Order Items]");
    }

    #[test]
    fn schema_qualifier_filters_tables() {
        let c = run("SELECT * FROM public.|");
        assert_eq!(labels(&c, Kind::Table), ["public.users", "public.orders"]);
        assert_eq!(inserts(&c, Kind::Table), ["users", "orders"]);
        assert!(labels(&c, Kind::Schema).is_empty());
        let c = run("SELECT * FROM users u JOIN sales.|");
        assert_eq!(inserts(&c, Kind::Table), [r#""Order Items""#]);
        let c = run(r#"SELECT * FROM "public".o|"#);
        assert_eq!(inserts(&c, Kind::Table), ["orders"]);
        let c = run("SELECT * FROM [sales].|");
        assert_eq!(labels(&c, Kind::Table), ["sales.Order Items"]);
    }

    #[test]
    fn no_table_cited_falls_back_to_the_old_behaviour() {
        let fallback = vec!["alpha".to_string(), "beta".to_string()];
        let c = run_q("SELECT |", PG, &fallback);
        assert_eq!(labels(&c, Kind::Column), ["alpha", "beta"]);
        assert_eq!(c.items[0].detail, "");
        assert!(c.items.iter().skip(2).all(|s| s.kind == Kind::Keyword));
        let c = run_q("SEL|", PG, &fallback);
        assert_eq!(labels(&c, Kind::Keyword), ["SELECT"]);
        let c = run_q("SELECT a|", PG, &fallback);
        assert_eq!(labels(&c, Kind::Column), ["alpha"]);
        assert!(c.missing.is_empty());
    }

    #[test]
    fn after_a_table_only_keywords() {
        let c = run("SELECT * FROM users |");
        assert!(c.items.iter().all(|s| s.kind == Kind::Keyword));
        assert!(labels(&c, Kind::Keyword).contains(&"WHERE".to_string()));
        let c = run("SELECT * FROM users LIMIT |");
        assert!(c.items.iter().all(|s| s.kind == Kind::Keyword));
    }

    #[test]
    fn missing_tables_are_reported() {
        let c = run("SELECT | FROM users JOIN audit a ON a.id = users.id");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        assert_eq!(
            c.missing,
            [TableRef {
                schema: None,
                name: "audit".into(),
                alias: Some("a".into())
            }]
        );
        let c = run("SELECT a.| FROM users JOIN audit a ON true");
        assert!(c.items.is_empty());
        assert_eq!(c.missing.len(), 1);
        let c = run("INSERT INTO audit (|");
        assert_eq!(c.missing, [tref("audit")]);
        // Loaded tables are not missing; table contexts load nothing.
        assert!(run("SELECT | FROM users").missing.is_empty());
        assert!(run("SELECT * FROM audit JOIN |").missing.is_empty());
    }

    #[test]
    fn subquery_and_script_completion() {
        let c = run("SELECT * FROM users WHERE id IN (SELECT u| FROM orders)");
        assert_eq!(labels(&c, Kind::Column), ["user_id"]);
        let c = run("SELECT * FROM orders; SELECT | FROM users");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        let c = run("WITH r AS (SELECT * FROM orders) SELECT | FROM r");
        assert!(c.missing.is_empty(), "a CTE is not a table to load");
    }

    #[test]
    fn nothing_inside_strings_and_comments() {
        assert!(run("SELECT * FROM users WHERE name = 'na|")
            .items
            .is_empty());
        assert!(run("SELECT * FROM users WHERE name = 'na|me'")
            .items
            .is_empty());
        assert!(run("SELECT * FROM users -- na|").items.is_empty());
        assert!(run("SELECT * /* na| */ FROM users").items.is_empty());
        // Right after a closed string: completion again.
        assert!(!run("SELECT * FROM users WHERE name = 'x' |")
            .items
            .is_empty());
    }

    #[test]
    fn column_inserts_are_quoted_per_engine() {
        let sql = "SELECT | FROM sales.\"Order Items\"";
        let c = run(sql);
        assert_eq!(labels(&c, Kind::Column), ["CreatedAt", "order"]);
        assert_eq!(inserts(&c, Kind::Column), [r#""CreatedAt""#, r#""order""#]);
        let c = run_q("SELECT | FROM sales.`Order Items`", MYSQL, &[]);
        assert_eq!(inserts(&c, Kind::Column), ["CreatedAt", "`order`"]);
        let c = run_q("SELECT | FROM sales.[Order Items]", MSSQL, &[]);
        assert_eq!(inserts(&c, Kind::Column), ["CreatedAt", "[order]"]);
        // Plain names stay bare.
        assert_eq!(
            inserts(&run("SELECT | FROM users"), Kind::Column),
            ["id", "email", "name"]
        );
    }

    #[test]
    fn typed_opening_quote_is_kept() {
        let c = run(r#"SELECT "em| FROM users"#);
        assert_eq!(inserts(&c, Kind::Column), [r#""email""#]);
        assert!(labels(&c, Kind::Keyword).is_empty());
        let c = run_q("SELECT [Cr| FROM sales.[Order Items]", MSSQL, &[]);
        assert_eq!(inserts(&c, Kind::Column), ["[CreatedAt]"]);
        let c = run(r#"SELECT * FROM "Ord| WHERE id = 1"#);
        assert_eq!(
            inserts(&c, Kind::Table),
            [r#""public"."orders""#, r#""sales"."Order Items""#]
        );
    }

    #[test]
    fn cursor_inside_a_word_completes_its_start() {
        let c = run("SELECT em|ail FROM users");
        assert_eq!(labels(&c, Kind::Column), ["email"]);
        let c = run("SELECT * FROM users WHERE id IN (SELECT user_id FROM orders) AND |");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
    }

    #[test]
    fn needs_quotes_rules() {
        assert!(!needs_quotes("user_id", PG));
        assert!(needs_quotes("UserId", PG));
        assert!(!needs_quotes("UserId", MYSQL));
        assert!(needs_quotes("select", MYSQL));
        assert!(needs_quotes("my col", MSSQL));
        assert!(needs_quotes("1st", MSSQL));
        assert!(needs_quotes("", PG));
        assert!(needs_quotes("café", PG));
        assert_eq!(ident(r#"a"b"#, None, PG), r#""a""b""#);
    }

    // --- bars ---------------------------------------------------------------

    fn bar(s: &str, mode: BarMode, table: &TableRef) -> Completions {
        let (text, c) = at(s);
        let tables = tables();
        let cx = Context {
            quotes: PG,
            tables: &tables,
            columns: &columns,
            fallback_columns: &[],
        };
        complete_for_table(&text, c, mode, table, &cx)
    }

    #[test]
    fn where_bar() {
        let c = bar("|", BarMode::Where, &tref("users"));
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        assert_eq!(
            labels(&c, Kind::Keyword),
            [
                "AND",
                "OR",
                "NOT",
                "IS NULL",
                "IS NOT NULL",
                "LIKE",
                "IN",
                "BETWEEN"
            ]
        );
        assert_eq!(c.items[0].kind, Kind::Column);
        let c = bar("id = 1 and e|", BarMode::Where, &tref("users"));
        assert_eq!(labels(&c, Kind::Column), ["email"]);
        assert!(labels(&c, Kind::Keyword).is_empty());
        let c = bar("email i|", BarMode::Where, &tref("users"));
        assert_eq!(labels(&c, Kind::Keyword), ["IS NULL", "IS NOT NULL", "IN"]);
        assert!(bar("name = 'x|", BarMode::Where, &tref("users"))
            .items
            .is_empty());
    }

    #[test]
    fn order_by_bar() {
        let c = bar("total |", BarMode::OrderBy, &tref("orders"));
        assert_eq!(labels(&c, Kind::Column), ["id", "user_id", "total"]);
        assert_eq!(
            labels(&c, Kind::Keyword),
            ["ASC", "DESC", "NULLS FIRST", "NULLS LAST"]
        );
        let c = bar("total D|", BarMode::OrderBy, &tref("orders"));
        assert_eq!(labels(&c, Kind::Keyword), ["DESC"]);
        let c = bar("|", BarMode::OrderBy, &tref("Order Items"));
        assert_eq!(inserts(&c, Kind::Column), [r#""CreatedAt""#, r#""order""#]);
    }

    #[test]
    fn bar_reports_a_missing_table() {
        let c = bar("|", BarMode::Where, &tref("audit"));
        assert_eq!(c.missing, [tref("audit")]);
        assert_eq!(labels(&c, Kind::Keyword).len(), WHERE_KEYWORDS.len());
    }

    // --- robustness ---------------------------------------------------------

    #[test]
    fn every_cursor_position_is_safe() {
        let texts = [
            "SELECT é.\"ü\" FROM \"日本\" é JOIN [🦄 x] AS y ON y.\"a\"\"b\" = é.c; UPDATE `ß`",
            "SELECT 'unterminated é",
            "/* open comment 日本",
            "WITH (( SELECT ) FROM ,, . .. \"",
            "INSERT INTO s.\"t\" (é, ",
            "SELECT * FROM a WHERE x IN (SELECT y FROM b WHERE z = (SELECT 1 FROM c)) -- 🦄",
            ")))(((;;;'\"`[",
            "é.",
            "",
        ];
        let tables = vec![
            "日本.表".to_string(),
            "é".to_string(),
            ".".to_string(),
            "".to_string(),
        ];
        let cx = Context {
            quotes: PG,
            tables: &tables,
            columns: &columns,
            fallback_columns: &["ü".to_string()],
        };
        for text in texts {
            for c in 0..=text.len() + 2 {
                let _ = complete(text, c, &cx);
                let _ = tables_in_scope(text, c);
                let _ = complete_for_table(text, c, BarMode::Where, &tref("users"), &cx);
                let _ = complete_for_table(text, c, BarMode::OrderBy, &tref("x"), &cx);
            }
        }
    }

    #[test]
    fn as_does_not_stop_completion() {
        let c = run("SELECT u.name AS n, | FROM users u");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        let c = run("SELECT COUNT(*) AS c, | FROM users");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        let c = run("SELECT * FROM users AS u, |");
        assert_eq!(labels(&c, Kind::Table).len(), 4);
        let c = run("SELECT * FROM users AS u JOIN orders o USING (id) WHERE |");
        assert_eq!(labels(&c, Kind::Column)[0], "u.id");
        // Right after AS: an alias is typed, no columns.
        let c = run("SELECT name AS | FROM users");
        assert!(labels(&c, Kind::Column).is_empty());
    }

    #[test]
    fn join_using_lists_the_shared_columns() {
        let c = run("SELECT * FROM users u JOIN orders o USING (|");
        assert_eq!(labels(&c, Kind::Column), ["id"]);
        assert_eq!(c.items.len(), 1);
        let c = run("SELECT * FROM orders JOIN users USING (id, |)");
        assert_eq!(labels(&c, Kind::Column), ["id"]);
        let c = run("SELECT * FROM users JOIN audit USING (|)");
        assert!(c.items.is_empty());
        assert_eq!(c.missing, [tref("audit")]);
    }

    #[test]
    fn set_operations_split_the_scope() {
        let sql = "SELECT id FROM users UNION SELECT | FROM orders";
        assert_eq!(scope(sql), vec![t(None, "orders", None)]);
        assert_eq!(labels(&run(sql), Kind::Column), ["id", "user_id", "total"]);
        let sql = "SELECT | FROM users UNION ALL SELECT id FROM orders";
        assert_eq!(scope(sql), vec![t(None, "users", None)]);
        let sql = "SELECT 1 FROM a INTERSECT SELECT 2 FROM b EXCEPT SELECT | FROM c MINUS SELECT 3 FROM d";
        assert_eq!(scope(sql), vec![t(None, "c", None)]);
        let sql = "SELECT * FROM (SELECT id FROM users UNION SELECT | FROM orders) x";
        assert_eq!(scope(sql), vec![t(None, "orders", None)]);
    }

    #[test]
    fn quote_chars_in_strings_and_comments_are_ignored() {
        for sql in [
            r#"SELECT * FROM users WHERE name = 'a"b' AND |"#,
            "SELECT * FROM users WHERE name = '[' AND |",
            "SELECT * FROM users WHERE name = '`' AND |",
            "-- the \"x
 SELECT * FROM users WHERE |",
            "/* [x */ SELECT * FROM users WHERE |",
        ] {
            let c = run(sql);
            assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"], "{sql}");
            assert!(!labels(&c, Kind::Keyword).is_empty(), "{sql}");
            assert_eq!(c.items[0].insert, "id", "{sql}");
        }
        let c = bar(r#"name = 'O"Brien' AND |"#, BarMode::Where, &tref("users"));
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        assert_eq!(c.items[0].insert, "id");
        let c = bar(r#"name = 'O"Brien' AND e|"#, BarMode::Where, &tref("users"));
        assert_eq!(labels(&c, Kind::Column), ["email"]);
    }

    #[test]
    fn current_token_rules() {
        assert_eq!(current_token("SELECT em"), Some((7, "em".into(), None)));
        assert_eq!(
            current_token("SELECT \"em"),
            Some((7, "em".into(), Some('"')))
        );
        assert_eq!(
            current_token("SELECT [em"),
            Some((7, "em".into(), Some('[')))
        );
        assert_eq!(
            current_token("SELECT \"em\""),
            Some((11, String::new(), None))
        );
        assert_eq!(current_token("SELECT u."), Some((9, String::new(), None)));
        assert_eq!(current_token("SELECT 'a"), None);
        assert_eq!(current_token("SELECT -- a"), None);
        assert_eq!(
            current_token("SELECT 'a' "),
            Some((11, String::new(), None))
        );
        assert_eq!(current_token("x = é"), Some((4, "é".into(), None)));
    }

    fn run_known(s: &str, known: &[&str]) -> (Completions, Vec<TableRef>) {
        let (text, c) = at(s);
        let tables: Vec<String> = known.iter().map(|t| t.to_string()).collect();
        let asked = std::cell::RefCell::new(Vec::new());
        let columns = |t: &TableRef| {
            asked.borrow_mut().push(t.clone());
            columns(t)
        };
        let cx = Context {
            quotes: PG,
            tables: &tables,
            columns: &columns,
            fallback_columns: &[],
        };
        let out = complete(&text, c, &cx);
        (out, asked.into_inner())
    }

    fn sref(schema: &str, name: &str, alias: Option<&str>) -> TableRef {
        TableRef {
            schema: Some(schema.into()),
            name: name.into(),
            alias: alias.map(Into::into),
        }
    }

    #[test]
    fn scope_refs_are_resolved_against_known_tables() {
        let (c, asked) = run_known("SELECT | FROM USERS", &["public.users"]);
        assert_eq!(asked, [sref("public", "users", None)]);
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        let (_, asked) = run_known("SELECT | FROM users u", &["public.users", "public.orders"]);
        assert_eq!(asked, [sref("public", "users", Some("u"))]);
        let (c, _) = run_known("SELECT | FROM Audit", &["public.audit"]);
        assert_eq!(c.missing, [sref("public", "audit", None)]);
        // Quoted names match exactly.
        let (c, _) = run_known(r#"SELECT | FROM "Audit""#, &["public.audit"]);
        assert_eq!(c.missing, [tref("Audit")]);
        let (c, _) = run_known(
            r#"SELECT | FROM "Audit""#,
            &["public.Audit", "public.audit"],
        );
        assert_eq!(c.missing, [sref("public", "Audit", None)]);
        // Ambiguous: left as written.
        let (c, _) = run_known("SELECT | FROM audit", &["a.audit", "b.audit"]);
        assert_eq!(c.missing, [tref("audit")]);
        // The schema written must match.
        let (c, _) = run_known("SELECT | FROM x.audit", &["public.audit"]);
        assert_eq!(c.missing, [sref("x", "audit", None)]);
        let (c, _) = run_known("INSERT INTO AUDIT (|", &["public.audit"]);
        assert_eq!(c.missing, [sref("public", "audit", None)]);
    }

    #[test]
    fn correlated_subquery_sees_outer_qualifiers() {
        let sql =
            "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.|)";
        let c = run(sql);
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        // Two levels out.
        let sql = "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE o.id IN \
                   (SELECT 1 FROM logs l WHERE u.|))";
        assert_eq!(labels(&run(sql), Kind::Column), ["id", "email", "name"]);
        // The inner level wins on a shared alias.
        let sql = "SELECT * FROM users x WHERE EXISTS (SELECT 1 FROM orders x WHERE x.|)";
        assert_eq!(labels(&run(sql), Kind::Column), ["id", "user_id", "total"]);
        // Bare columns: only the level of the cursor.
        let sql = "SELECT * FROM users u WHERE EXISTS (SELECT 1 FROM orders o WHERE |)";
        assert_eq!(labels(&run(sql), Kind::Column), ["id", "user_id", "total"]);
    }

    #[test]
    fn dollar_quoted_strings() {
        let c = run(r#"SELECT $$ it's "a" [b $$ AS a, | FROM users"#);
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        let sql = "DO $body$ BEGIN; PERFORM 1; END $body$; SELECT | FROM users";
        assert_eq!(scope(sql), vec![t(None, "users", None)]);
        assert!(run("SELECT $$ ab|").items.is_empty());
        assert!(run("SELECT $x$ a; b| $x$").items.is_empty());
        // `$1` is a parameter, not a quote.
        let c = run("SELECT * FROM users WHERE id = $1 AND |");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
    }

    #[test]
    fn go_is_not_an_alias() {
        assert_eq!(
            scope("SELECT | FROM users\nGO"),
            vec![t(None, "users", None)]
        );
    }

    #[test]
    fn common_reserved_words_are_quoted() {
        for w in [
            "user",
            "USER",
            "limit",
            "end",
            "analyze",
            "current_user",
            "window",
        ] {
            assert!(needs_quotes(w, MYSQL), "{w}");
        }
        assert!(!needs_quotes("users", MYSQL));
    }

    #[test]
    fn statement_boundaries_skip_quoted_semicolons() {
        let c = run("SELECT `a;b`, | FROM users");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        let c = run("SELECT [a;b], | FROM users; SELECT * FROM orders");
        assert_eq!(labels(&c, Kind::Column), ["id", "email", "name"]);
        // A typed opening quote does not swallow the next statements.
        let c = run(r#"SELECT "em| FROM users; SELECT * FROM orders"#);
        assert_eq!(labels(&c, Kind::Column), ["email"]);
        let c = run("SELECT * FROM orders; SELECT [em| FROM users");
        assert_eq!(labels(&c, Kind::Column), ["email"]);
    }

    #[test]
    fn token_start_follows_the_completion_rule() {
        let ts = |s: &str| {
            let (text, c) = at(s);
            token_start(&text, c)
        };
        assert_eq!(ts("SELECT em|"), 7);
        assert_eq!(ts("SELECT u.em| FROM users u"), 9);
        assert_eq!(ts("SELECT * FROM |"), 14);
        assert_eq!(ts(r#"SELECT "em| FROM users"#), 7);
        assert_eq!(ts("SELECT [em|"), 7);
        // Quotes inside earlier literals and comments are not opening quotes.
        assert_eq!(ts(r#"WHERE name = 'O"Brien' AND em|"#), 27);
        assert_eq!(ts("-- \"x\nSELECT em|"), 13);
        // Inside a literal: nothing to replace.
        assert_eq!(ts("SELECT 'ab|"), 10);
        assert_eq!(ts("SELECT é|"), 7);
        // Past the end or inside a multibyte char: clamped to a boundary.
        assert_eq!(token_start("é", 1), 0);
        assert_eq!(token_start("ab", 9), 0);
    }
}
