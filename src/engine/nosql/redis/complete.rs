//! Console completion: command names (with their arguments) at the start of
//! a line, known keys as the first argument of key commands.

use super::cli::quote_arg;
use crate::engine::nosql::CompletionContext;
use crate::engine::sql::completion::{Completions, Kind, Suggestion};

/// Name, arguments, whether the first argument is a key.
const COMMANDS: &[(&str, &str, bool)] = &[
    (
        "ACL",
        "WHOAMI | LIST | USERS | CAT | SETUSER | DELUSER …",
        false,
    ),
    ("APPEND", "clé valeur", true),
    ("BITCOUNT", "clé [début fin]", true),
    (
        "BLMOVE",
        "source destination LEFT|RIGHT LEFT|RIGHT délai",
        true,
    ),
    ("BLPOP", "clé [clé …] délai", true),
    ("BRPOP", "clé [clé …] délai", true),
    ("BZPOPMAX", "clé [clé …] délai", true),
    ("BZPOPMIN", "clé [clé …] délai", true),
    ("CLIENT", "LIST | INFO | SETNAME | KILL | PAUSE …", false),
    (
        "CONFIG",
        "GET paramètre | SET paramètre valeur | REWRITE",
        false,
    ),
    ("COPY", "source destination [DB n] [REPLACE]", true),
    ("DBSIZE", "", false),
    ("DECR", "clé", true),
    ("DECRBY", "clé n", true),
    ("DEL", "clé [clé …]", true),
    ("DISCARD", "", false),
    ("DUMP", "clé", true),
    ("ECHO", "message", false),
    ("EVAL", "script nb_clés [clé …] [arg …]", false),
    ("EVALSHA", "sha1 nb_clés [clé …] [arg …]", false),
    ("EXEC", "", false),
    ("EXISTS", "clé [clé …]", true),
    ("EXPIRE", "clé secondes [NX|XX|GT|LT]", true),
    ("EXPIREAT", "clé horodatage", true),
    ("EXPIRETIME", "clé", true),
    ("FLUSHALL", "[ASYNC|SYNC]", false),
    ("FLUSHDB", "[ASYNC|SYNC]", false),
    ("GEOADD", "clé longitude latitude membre …", true),
    ("GEODIST", "clé membre1 membre2 [M|KM]", true),
    ("GEOPOS", "clé membre [membre …]", true),
    (
        "GEOSEARCH",
        "clé FROMMEMBER m | FROMLONLAT lon lat BYRADIUS r unité",
        true,
    ),
    ("GET", "clé", true),
    ("GETDEL", "clé", true),
    ("GETEX", "clé [EX s | PX ms | PERSIST]", true),
    ("GETRANGE", "clé début fin", true),
    ("GETSET", "clé valeur", true),
    ("HDEL", "clé champ [champ …]", true),
    ("HELLO", "[protover]", false),
    ("HEXISTS", "clé champ", true),
    ("HEXPIRE", "clé secondes FIELDS n champ …", true),
    ("HGET", "clé champ", true),
    ("HGETALL", "clé", true),
    ("HINCRBY", "clé champ n", true),
    ("HINCRBYFLOAT", "clé champ n", true),
    ("HKEYS", "clé", true),
    ("HLEN", "clé", true),
    ("HMGET", "clé champ [champ …]", true),
    ("HRANDFIELD", "clé [nombre [WITHVALUES]]", true),
    ("HSCAN", "clé curseur [MATCH motif] [COUNT n]", true),
    ("HSET", "clé champ valeur [champ valeur …]", true),
    ("HSETNX", "clé champ valeur", true),
    ("HSTRLEN", "clé champ", true),
    ("HTTL", "clé FIELDS n champ …", true),
    ("HVALS", "clé", true),
    ("INCR", "clé", true),
    ("INCRBY", "clé n", true),
    ("INCRBYFLOAT", "clé n", true),
    ("INFO", "[section]", false),
    ("KEYS", "motif", false),
    ("LASTSAVE", "", false),
    ("LINDEX", "clé index", true),
    ("LINSERT", "clé BEFORE|AFTER pivot valeur", true),
    ("LLEN", "clé", true),
    ("LMOVE", "source destination LEFT|RIGHT LEFT|RIGHT", true),
    ("LMPOP", "nb_clés clé … LEFT|RIGHT [COUNT n]", false),
    ("LPOP", "clé [nombre]", true),
    ("LPOS", "clé élément [RANK r] [COUNT n]", true),
    ("LPUSH", "clé valeur [valeur …]", true),
    ("LPUSHX", "clé valeur [valeur …]", true),
    ("LRANGE", "clé début fin", true),
    ("LREM", "clé nombre valeur", true),
    ("LSET", "clé index valeur", true),
    ("LTRIM", "clé début fin", true),
    ("MEMORY", "USAGE clé | STATS | DOCTOR", false),
    ("MGET", "clé [clé …]", true),
    ("MOVE", "clé base", true),
    ("MSET", "clé valeur [clé valeur …]", true),
    ("MSETNX", "clé valeur [clé valeur …]", true),
    ("MULTI", "", false),
    ("OBJECT", "ENCODING | FREQ | IDLETIME | REFCOUNT clé", false),
    ("PERSIST", "clé", true),
    ("PEXPIRE", "clé millisecondes", true),
    ("PEXPIREAT", "clé horodatage_ms", true),
    ("PFADD", "clé élément [élément …]", true),
    ("PFCOUNT", "clé [clé …]", true),
    ("PFMERGE", "destination source [source …]", true),
    ("PING", "[message]", false),
    ("PSETEX", "clé millisecondes valeur", true),
    ("PTTL", "clé", true),
    ("PUBLISH", "canal message", false),
    ("RANDOMKEY", "", false),
    ("RENAME", "clé nouvelle_clé", true),
    ("RENAMENX", "clé nouvelle_clé", true),
    ("RPOP", "clé [nombre]", true),
    ("RPUSH", "clé valeur [valeur …]", true),
    ("RPUSHX", "clé valeur [valeur …]", true),
    ("SADD", "clé membre [membre …]", true),
    ("SCAN", "curseur [MATCH motif] [COUNT n] [TYPE type]", false),
    ("SCARD", "clé", true),
    ("SCRIPT", "EXISTS | FLUSH | LOAD …", false),
    ("SDIFF", "clé [clé …]", true),
    ("SDIFFSTORE", "destination clé [clé …]", true),
    ("SELECT", "base", false),
    (
        "SET",
        "clé valeur [NX|XX] [GET] [EX s | PX ms | KEEPTTL]",
        true,
    ),
    ("SETEX", "clé secondes valeur", true),
    ("SETNX", "clé valeur", true),
    ("SETRANGE", "clé décalage valeur", true),
    ("SINTER", "clé [clé …]", true),
    ("SINTERCARD", "nb_clés clé … [LIMIT n]", false),
    ("SINTERSTORE", "destination clé [clé …]", true),
    ("SISMEMBER", "clé membre", true),
    ("SMEMBERS", "clé", true),
    ("SMISMEMBER", "clé membre [membre …]", true),
    ("SMOVE", "source destination membre", true),
    (
        "SORT",
        "clé [BY motif] [LIMIT o n] [ASC|DESC] [ALPHA]",
        true,
    ),
    ("SPOP", "clé [nombre]", true),
    ("SRANDMEMBER", "clé [nombre]", true),
    ("SREM", "clé membre [membre …]", true),
    ("SSCAN", "clé curseur [MATCH motif] [COUNT n]", true),
    ("STRLEN", "clé", true),
    ("SUNION", "clé [clé …]", true),
    ("SUNIONSTORE", "destination clé [clé …]", true),
    ("SWAPDB", "base1 base2", false),
    ("TIME", "", false),
    ("TOUCH", "clé [clé …]", true),
    ("TTL", "clé", true),
    ("TYPE", "clé", true),
    ("UNLINK", "clé [clé …]", true),
    ("UNWATCH", "", false),
    ("WATCH", "clé [clé …]", true),
    ("XACK", "clé groupe id [id …]", true),
    (
        "XADD",
        "clé [MAXLEN n] *|id champ valeur [champ valeur …]",
        true,
    ),
    ("XDEL", "clé id [id …]", true),
    (
        "XGROUP",
        "CREATE clé groupe id | DESTROY clé groupe …",
        false,
    ),
    (
        "XINFO",
        "STREAM clé | GROUPS clé | CONSUMERS clé groupe",
        false,
    ),
    ("XLEN", "clé", true),
    ("XPENDING", "clé groupe [début fin nombre]", true),
    ("XRANGE", "clé début fin [COUNT n]", true),
    ("XREAD", "[COUNT n] [BLOCK ms] STREAMS clé … id …", false),
    (
        "XREADGROUP",
        "GROUP groupe consommateur [COUNT n] STREAMS clé … id …",
        false,
    ),
    ("XREVRANGE", "clé fin début [COUNT n]", true),
    ("XTRIM", "clé MAXLEN|MINID [~] seuil", true),
    (
        "ZADD",
        "clé [NX|XX] [GT|LT] [CH] [INCR] score membre [score membre …]",
        true,
    ),
    ("ZCARD", "clé", true),
    ("ZCOUNT", "clé min max", true),
    ("ZINCRBY", "clé incrément membre", true),
    ("ZLEXCOUNT", "clé min max", true),
    ("ZMSCORE", "clé membre [membre …]", true),
    ("ZPOPMAX", "clé [nombre]", true),
    ("ZPOPMIN", "clé [nombre]", true),
    ("ZRANDMEMBER", "clé [nombre [WITHSCORES]]", true),
    (
        "ZRANGE",
        "clé début fin [BYSCORE|BYLEX] [REV] [LIMIT o n] [WITHSCORES]",
        true,
    ),
    (
        "ZRANGEBYSCORE",
        "clé min max [WITHSCORES] [LIMIT o n]",
        true,
    ),
    ("ZRANK", "clé membre [WITHSCORE]", true),
    ("ZREM", "clé membre [membre …]", true),
    ("ZREMRANGEBYRANK", "clé début fin", true),
    ("ZREMRANGEBYSCORE", "clé min max", true),
    ("ZREVRANGE", "clé début fin [WITHSCORES]", true),
    ("ZREVRANK", "clé membre [WITHSCORE]", true),
    ("ZSCAN", "clé curseur [MATCH motif] [COUNT n]", true),
    ("ZSCORE", "clé membre", true),
    ("ZUNIONSTORE", "destination nb_clés clé [clé …]", true),
];

