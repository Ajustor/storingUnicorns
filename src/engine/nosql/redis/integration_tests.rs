//! Integration tests against real Redis servers (plan 13, « Serveurs de
//! test »). Every test is `#[ignore]` and returns early when its variable is
//! missing. Run them with `cargo test -- --ignored integration_redis` and:
//!
//! - `SU_IT_REDIS=redis://127.0.0.1:46379`
//! - `SU_IT_REDIS_AUTH=redis://default:p%40ss%20w@127.0.0.1:46380/0`
//!
//! ```text
//! docker run -d --name su-it-redis -p 46379:6379 redis:8
//! docker run -d --name su-it-redis-auth -p 46380:6379 redis:8 redis-server --requirepass "p@ss w"
//! ```
//!
//! They only use database 15 and keys prefixed `su_it:` (one prefix per
//! test, deleted at its start; never a FLUSH), so they can run in parallel
//! and repeatedly.

use super::*;
use crate::engine::models::{ConnectionConfig, DatabaseType};
use crate::engine::nosql::{ImportOptions, TransferFormat};
use crate::engine::presets::parse_url;

const DB: u8 = 15;

fn config(var: &str) -> Option<ConnectionConfig> {
    let url = std::env::var(var).ok()?;
    let p = parse_url(&url).unwrap();
    Some(ConnectionConfig {
        db_type: DatabaseType::Redis,
        host: p.host,
        port: p.port,
        username: p.username,
        password: p.password,
        database: DB.to_string(),
        ssl_mode: p.ssl_mode,
        ..Default::default()
    })
}

/// A connection to `SU_IT_REDIS`, with the keys of `prefix` deleted.
async fn setup(prefix: &str) -> Option<RedisClient> {
    let c = connect(&config("SU_IT_REDIS")?).await.unwrap();
    assert!(prefix.starts_with("su_it:"));
    for key in scan_all(&c, DB, &format!("{prefix}*")).await.unwrap() {
        apply_edits(&c, DB, &key, &[KeyEdit::Delete]).await.unwrap();
    }
    Some(c)
}

fn k(prefix: &str, name: &str) -> Vec<u8> {
    format!("{prefix}{name}").into_bytes()
}

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// One value of each type (binary included).
fn samples() -> Vec<(&'static str, RedisValue)> {
    vec![
        ("str", RedisValue::String(vec![0xff, 0x00, b'a'])),
        ("list", RedisValue::List(vec![b("a"), b("b"), b("a")])),
        ("set", RedisValue::Set(vec![b("x")])),
        (
            "zset",
            RedisValue::ZSet(vec![(b("m"), 1.5), (b("n"), f64::INFINITY)]),
        ),
        (
            "hash",
            RedisValue::Hash(vec![(b("f"), b("v")), (vec![0xfe], vec![0xfd])]),
        ),
        (
            "stream",
            RedisValue::Stream(vec![StreamEntry {
                id: "1-1".into(),
                fields: vec![(b("a"), b("1"))],
            }]),
        ),
    ]
}

async fn value(c: &RedisClient, key: &[u8]) -> RedisValue {
    read_all(c, DB, key).await.unwrap().unwrap()
}

fn sorted(v: RedisValue) -> RedisValue {
    match v {
        RedisValue::Set(mut i) => {
            i.sort();
            RedisValue::Set(i)
        }
        RedisValue::Hash(mut p) => {
            p.sort();
            RedisValue::Hash(p)
        }
        other => other,
    }
}

