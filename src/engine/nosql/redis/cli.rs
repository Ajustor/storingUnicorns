//! Redis console syntax (plan 13, « Syntaxe Redis »): redis-cli tokenizing,
//! safety classification and reply shaping.

use ::redis::Value;

use super::display_bytes;
use crate::engine::models::{Column, QueryResult, NULL_CELL};

/// What to do with a command before sending it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Safety {
    Safe,
    /// Destructive: confirm first (French reason).
    Confirm(String),
    /// Never sent from the console (blocking, subscriptions…).
    Refuse(String),
    /// `KEYS`: run as an iterative `SCAN MATCH`.
    RewriteScan,
}

/// The arguments of one command line (redis-cli quoting rules): tokens
/// separated by blanks; `"…"` with `\" \\ \n \r \t \b \a \xHH`; `'…'`
/// literal but for `\'`; a closing quote must be followed by a blank or the
/// end of the line.
pub fn tokenize(line: &str) -> Result<Vec<Vec<u8>>, String> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return Ok(out);
        }
        let mut token = Vec::new();
        loop {
            match b.get(i) {
                None => break,
                Some(c) if c.is_ascii_whitespace() => break,
                Some(b'"') => {
                    i = read_double_quoted(b, i + 1, &mut token)?;
                }
                Some(b'\'') => {
                    i = read_single_quoted(b, i + 1, &mut token)?;
                }
                Some(&c) => {
                    token.push(c);
                    i += 1;
                }
            }
        }
        out.push(token);
    }
}

/// Reads a `"…"` body from `i` (after the opening quote) into `token`;
/// returns the index after the closing quote.
fn read_double_quoted(b: &[u8], mut i: usize, token: &mut Vec<u8>) -> Result<usize, String> {
    loop {
        match b.get(i) {
            None => return Err("Guillemets non fermés".into()),
            Some(b'\\') => {
                let Some(&e) = b.get(i + 1) else {
                    return Err("Guillemets non fermés".into());
                };
                if e == b'x' {
                    if let (Some(h), Some(l)) = (
                        b.get(i + 2).and_then(|c| hex(*c)),
                        b.get(i + 3).and_then(|c| hex(*c)),
                    ) {
                        token.push(h * 16 + l);
                        i += 4;
                        continue;
                    }
                }
                token.push(match e {
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'b' => 0x08,
                    b'a' => 0x07,
                    other => other,
                });
                i += 2;
            }
            Some(b'"') => return closing(b, i + 1),
            Some(&c) => {
                token.push(c);
                i += 1;
            }
        }
    }
}

fn read_single_quoted(b: &[u8], mut i: usize, token: &mut Vec<u8>) -> Result<usize, String> {
    loop {
        match b.get(i) {
            None => return Err("Guillemets non fermés".into()),
            Some(b'\\') if b.get(i + 1) == Some(&b'\'') => {
                token.push(b'\'');
                i += 2;
            }
            Some(b'\'') => return closing(b, i + 1),
            Some(&c) => {
                token.push(c);
                i += 1;
            }
        }
    }
}

/// A closing quote must be followed by a blank or the end of the line.
fn closing(b: &[u8], i: usize) -> Result<usize, String> {
    match b.get(i) {
        None => Ok(i),
        Some(c) if c.is_ascii_whitespace() => Ok(i),
        Some(_) => Err("Un guillemet fermant doit être suivi d'un espace".into()),
    }
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// One argument as typed in the console: bare when it can be, else
/// double-quoted with escapes (`tokenize` reads it back).
pub fn quote_arg(arg: &[u8]) -> String {
    let bare = !arg.is_empty()
        && std::str::from_utf8(arg).is_ok_and(|s| {
            !s.chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\'')
        });
    if bare {
        String::from_utf8_lossy(arg).into_owned()
    } else {
        super::quoted(arg)
    }
}

fn upper(arg: Option<&Vec<u8>>) -> String {
    arg.map(|a| String::from_utf8_lossy(a).to_ascii_uppercase())
        .unwrap_or_default()
}