/// Suggestions for the token ending at `cursor`: a command name as the first
/// token of the line, a known key (`cx.keys`) as the first argument of a key
/// command, a database index after `SELECT`.
pub fn complete(text: &str, cursor: usize, cx: &CompletionContext) -> Completions {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return Completions::default();
    }
    let line_start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..cursor];
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with("//") {
        return Completions::default();
    }
    // Rough split: completion runs on partial lines (open quotes).
    let words: Vec<&str> = line.split_whitespace().collect();
    let ends_blank = line.is_empty() || line.ends_with(char::is_whitespace);
    let (index, prefix) = if ends_blank {
        (words.len(), "")
    } else {
        (words.len() - 1, *words.last().unwrap())
    };
    let mut items = Vec::new();
    if index == 0 {
        let p = prefix.to_ascii_uppercase();
        for (name, args, _) in COMMANDS {
            if name.starts_with(&p) {
                items.push(Suggestion {
                    label: name.to_string(),
                    detail: args.to_string(),
                    insert: name.to_string(),
                    kind: Kind::Keyword,
                });
            }
        }
        return Completions {
            items,
            missing: Vec::new(),
        };
    }
    let command = words[0].to_ascii_uppercase();
    if index == 1 && command == "SELECT" {
        for db in 0..16 {
            let label = db.to_string();
            if label.starts_with(prefix) {
                items.push(Suggestion {
                    detail: "base".into(),
                    insert: label.clone(),
                    label,
                    kind: Kind::Schema,
                });
            }
        }
    } else if index == 1
        && COMMANDS
            .iter()
            .any(|(name, _, key)| *key && *name == command)
    {
        let bare = prefix.trim_start_matches(['"', '\'']);
        let mut keys: Vec<&String> = cx
            .keys
            .iter()
            .filter(|k| k.starts_with(bare) && k.as_str() != bare)
            .collect();
        keys.sort_by_key(|k| k.to_lowercase());
        keys.dedup();
        for key in keys {
            items.push(Suggestion {
                label: key.clone(),
                detail: "clé".into(),
                insert: quote_arg(key.as_bytes()),
                kind: Kind::Table,
            });
        }
    }
    Completions {
        items,
        missing: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::nosql::NoSqlScope;

    fn run(text: &str, keys: &[&str]) -> Completions {
        let scope = NoSqlScope {
            database: "0".into(),
        };
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        let none = |_: &crate::engine::sql::completion::TableRef| None;
        let cx = CompletionContext {
            scope: &scope,
            schemas: &[],
            columns: &none,
            keys: &keys,
        };
        complete(text, text.len(), &cx)
    }

    fn labels(c: &Completions) -> Vec<&str> {
        c.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn command_names_with_their_arguments() {
        let c = run("hge", &[]);
        assert_eq!(labels(&c), ["HGET", "HGETALL"]);
        assert_eq!(c.items[0].detail, "clé champ");
        assert_eq!(c.items[0].kind, Kind::Keyword);
        // On a later line.
        assert_eq!(labels(&run("GET a\nZPOPM", &[])), ["ZPOPMAX", "ZPOPMIN"]);
        assert!(run("", &[]).items.len() > 100);
        assert!(run("# HG", &[]).items.is_empty());
    }

    #[test]
    fn known_keys_as_first_argument() {
        let keys = ["user:1", "user:2", "session", "a b"];
        assert_eq!(labels(&run("GET user", &keys)), ["user:1", "user:2"]);
        assert_eq!(labels(&run("hgetall ", &keys)).len(), 4);
        let spaced = run("DEL a", &keys);
        assert_eq!(spaced.items[0].insert, "\"a b\"");
        assert_eq!(spaced.items[0].kind, Kind::Table);
        // Not a key command, or past the first argument.
        assert!(run("PING us", &keys).items.is_empty());
        assert!(run("SET k user", &keys).items.is_empty());
    }

    #[test]
    fn databases_after_select() {
        assert_eq!(
            labels(&run("SELECT 1", &[])),
            ["1", "10", "11", "12", "13", "14", "15"]
        );
    }
}