#[tokio::test]
#[ignore]
async fn integration_redis_databases_and_scan() {
    let p = "su_it:scan:";
    let Some(c) = setup(p).await else { return };
    let dbs = databases(&c).await.unwrap();
    assert!(dbs.len() >= 16);
    assert!(dbs.iter().any(|s| s.name == "db15" && s.tables.is_empty()));
    for (name, v) in samples() {
        let ttl = (name == "list").then_some(600);
        create_key(&c, DB, &k(p, name), &v, ttl).await.unwrap();
    }
    assert!(key_counts(&c).await.unwrap()[&DB] >= 6);
    let mut found = Vec::new();
    let mut cursor = 0;
    loop {
        let page = scan(&c, DB, &format!("{p}*"), None, cursor, 2)
            .await
            .unwrap();
        found.extend(page.keys);
        if page.cursor == 0 {
            break;
        }
        cursor = page.cursor;
    }
    found.sort_by(|a, b| a.key.cmp(&b.key));
    found.dedup_by(|a, b| a.key == b.key);
    let summary: Vec<(String, RedisType, bool, Option<u64>)> = found
        .iter()
        .map(|i| {
            (
                display_bytes(&i.key),
                i.kind.clone(),
                i.ttl.is_some(),
                i.size,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (format!("{p}hash"), RedisType::Hash, false, Some(2)),
            (format!("{p}list"), RedisType::List, true, Some(3)),
            (format!("{p}set"), RedisType::Set, false, Some(1)),
            (format!("{p}str"), RedisType::String, false, Some(3)),
            (format!("{p}stream"), RedisType::Stream, false, Some(1)),
            (format!("{p}zset"), RedisType::ZSet, false, Some(2)),
        ]
    );
    let ttl = found[1].ttl.unwrap();
    assert!(ttl > 590 && ttl <= 600, "{ttl}");
    // Filtered by type.
    let mut hashes = Vec::new();
    let mut cursor = 0;
    loop {
        let page = scan(
            &c,
            DB,
            &format!("{p}*"),
            Some(&RedisType::Hash),
            cursor,
            100,
        )
        .await
        .unwrap();
        hashes.extend(page.keys);
        if page.cursor == 0 {
            break;
        }
        cursor = page.cursor;
    }
    assert_eq!(hashes.len(), 1);
    assert_eq!(key_info(&c, DB, &k(p, "missing")).await.unwrap(), None);
    assert_eq!(
        key_info(&c, DB, &k(p, "zset")).await.unwrap().unwrap().kind,
        RedisType::ZSet
    );
}

#[tokio::test]
#[ignore]
async fn integration_redis_paged_reads_of_each_type() {
    let p = "su_it:read:";
    let Some(c) = setup(p).await else { return };
    let n = 25;
    let items: Vec<Vec<u8>> = (0..n).map(|i| format!("e{i:02}").into_bytes()).collect();
    let values = [
        RedisValue::List(items.clone()),
        RedisValue::Set(items.clone()),
        RedisValue::ZSet(
            items
                .iter()
                .enumerate()
                .map(|(i, m)| (m.clone(), i as f64))
                .collect(),
        ),
        RedisValue::Hash(items.iter().map(|m| (m.clone(), m.clone())).collect()),
        RedisValue::Stream(
            (0..n)
                .map(|i| StreamEntry {
                    id: format!("{}-0", i + 1),
                    fields: vec![(b("i"), i.to_string().into_bytes())],
                })
                .collect(),
        ),
        RedisValue::String(b("plain")),
    ];
    for v in &values {
        let key = k(p, v.kind().name());
        create_key(&c, DB, &key, v, None).await.unwrap();
        // Pages of 10.
        let mut from = 0;
        let mut pages = 0;
        let mut all = 0;
        loop {
            let page = read_value(&c, DB, &key, from, 10).await.unwrap();
            assert_eq!(page.value.kind(), v.kind());
            pages += 1;
            all += page.value.len();
            if matches!(
                v,
                RedisValue::List(_) | RedisValue::ZSet(_) | RedisValue::Stream(_)
            ) {
                assert_eq!(page.total, n as u64);
                assert_eq!(page_offset(&page), from);
            }
            match page.next {
                Some(next) => from = next,
                None => break,
            }
            assert!(pages < 100);
        }
        match v {
            RedisValue::String(s) => assert_eq!(all, s.len()),
            // SSCAN / HSCAN may repeat; small values come in one page.
            _ => assert!(all >= n, "{:?}", v.kind()),
        }
        if matches!(
            v,
            RedisValue::List(_) | RedisValue::ZSet(_) | RedisValue::Stream(_)
        ) {
            assert_eq!(pages, 3, "{:?}", v.kind());
        }
        assert_eq!(sorted(value(&c, &key).await), sorted(v.clone()));
    }
    assert!(read_value(&c, DB, &k(p, "missing"), 0, 10).await.is_err());
}

#[tokio::test]
#[ignore]
async fn integration_redis_each_key_edit() {
    let p = "su_it:edit:";
    let Some(c) = setup(p).await else { return };
    let apply = |key: Vec<u8>, edits: Vec<KeyEdit>| {
        let c = &c;
        async move { apply_edits(c, DB, &key, &edits).await }
    };
    // String, keeping the TTL.
    let s = k(p, "s");
    create_key(&c, DB, &s, &RedisValue::String(b("a")), Some(100))
        .await
        .unwrap();
    assert!(create_key(&c, DB, &s, &RedisValue::String(b("b")), None)
        .await
        .unwrap_err()
        .to_string()
        .contains("existe déjà"));
    apply(s.clone(), vec![KeyEdit::SetString(vec![0xff])])
        .await
        .unwrap();
    assert_eq!(value(&c, &s).await, RedisValue::String(vec![0xff]));
    assert!(key_info(&c, DB, &s).await.unwrap().unwrap().ttl.is_some());
    apply(s.clone(), vec![KeyEdit::Expire(None)]).await.unwrap();
    assert_eq!(key_info(&c, DB, &s).await.unwrap().unwrap().ttl, None);
    apply(s.clone(), vec![KeyEdit::Expire(Some(50))])
        .await
        .unwrap();
    assert!(key_info(&c, DB, &s).await.unwrap().unwrap().ttl.is_some());
    // List: set, push both ends, remove by index among duplicates.
    let l = k(p, "l");
    create_key(
        &c,
        DB,
        &l,
        &RedisValue::List(vec![b("a"), b("x"), b("a")]),
        None,
    )
    .await
    .unwrap();
    let n = apply(
        l.clone(),
        vec![
            KeyEdit::ListSet {
                index: 1,
                value: b("y"),
            },
            KeyEdit::ListRemove { index: 2 },
            KeyEdit::ListPush {
                left: true,
                value: b("first"),
            },
            KeyEdit::ListPush {
                left: false,
                value: b("last"),
            },
        ],
    )
    .await
    .unwrap();
    assert_eq!(n, 4);
    assert_eq!(
        value(&c, &l).await,
        RedisValue::List(vec![b("first"), b("a"), b("y"), b("last")])
    );
    // Out of range: error with the count applied.
    let err = apply(
        l.clone(),
        vec![
            KeyEdit::ListPush {
                left: false,
                value: b("z"),
            },
            KeyEdit::ListRemove { index: 99 },
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.starts_with("1 appliquée(s), échec"), "{err}");
    // Set, zset, hash, stream.
    let st = k(p, "set");
    create_key(&c, DB, &st, &RedisValue::Set(vec![b("a")]), None)
        .await
        .unwrap();
    apply(
        st.clone(),
        vec![KeyEdit::SetAdd(b("b")), KeyEdit::SetRemove(b("a"))],
    )
    .await
    .unwrap();
    assert_eq!(value(&c, &st).await, RedisValue::Set(vec![b("b")]));
    let z = k(p, "z");
    create_key(&c, DB, &z, &RedisValue::ZSet(vec![(b("a"), 1.0)]), None)
        .await
        .unwrap();
    apply(
        z.clone(),
        vec![
            KeyEdit::ZAdd {
                member: b("b"),
                score: f64::NEG_INFINITY,
            },
            KeyEdit::ZAdd {
                member: b("a"),
                score: 3.25,
            },
            KeyEdit::ZRemove(b("nope")),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        value(&c, &z).await,
        RedisValue::ZSet(vec![(b("b"), f64::NEG_INFINITY), (b("a"), 3.25)])
    );
    let h = k(p, "h");
    create_key(&c, DB, &h, &RedisValue::Hash(vec![(b("f"), b("1"))]), None)
        .await
        .unwrap();
    apply(
        h.clone(),
        vec![
            KeyEdit::HSet {
                field: b("g"),
                value: b("2"),
            },
            KeyEdit::HDel(b("f")),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        value(&c, &h).await,
        RedisValue::Hash(vec![(b("g"), b("2"))])
    );
    let x = k(p, "x");
    create_key(
        &c,
        DB,
        &x,
        &RedisValue::Stream(vec![StreamEntry {
            id: "5-0".into(),
            fields: vec![(b("a"), b("1"))],
        }]),
        None,
    )
    .await
    .unwrap();
    apply(
        x.clone(),
        vec![
            KeyEdit::XAdd {
                id: Some("6-0".into()),
                fields: vec![(b("b"), b("2"))],
            },
            KeyEdit::XAdd {
                id: None,
                fields: vec![(b("c"), b("3"))],
            },
            KeyEdit::XDel("5-0".into()),
        ],
    )
    .await
    .unwrap();
    let RedisValue::Stream(entries) = value(&c, &x).await else {
        panic!()
    };
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, "6-0");
    assert!(apply(x.clone(), vec![KeyEdit::XDel("5-0".into())])
        .await
        .is_err());
    // Rename (refused onto an existing key), then edits follow the new name.
    assert!(apply(h.clone(), vec![KeyEdit::Rename { to: z.clone() }])
        .await
        .is_err());
    let h2 = k(p, "h2");
    apply(
        h.clone(),
        vec![
            KeyEdit::Rename { to: h2.clone() },
            KeyEdit::HSet {
                field: b("n"),
                value: b("v"),
            },
        ],
    )
    .await
    .unwrap();
    assert_eq!(key_info(&c, DB, &h).await.unwrap(), None);
    assert_eq!(key_info(&c, DB, &h2).await.unwrap().unwrap().size, Some(2));
    apply(h2.clone(), vec![KeyEdit::Delete]).await.unwrap();
    assert_eq!(key_info(&c, DB, &h2).await.unwrap(), None);
    // Grid edits on a page, applied.
    let gl = k(p, "grid");
    create_key(
        &c,
        DB,
        &gl,
        &RedisValue::List(vec![b("a"), b("b"), b("a")]),
        None,
    )
    .await
    .unwrap();
    let info = key_info(&c, DB, &gl).await.unwrap().unwrap();
    let page = read_value(&c, DB, &gl, 0, 100).await.unwrap();
    let rows = page_result(&page).rows;
    let changes = RowChanges {
        updates: vec![(rows[1].clone(), vec!["1".into(), "B".into()])],
        deletes: vec![rows[2].clone()],
        inserts: vec![vec![None, Some("c".into())]],
    };
    let edits = grid_edits(&info, &page, &changes).unwrap();
    apply(gl.clone(), edits).await.unwrap();
    assert_eq!(
        value(&c, &gl).await,
        RedisValue::List(vec![b("a"), b("B"), b("c")])
    );
}

#[tokio::test]
#[ignore]
async fn integration_redis_console() {
    let p = "su_it:con:";
    let Some(c) = setup(p).await else { return };
    let mut scope = NoSqlScope {
        database: String::new(),
    };
    let script = format!(
        "SELECT 15\n\
         SET {p}a 1\n\
         # comment\n\
         RPUSH {p}l x y z\n\
         KEYS {p}*\n\
         MULTI\n\
         INCR {p}a\n\
         INCR {p}a\n\
         EXEC\n\
         GET {p}a\n\
         LRANGE {p}l 1 2\n\
         HGETALL {p}missing"
    );
    let out = run_script(&c, &script, None, &mut scope, 1000).await;
    assert_eq!(out.len(), 11, "{out:?}");
    for o in &out {
        assert!(
            o.outcome.result.is_ok(),
            "{}: {:?}",
            o.outcome.sql,
            o.outcome.result
        );
    }
    assert_eq!(scope.database, "15");
    let result = |i: usize| out[i].outcome.result.as_ref().unwrap();
    assert_eq!(result(0).rows, [["OK"]]);
    let keys = result(3);
    assert!(keys.columns[0].name.contains("SCAN"));
    assert_eq!(keys.rows, [[format!("{p}a")], [format!("{p}l")]]);
    assert_eq!(result(4).rows, [["OK"]]);
    assert_eq!(result(5).rows, [["QUEUED"]]);
    assert_eq!(result(7).rows, [["1", "2"], ["2", "3"]]);
    assert_eq!(result(8).rows, [["3"]]);
    assert_eq!(
        out[8].source,
        Some(Source::Key {
            db: DB,
            key: k(p, "a"),
            kind: RedisType::String
        })
    );
    // LRANGE from 1: indexes as LSET takes them.
    assert_eq!(result(9).rows, [["1", "y"], ["2", "z"]]);
    assert!(matches!(
        out[9].source,
        Some(Source::Key {
            kind: RedisType::List,
            ..
        })
    ));
    // KEYS truncated at max_rows.
    let out = run_script(&c, &format!("KEYS {p}*"), None, &mut scope, 1).await;
    let r = out[0].outcome.result.as_ref().unwrap();
    assert_eq!(r.rows.len(), 1);
    assert!(r.truncated);
    // The first error stops the script.
    let out = run_script(
        &c,
        &format!("GET {p}a\nHGET {p}a f\nPING"),
        None,
        &mut scope,
        1000,
    )
    .await;
    assert_eq!(out.len(), 2);
    assert!(out[1]
        .outcome
        .result
        .as_ref()
        .unwrap_err()
        .contains("WRONGTYPE"));
    // Refused, and only the selected unit runs.
    let text = format!("BLPOP {p}l 0\nINCR {p}a");
    let out = run_script(&c, &text, Some(0..1), &mut scope, 1000).await;
    assert_eq!(out.len(), 1);
    assert!(out[0].outcome.result.is_err());
    let out = run_script(&c, &text, Some(text.len()..text.len()), &mut scope, 1000).await;
    assert_eq!(out[0].outcome.result.as_ref().unwrap().rows, [["4"]]);
    // A blocking read with a timeout returns.
    let out = run_script(&c, &format!("BLPOP {p}empty 0.1"), None, &mut scope, 1000).await;
    assert!(out[0].outcome.result.is_ok());
}

#[tokio::test]
#[ignore]
async fn integration_redis_export_import() {
    let p = "su_it:xfer:";
    let Some(c) = setup(p).await else { return };
    let mut keys: Vec<(Vec<u8>, RedisValue)> = samples()
        .into_iter()
        .map(|(name, v)| (k(p, name), v))
        .collect();
    let mut binary = k(p, "bin");
    binary.push(0xff);
    keys.push((binary, RedisValue::String(b("\"quoted\"\nline"))));
    for (i, (key, v)) in keys.iter().enumerate() {
        let ttl = (i == 0).then_some(1000);
        create_key(&c, DB, key, v, ttl).await.unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    for format in TransferFormat::ALL {
        let path = dir.path().join(format!("keys.{}", format.extension()));
        let mut calls = 0;
        let n = export_keys(&c, DB, &format!("{p}*"), &path, format, |_, _, _| {
            calls += 1
        })
        .await
        .unwrap();
        assert_eq!(n, keys.len());
        assert!(calls > keys.len());
        for (key, _) in &keys {
            apply_edits(&c, DB, key, &[KeyEdit::Delete]).await.unwrap();
        }
        let stats = import_keys(
            &c,
            DB,
            &path,
            format,
            &ImportOptions::default(),
            |_, _, _| {},
        )
        .await
        .unwrap();
        assert_eq!(stats.errors, Vec::<String>::new(), "{format:?}");
        assert_eq!(stats.inserted, keys.len());
        for (key, v) in &keys {
            assert_eq!(
                sorted(value(&c, key).await),
                sorted(v.clone()),
                "{format:?}"
            );
        }
        assert!(key_info(&c, DB, &keys[0].0)
            .await
            .unwrap()
            .unwrap()
            .ttl
            .is_some());
        // Existing keys: refused, unless overwritten.
        let stats = import_keys(
            &c,
            DB,
            &path,
            format,
            &ImportOptions::default(),
            |_, _, _| {},
        )
        .await
        .unwrap();
        assert_eq!(stats.errors.len(), keys.len());
        assert_eq!(stats.succeeded(), 0);
        apply_edits(
            &c,
            DB,
            &keys[1].0,
            &[KeyEdit::ListPush {
                left: false,
                value: b("extra"),
            }],
        )
        .await
        .unwrap();
        let upsert = ImportOptions {
            upsert: true,
            ..Default::default()
        };
        let stats = import_keys(&c, DB, &path, format, &upsert, |_, _, _| {})
            .await
            .unwrap();
        assert_eq!(stats.updated, keys.len());
        assert_eq!(value(&c, &keys[1].0).await, keys[1].1);
    }
}

#[tokio::test]
#[ignore]
async fn integration_redis_auth() {
    let Some(config) = config("SU_IT_REDIS_AUTH") else {
        return;
    };
    let c = connect(&config).await.unwrap();
    test(&c).await.unwrap();
    let mut scope = NoSqlScope::initial(&config);
    let out = run_script(&c, "PING", None, &mut scope, 10).await;
    assert_eq!(out[0].outcome.result.as_ref().unwrap().rows, [["PONG"]]);
    let wrong = ConnectionConfig {
        password: Some("wrong".into()),
        ..config.clone()
    };
    let err = connect(&wrong).await.err().unwrap().to_string();
    assert!(err.contains("authentification"), "{err}");
    let none = ConnectionConfig {
        password: None,
        username: None,
        ..config
    };
    let err = connect(&none).await.err().unwrap().to_string();
    assert!(err.contains("authentification"), "{err}");
}