/// Whether `arg` is a zero timeout ("0", "0.0").
fn zero(arg: Option<&Vec<u8>>) -> bool {
    arg.and_then(|a| std::str::from_utf8(a).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .is_some_and(|t| t == 0.0)
}

pub fn classify(argv: &[Vec<u8>]) -> Safety {
    let name = upper(argv.first());
    let sub = upper(argv.get(1));
    let refuse_block = || {
        Safety::Refuse(format!(
            "{name} avec un délai 0 bloquerait la console indéfiniment : indiquer un délai en secondes"
        ))
    };
    match name.as_str() {
        "" => Safety::Safe,
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "MONITOR" | "SYNC" | "PSYNC" => Safety::Refuse(
            format!("{name} n'est pas disponible dans la console (la connexion resterait bloquée)"),
        ),
        "BLPOP" | "BRPOP" | "BLMOVE" | "BRPOPLPUSH" | "BZPOPMIN" | "BZPOPMAX"
            if argv.len() > 1 && zero(argv.last()) =>
        {
            refuse_block()
        }
        "BLMPOP" | "BZMPOP" if zero(argv.get(1)) => refuse_block(),
        "XREAD" | "XREADGROUP"
            if argv
                .iter()
                .position(|a| a.eq_ignore_ascii_case(b"BLOCK"))
                .is_some_and(|p| zero(argv.get(p + 1))) =>
        {
            refuse_block()
        }
        "WAIT" if zero(argv.get(2)) => refuse_block(),
        "WAITAOF" if zero(argv.get(3)) => refuse_block(),
        "KEYS" => Safety::RewriteScan,
        "FLUSHALL" => Safety::Confirm("Supprime toutes les clés de toutes les bases".into()),
        "FLUSHDB" => Safety::Confirm("Supprime toutes les clés de la base courante".into()),
        "SHUTDOWN" => Safety::Confirm("Arrête le serveur Redis".into()),
        "SWAPDB" => Safety::Confirm("Échange le contenu de deux bases".into()),
        "MIGRATE" => Safety::Confirm("Déplace des clés vers un autre serveur".into()),
        "REPLICAOF" | "SLAVEOF" => Safety::Confirm("Change la réplication du serveur".into()),
        "FAILOVER" => Safety::Confirm("Bascule le serveur vers un réplica".into()),
        "DEBUG" => Safety::Confirm("Commande de débogage du serveur".into()),
        "CLUSTER" => Safety::Confirm(format!("Modifie ou interroge le cluster ({name} {sub})")),
        "CONFIG" if matches!(sub.as_str(), "SET" | "REWRITE" | "RESETSTAT") => Safety::Confirm(
            format!("Modifie la configuration du serveur ({name} {sub})"),
        ),
        "SCRIPT" if sub == "FLUSH" => Safety::Confirm("Supprime tous les scripts Lua".into()),
        "FUNCTION" if matches!(sub.as_str(), "FLUSH" | "DELETE" | "RESTORE") => {
            Safety::Confirm(format!("Modifie les fonctions du serveur ({name} {sub})"))
        }
        "CLIENT" if matches!(sub.as_str(), "KILL" | "PAUSE") => {
            Safety::Confirm(format!("Agit sur les clients connectés ({name} {sub})"))
        }
        "ACL" if matches!(sub.as_str(), "SETUSER" | "DELUSER") => {
            Safety::Confirm(format!("Modifie les utilisateurs ({name} {sub})"))
        }
        "MODULE" if matches!(sub.as_str(), "LOAD" | "UNLOAD" | "LOADEX") => {
            Safety::Confirm(format!("Charge ou décharge un module ({name} {sub})"))
        }
        _ => Safety::Safe,
    }
}

fn col(name: &str) -> Column {
    Column {
        name: name.to_string(),
        type_name: String::new(),
        nullable: true,
        is_primary_key: false,
    }
}

fn grid(columns: &[&str], rows: Vec<Vec<String>>) -> QueryResult {
    QueryResult {
        columns: columns.iter().map(|c| col(c)).collect(),
        rows,
        ..Default::default()
    }
}

/// A scalar or nested reply as one cell.
pub(crate) fn cell(v: &Value) -> String {
    match v {
        Value::Nil => NULL_CELL.to_string(),
        _ => nested(v, false),
    }
}

/// Compact text of a reply; nested strings are quoted when `quote`.
fn nested(v: &Value, quote: bool) -> String {
    let text = |b: &[u8]| {
        if quote {
            super::quoted(b)
        } else {
            display_bytes(b)
        }
    };
    match v {
        Value::Nil => "(nil)".into(),
        Value::Int(i) => i.to_string(),
        Value::BulkString(b) => text(b),
        Value::SimpleString(s) => text(s.as_bytes()),
        Value::Okay => "OK".into(),
        Value::Double(d) => format_score(*d),
        Value::Boolean(b) => b.to_string(),
        Value::VerbatimString { text: t, .. } => text(t.as_bytes()),
        Value::BigNumber(n) => format!("{n:?}"),
        Value::Array(items) | Value::Set(items) | Value::Push { data: items, .. } => format!(
            "[{}]",
            items
                .iter()
                .map(|i| nested(i, true))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Map(pairs) => format!(
            "{{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", nested(k, true), nested(v, true)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Attribute { data, .. } => nested(data, quote),
        Value::ServerError(e) => format!("(erreur) {e}"),
        other => format!("{other:?}"),
    }
}

/// A score as Redis reads it back (`+inf`, `-inf`, shortest decimal).
pub(crate) fn format_score(s: f64) -> String {
    if s == f64::INFINITY {
        "+inf".into()
    } else if s == f64::NEG_INFINITY {
        "-inf".into()
    } else {
        s.to_string()
    }
}

/// Elements of an array / set reply.
fn items(v: &Value) -> Option<&[Value]> {
    match v {
        Value::Array(i) | Value::Set(i) => Some(i),
        _ => None,
    }
}

/// Pairs of a flat `[k, v, k, v]` array (RESP2), of `[[k, v], …]` (RESP3)
/// or of a map.
fn pairs(v: &Value) -> Option<Vec<(&Value, &Value)>> {
    if let Value::Map(m) = v {
        return Some(m.iter().map(|(k, v)| (k, v)).collect());
    }
    let items = items(v)?;
    if !items.is_empty() && items.iter().all(|i| items_len(i) == Some(2)) {
        return Some(
            items
                .iter()
                .map(|i| {
                    let p = self::items(i).unwrap();
                    (&p[0], &p[1])
                })
                .collect(),
        );
    }
    if items.len() % 2 != 0 {
        return None;
    }
    Some(items.chunks(2).map(|c| (&c[0], &c[1])).collect())
}

fn items_len(v: &Value) -> Option<usize> {
    items(v).map(|i| i.len())
}

/// Column names of the pair replies.
fn pair_columns(name: &str, argv: &[Vec<u8>]) -> Option<[&'static str; 2]> {
    let has = |w: &[u8]| argv.iter().skip(1).any(|a| a.eq_ignore_ascii_case(w));
    match name {
        "HGETALL" => Some(["champ", "valeur"]),
        "HRANDFIELD" if has(b"WITHVALUES") => Some(["champ", "valeur"]),
        "CONFIG" if upper(argv.get(1)) == "GET" => Some(["paramètre", "valeur"]),
        "ZPOPMIN" | "ZPOPMAX" => Some(["membre", "score"]),
        n if n.starts_with('Z') && has(b"WITHSCORES") => Some(["membre", "score"]),
        _ => None,
    }
}

/// Rows of a stream range reply: `id, champs`.
pub(crate) fn stream_rows(entries: &[Value]) -> Option<Vec<Vec<String>>> {
    entries
        .iter()
        .map(|e| {
            let e = items(e)?;
            let [id, fields] = e else { return None };
            let fields = items(fields)?;
            let text = fields
                .iter()
                .map(|f| match f {
                    Value::BulkString(b) => Some(quote_arg(b)),
                    Value::SimpleString(s) => Some(quote_arg(s.as_bytes())),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?
                .join(" ");
            Some(vec![nested(id, false), text])
        })
        .collect()
}

/// The reply of `argv` as a grid: nil → NULL; scalar → one cell; arrays →
/// `#, valeur`; pair replies → two columns; `*SCAN` → the elements, the
/// next cursor in the first column's name; `XRANGE` → `id, champs`;
/// `INFO` → `section, clé, valeur`; RESP3 maps → two columns.
pub fn reply_to_result(argv: &[Vec<u8>], reply: &Value) -> QueryResult {
    let name = upper(argv.first());
    if let Value::Attribute { data, .. } = reply {
        return reply_to_result(argv, data);
    }
    // SCAN family: [cursor, elements].
    if name.ends_with("SCAN") {
        if let Some([cursor, elements]) = items(reply).and_then(|i| {
            if i.len() == 2 {
                Some([&i[0], &i[1]])
            } else {
                None
            }
        }) {
            let next = format!("(curseur suivant : {})", nested(cursor, false));
            let mut result = match name.as_str() {
                "HSCAN" | "ZSCAN" => {
                    let names = if name == "HSCAN" {
                        ["champ", "valeur"]
                    } else {
                        ["membre", "score"]
                    };
                    match pairs(elements) {
                        Some(p) => grid(
                            &names,
                            p.iter().map(|(k, v)| vec![cell(k), cell(v)]).collect(),
                        ),
                        None => list_grid(elements),
                    }
                }
                "SSCAN" => grid(
                    &["membre"],
                    items(elements)
                        .unwrap_or_default()
                        .iter()
                        .map(|v| vec![cell(v)])
                        .collect(),
                ),
                _ => grid(
                    &["clé"],
                    items(elements)
                        .unwrap_or_default()
                        .iter()
                        .map(|v| vec![cell(v)])
                        .collect(),
                ),
            };
            if let Some(c) = result.columns.first_mut() {
                c.name = format!("{} {next}", c.name);
            }
            return result;
        }
    }
    if name == "INFO" {
        if let Some(text) = text_of(reply) {
            return info_grid(&text);
        }
    }
    if matches!(name.as_str(), "XRANGE" | "XREVRANGE") {
        if let Some(rows) = items(reply).and_then(stream_rows) {
            return grid(&["id", "champs"], rows);
        }
    }
    if let Some(names) = pair_columns(&name, argv) {
        if let Some(p) = pairs(reply) {
            return grid(
                &names,
                p.iter().map(|(k, v)| vec![cell(k), cell(v)]).collect(),
            );
        }
    }
    match reply {
        Value::Map(m) => grid(
            &["clé", "valeur"],
            m.iter().map(|(k, v)| vec![cell(k), cell(v)]).collect(),
        ),
        Value::Array(_) | Value::Set(_) => list_grid(reply),
        Value::Push { data, .. } => list_grid(&Value::Array(data.clone())),
        other => grid(&["résultat"], vec![vec![cell(other)]]),
    }
}

/// `#, valeur` (1-based like redis-cli).
fn list_grid(v: &Value) -> QueryResult {
    grid(
        &["#", "valeur"],
        items(v)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, v)| vec![(i + 1).to_string(), cell(v)])
            .collect(),
    )
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Value::SimpleString(s) => Some(s.clone()),
        Value::VerbatimString { text, .. } => Some(text.clone()),
        _ => None,
    }
}

/// `INFO` text as `section, clé, valeur` rows.
fn info_grid(text: &str) -> QueryResult {
    let mut section = String::new();
    let mut rows = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(s) = line.strip_prefix('#') {
            section = s.trim().to_string();
        } else if let Some((k, v)) = line.split_once(':') {
            rows.push(vec![section.clone(), k.to_string(), v.to_string()]);
        }
    }
    grid(&["section", "clé", "valeur"], rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(line: &str) -> Vec<String> {
        tokenize(line)
            .unwrap()
            .into_iter()
            .map(|t| String::from_utf8(t).unwrap())
            .collect()
    }

    fn argv(line: &str) -> Vec<Vec<u8>> {
        tokenize(line).unwrap()
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn tokenize_splits_on_blanks() {
        assert_eq!(toks("  SET  k\tv  "), ["SET", "k", "v"]);
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn tokenize_double_quotes_with_escapes() {
        assert_eq!(toks(r#"SET "a b" "x\"y\\z""#), ["SET", "a b", "x\"y\\z"]);
        assert_eq!(toks(r#""\n\r\t""#), ["\n\r\t"]);
        assert_eq!(
            tokenize(r#""\xff\x00\a\b""#).unwrap(),
            [vec![0xff, 0, 7, 8]]
        );
        // Invalid hex and unknown escapes are literal.
        assert_eq!(toks(r#""\xzz\q""#), ["xzzq"]);
        assert_eq!(toks(r#""""#), [""]);
    }

    #[test]
    fn tokenize_single_quotes_are_literal() {
        assert_eq!(toks(r#"'a\nb' 'it\'s'"#), ["a\\nb", "it's"]);
        assert_eq!(toks(r#"'say "hi"'"#), ["say \"hi\""]);
    }

    #[test]
    fn tokenize_quotes_inside_a_token() {
        assert_eq!(toks(r#"a"b c""#), ["ab c"]);
    }

    #[test]
    fn tokenize_errors() {
        assert!(tokenize(r#"SET "abc"#).is_err());
        assert!(tokenize("SET 'abc").is_err());
        assert!(tokenize(r#"SET "a"b"#).is_err());
        assert!(tokenize(r#"SET "a\"#).is_err());
    }

    #[test]
    fn quote_arg_round_trips() {
        for arg in [
            b"plain".to_vec(),
            b"with space".to_vec(),
            b"".to_vec(),
            b"q\"uote's".to_vec(),
            vec![0xff, 0x00, b'\n'],
            "héllo".as_bytes().to_vec(),
        ] {
            let line = format!("CMD {}", quote_arg(&arg));
            assert_eq!(tokenize(&line).unwrap()[1], arg, "{line}");
        }
        assert_eq!(quote_arg(b"user:1"), "user:1");
    }

    #[test]
    fn classify_each_class() {
        assert_eq!(classify(&argv("GET k")), Safety::Safe);
        assert_eq!(classify(&[]), Safety::Safe);
        assert_eq!(classify(&argv("keys user:*")), Safety::RewriteScan);
        for line in [
            "FLUSHALL",
            "flushdb async",
            "SHUTDOWN NOSAVE",
            "DEBUG SLEEP 1",
            "SWAPDB 0 1",
            "MIGRATE h 6379 k 0 1000",
            "REPLICAOF h 6379",
            "SLAVEOF NO ONE",
            "FAILOVER",
            "CONFIG SET maxmemory 1",
            "config rewrite",
            "CONFIG RESETSTAT",
            "SCRIPT FLUSH",
            "FUNCTION FLUSH",
            "FUNCTION DELETE lib",
            "FUNCTION RESTORE x",
            "CLIENT KILL ID 1",
            "CLIENT PAUSE 10",
            "ACL SETUSER u on",
            "ACL DELUSER u",
            "MODULE LOAD /x.so",
            "MODULE UNLOAD m",
            "CLUSTER INFO",
        ] {
            assert!(
                matches!(classify(&argv(line)), Safety::Confirm(_)),
                "{line}"
            );
        }
        for line in [
            "CONFIG GET maxmemory",
            "SCRIPT EXISTS a",
            "CLIENT LIST",
            "ACL WHOAMI",
        ] {
            assert_eq!(classify(&argv(line)), Safety::Safe, "{line}");
        }
        for line in [
            "SUBSCRIBE ch",
            "PSUBSCRIBE c*",
            "SSUBSCRIBE c",
            "MONITOR",
            "SYNC",
            "PSYNC ? -1",
        ] {
            assert!(matches!(classify(&argv(line)), Safety::Refuse(_)), "{line}");
        }
    }

    #[test]
    fn classify_blocking_commands_by_timeout() {
        let refused = |l: &str| matches!(classify(&argv(l)), Safety::Refuse(_));
        assert!(refused("BLPOP a b 0"));
        assert!(!refused("BLPOP a b 5"));
        assert!(refused("BRPOP a 0.0"));
        assert!(!refused("BRPOP a 0.5"));
        assert!(refused("BLMOVE a b LEFT RIGHT 0"));
        assert!(refused("BRPOPLPUSH a b 0"));
        assert!(refused("BZPOPMIN z 0"));
        assert!(!refused("BZPOPMAX z 1"));
        assert!(refused("BLMPOP 0 1 a LEFT"));
        assert!(!refused("BLMPOP 2 1 a LEFT"));
        assert!(refused("BZMPOP 0 1 z MIN"));
        assert!(refused("XREAD COUNT 1 BLOCK 0 STREAMS s $"));
        assert!(!refused("XREAD BLOCK 100 STREAMS s $"));
        assert!(!refused("XREAD STREAMS s 0"));
        assert!(refused("XREADGROUP GROUP g c block 0 STREAMS s >"));
        assert!(refused("WAIT 1 0"));
        assert!(!refused("WAIT 1 100"));
        assert!(refused("WAITAOF 1 0 0"));
    }

    #[test]
    fn scalars_are_one_cell() {
        let r = reply_to_result(&argv("GET k"), &Value::Nil);
        assert_eq!(r.columns.len(), 1);
        assert_eq!(r.rows, [[NULL_CELL]]);
        assert_eq!(
            reply_to_result(&argv("INCR k"), &Value::Int(3)).rows,
            [["3"]]
        );
        assert_eq!(
            reply_to_result(&argv("SET k v"), &Value::Okay).rows,
            [["OK"]]
        );
        assert_eq!(
            reply_to_result(&argv("PING"), &Value::SimpleString("PONG".into())).rows,
            [["PONG"]]
        );
        assert_eq!(
            reply_to_result(&argv("GET k"), &Value::BulkString(vec![0xff])).rows,
            [[r#""\xff""#]]
        );
        assert_eq!(
            reply_to_result(&argv("ZSCORE z m"), &Value::Double(1.5)).rows,
            [["1.5"]]
        );
        assert_eq!(
            reply_to_result(&argv("X"), &Value::Boolean(true)).rows,
            [["true"]]
        );
    }

    #[test]
    fn arrays_are_numbered() {
        let r = reply_to_result(
            &argv("LRANGE l 0 -1"),
            &Value::Array(vec![bulk("a"), Value::Nil, Value::Int(2)]),
        );
        assert_eq!(
            r.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["#", "valeur"]
        );
        assert_eq!(r.rows, [["1", "a"], ["2", NULL_CELL], ["3", "2"]]);
        let empty = reply_to_result(&argv("SMEMBERS s"), &Value::Array(vec![]));
        assert_eq!(empty.columns.len(), 2);
        assert!(empty.rows.is_empty());
        let nested = reply_to_result(
            &argv("X"),
            &Value::Array(vec![Value::Array(vec![bulk("a b"), Value::Int(1)])]),
        );
        assert_eq!(nested.rows, [["1", r#"["a b", 1]"#]]);
    }

    #[test]
    fn pair_replies_have_two_columns() {
        let flat = Value::Array(vec![bulk("f1"), bulk("v1"), bulk("f2"), bulk("v2")]);
        let r = reply_to_result(&argv("HGETALL h"), &flat);
        assert_eq!(r.columns[0].name, "champ");
        assert_eq!(r.rows, [["f1", "v1"], ["f2", "v2"]]);
        let r = reply_to_result(&argv("CONFIG GET maxmemory"), &flat);
        assert_eq!(r.columns[0].name, "paramètre");
        let r = reply_to_result(&argv("HRANDFIELD h 2 WITHVALUES"), &flat);
        assert_eq!(r.columns[1].name, "valeur");
        let scores = Value::Array(vec![bulk("m"), bulk("1.5")]);
        for line in [
            "ZRANGE z 0 -1 WITHSCORES",
            "ZREVRANGEBYSCORE z +inf -inf withscores",
            "ZPOPMIN z",
            "ZPOPMAX z 2",
            "ZRANDMEMBER z 1 WITHSCORES",
        ] {
            let r = reply_to_result(&argv(line), &scores);
            assert_eq!(r.columns[1].name, "score", "{line}");
            assert_eq!(r.rows, [["m", "1.5"]], "{line}");
        }
        // RESP3: [[member, score], …]
        let resp3 = Value::Array(vec![Value::Array(vec![bulk("m"), Value::Double(2.0)])]);
        assert_eq!(
            reply_to_result(&argv("ZRANGE z 0 -1 WITHSCORES"), &resp3).rows,
            [["m", "2"]]
        );
        // Without WITHSCORES: a plain array.
        let r = reply_to_result(&argv("ZRANGE z 0 -1"), &scores);
        assert_eq!(r.columns[0].name, "#");
        // RESP3 map.
        let map = Value::Map(vec![(bulk("a"), Value::Int(1))]);
        let r = reply_to_result(&argv("HELLO"), &map);
        assert_eq!(r.columns.len(), 2);
        assert_eq!(r.rows, [["a", "1"]]);
    }

    #[test]
    fn scan_replies_show_the_next_cursor() {
        let reply = Value::Array(vec![
            bulk("17"),
            Value::Array(vec![bulk("user:1"), bulk("user:2")]),
        ]);
        let r = reply_to_result(&argv("SCAN 0 MATCH user:*"), &reply);
        assert_eq!(r.columns[0].name, "clé (curseur suivant : 17)");
        assert_eq!(r.rows, [["user:1"], ["user:2"]]);
        let r = reply_to_result(&argv("SSCAN s 0"), &reply);
        assert!(r.columns[0].name.starts_with("membre"));
        let reply = Value::Array(vec![bulk("0"), Value::Array(vec![bulk("f"), bulk("v")])]);
        let r = reply_to_result(&argv("HSCAN h 0"), &reply);
        assert_eq!(r.columns[0].name, "champ (curseur suivant : 0)");
        assert_eq!(r.rows, [["f", "v"]]);
        let r = reply_to_result(&argv("ZSCAN z 0"), &reply);
        assert_eq!(r.columns[1].name, "score");
    }

    #[test]
    fn stream_ranges_have_id_and_fields() {
        let reply = Value::Array(vec![Value::Array(vec![
            bulk("1-0"),
            Value::Array(vec![bulk("name"), bulk("Ada L"), bulk("n"), bulk("1")]),
        ])]);
        let r = reply_to_result(&argv("XRANGE s - +"), &reply);
        assert_eq!(r.columns[0].name, "id");
        assert_eq!(r.rows, [["1-0", r#"name "Ada L" n 1"#]]);
        let r = reply_to_result(&argv("XREVRANGE s + -"), &reply);
        assert_eq!(r.columns[1].name, "champs");
    }

    #[test]
    fn info_is_split_in_sections() {
        let text = "# Server\r\nredis_version:8.0.0\r\nos:Linux\r\n\r\n# Keyspace\r\ndb0:keys=1,expires=0\r\n";
        let r = reply_to_result(&argv("INFO"), &bulk(text));
        assert_eq!(
            r.columns
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["section", "clé", "valeur"]
        );
        assert_eq!(r.rows[0], ["Server", "redis_version", "8.0.0"]);
        assert_eq!(r.rows[2], ["Keyspace", "db0", "keys=1,expires=0"]);
    }

    #[test]
    fn exec_replies_are_numbered() {
        let r = reply_to_result(
            &argv("EXEC"),
            &Value::Array(vec![Value::Okay, Value::Int(1)]),
        );
        assert_eq!(r.rows, [["1", "OK"], ["2", "1"]]);
    }
}
