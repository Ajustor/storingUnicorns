//! Redis engine (plan 13): connection, key browsing, values and their
//! edits, console and JSON / CSV transfer. (The `redis` crate is `::redis`
//! here.)

pub mod cli;
mod complete;
#[cfg(test)]
mod integration_tests;
mod keys;
mod transfer;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::time::{Duration, Instant};

use ::redis::aio::{
    ConnectionLike, ConnectionManager, ConnectionManagerConfig, MultiplexedConnection,
};
use ::redis::{
    AsyncConnectionConfig, Cmd, ConnectionAddr, IntoConnectionInfo, RedisConnectionInfo,
    RedisError, TlsCertificates, Value,
};
use anyhow::{anyhow, bail, Result};
use tokio::sync::Mutex;

use self::cli::{classify, quote_arg, reply_to_result, tokenize, Safety};
pub use self::complete::{complete, replace_start};
// Used by the GUI and the TUI (plan 13, Tasks 4 and 5).
#[allow(unused_imports)]
pub use self::keys::key_tree;
#[allow(unused_imports)]
pub use self::transfer::{export_keys, import_keys, value_from_json, value_to_json};
use super::{NoSqlOutcome, NoSqlScope, Source, Warning};
use crate::engine::models::{is_null, Column, ConnectionConfig, QueryResult, SchemaInfo};
use crate::engine::nosql::is_absent;
use crate::engine::ops::query::StatementOutcome;
use crate::engine::ops::rows::RowChanges;

const ENGINE: &str = "Redis";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
/// SCAN page size of the whole-database walks (export, KEYS).
const SCAN_COUNT: usize = 1000;
/// Elements per command when writing a value (RPUSH, SADD…).
const WRITE_CHUNK: usize = 1000;

/// An open Redis connection: the client, and one auto-reconnecting manager
/// per database browsed.
pub struct RedisClient {
    /// Boxed: `ConnectionInfo` would make every `DatabaseConnection` large.
    pub(crate) client: Box<::redis::Client>,
    pub(crate) managers: Mutex<HashMap<u8, ::redis::aio::ConnectionManager>>,
    /// Database of the connection settings.
    pub(crate) default_db: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedisType {
    String,
    List,
    Set,
    ZSet,
    Hash,
    Stream,
    /// A module type (`ReJSON-RL`…): read only.
    Other(String),
}

impl RedisType {
    /// The name `TYPE` answers (`string`, `zset`…).
    pub fn name(&self) -> &str {
        match self {
            Self::String => "string",
            Self::List => "list",
            Self::Set => "set",
            Self::ZSet => "zset",
            Self::Hash => "hash",
            Self::Stream => "stream",
            Self::Other(name) => name,
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "string" => Self::String,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::ZSet,
            "hash" => Self::Hash,
            "stream" => Self::Stream,
            _ => Self::Other(name.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyInfo {
    pub key: Vec<u8>,
    pub kind: RedisType,
    /// Seconds to live; `None` = persistent.
    pub ttl: Option<i64>,
    /// Length / cardinality / bytes, when known.
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanPage {
    pub keys: Vec<KeyInfo>,
    /// Next SCAN cursor; 0 = done.
    pub cursor: u64,
}

// ---------------------------------------------------------------- bytes

/// Bytes as text: UTF-8 when valid, else escaped like redis-cli (`"\xff"`).
/// Text holding control characters, or starting with `"`, is quoted too, so
/// `parse_display` reads every output back.
pub fn display_bytes(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) if !s.starts_with('"') && !s.chars().any(char::is_control) => s.to_string(),
        _ => quoted(b),
    }
}

/// `"…"` with redis-cli escapes; valid UTF-8 text stays readable.
pub(crate) fn quoted(b: &[u8]) -> String {
    let mut out = String::from('"');
    for chunk in b.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{7}' => out.push_str("\\a"),
                '\u{8}' => out.push_str("\\b"),
                c if c.is_control() => {
                    let mut buf = [0; 4];
                    for byte in c.encode_utf8(&mut buf).bytes() {
                        out.push_str(&format!("\\x{byte:02x}"));
                    }
                }
                c => out.push(c),
            }
        }
        for byte in chunk.invalid() {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out.push('"');
    out
}

/// Inverse of `display_bytes`: a `"…"` text is unescaped, anything else is
/// taken as is.
pub fn parse_display(s: &str) -> Result<Vec<u8>, String> {
    if !s.starts_with('"') {
        return Ok(s.as_bytes().to_vec());
    }
    let tokens = tokenize(s)?;
    match tokens.as_slice() {
        [one] if s.trim_end().len() == s.len() => Ok(one.clone()),
        _ => Err(format!(
            "Texte entre guillemets attendu (\"…\" avec \\xHH) : {s}"
        )),
    }
}

/// A score as typed (`1.5`, `inf`, `-inf`).
pub(crate) fn parse_score(s: &str) -> Result<f64, String> {
    let t = s.trim();
    match t.to_ascii_lowercase().as_str() {
        "inf" | "+inf" => Ok(f64::INFINITY),
        "-inf" => Ok(f64::NEG_INFINITY),
        _ => t
            .parse::<f64>()
            .ok()
            .filter(|f| !f.is_nan())
            .ok_or_else(|| format!("Score invalide : {s}")),
    }
}

// ---------------------------------------------------------- connection

/// Database index of a setting / scope ("" = `default`).
fn parse_db(text: &str, default: u8) -> Result<u8> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(default);
    }
    t.parse()
        .map_err(|_| anyhow!("{ENGINE} : base invalide « {t} » (0 à 15 attendu)"))
}

/// The index of a `SchemaInfo` name ("db3" → 3).
pub fn db_index(name: &str) -> Option<u8> {
    name.strip_prefix("db")?.parse().ok()
}

/// A CA file plus the system roots, as PEM.
fn ca_bundle(path: &std::path::Path) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let mut pem = std::fs::read(path).map_err(|e| {
        anyhow!(
            "{ENGINE} : certificat CA illisible ({}) : {e}",
            path.display()
        )
    })?;
    pem.push(b'\n');
    for cert in rustls_native_certs::load_native_certs().certs {
        let b64 = base64::engine::general_purpose::STANDARD.encode(cert.as_ref());
        pem.extend_from_slice(b"-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.extend_from_slice(line);
            pem.push(b'\n');
        }
        pem.extend_from_slice(b"-----END CERTIFICATE-----\n");
    }
    Ok(pem)
}

/// A French error for a failed connection or command.
fn friendly(c: &::redis::Client, e: RedisError) -> anyhow::Error {
    let addr = c.get_connection_info().addr().to_string();
    let text = e.to_string();
    let lower = text.to_lowercase();
    if e.kind() == ::redis::ErrorKind::AuthenticationFailed
        || lower.contains("wrongpass")
        || matches!(e.code(), Some("NOAUTH" | "WRONGPASS"))
        || lower.contains("noauth")
        || lower.contains("authentication")
        || lower.contains("invalid password")
    {
        anyhow!("{ENGINE} : authentification refusée sur {addr} ({text})")
    } else if e.is_connection_refusal() {
        anyhow!("{ENGINE} : connexion refusée par {addr} ({text})")
    } else if e.is_timeout() {
        anyhow!("{ENGINE} : délai dépassé pour {addr} ({text})")
    } else if lower.contains("certificate") || lower.contains("tls") || lower.contains("handshake")
    {
        anyhow!("{ENGINE} : échec TLS avec {addr} ({text})")
    } else if e.is_io_error() {
        anyhow!("{ENGINE} : connexion impossible à {addr} ({text})")
    } else {
        anyhow!("{ENGINE} : {text}")
    }
}

/// PING, AUTH (optional ACL user), TLS.
pub async fn connect(config: &ConnectionConfig) -> Result<RedisClient> {
    let host = config
        .host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = config.port.unwrap_or(6379);
    let db = parse_db(&config.database, 0)?;
    let mut settings = RedisConnectionInfo::default().set_db(i64::from(db));
    if let Some(user) = config.username.as_deref().filter(|u| !u.is_empty()) {
        settings = settings.set_username(user);
    }
    if let Some(password) = config.password.as_deref().filter(|p| !p.is_empty()) {
        settings = settings.set_password(password);
    }
    let tls = config.nosql_tls();
    let addr = match tls {
        None => ConnectionAddr::Tcp(host, port),
        Some(verify) => ConnectionAddr::TcpTls {
            host,
            port,
            insecure: !verify,
            tls_params: None,
        },
    };
    let info = addr
        .into_connection_info()
        .map_err(|e| anyhow!("{ENGINE} : {e}"))?
        .set_redis_settings(settings);
    let client = match (tls, &config.ssl_ca) {
        (Some(true), Some(ca)) => ::redis::Client::build_with_tls(
            info,
            TlsCertificates {
                client_tls: None,
                root_cert: Some(ca_bundle(ca)?),
            },
        ),
        _ => ::redis::Client::open(info),
    }
    .map_err(|e| anyhow!("{ENGINE} : paramètres invalides ({e})"))?;
    let c = RedisClient {
        client: Box::new(client),
        managers: Mutex::new(HashMap::new()),
        default_db: db,
    };
    test(&c).await?;
    Ok(c)
}

/// The client of database `db` (same server, credentials and TLS).
fn client_for(c: &RedisClient, db: u8) -> Result<::redis::Client> {
    let info = c.client.get_connection_info().clone();
    let settings = info.redis_settings().clone().set_db(i64::from(db));
    ::redis::Client::open(info.set_redis_settings(settings)).map_err(|e| anyhow!("{ENGINE} : {e}"))
}

/// The shared, auto-reconnecting connection of database `db`.
async fn manager(c: &RedisClient, db: u8) -> Result<ConnectionManager> {
    let mut managers = c.managers.lock().await;
    if let Some(m) = managers.get(&db) {
        return Ok(m.clone());
    }
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(Some(RESPONSE_TIMEOUT))
        .set_number_of_retries(2);
    let m = client_for(c, db)?
        .get_connection_manager_with_config(config)
        .await
        .map_err(|e| friendly(&c.client, e))?;
    managers.insert(db, m.clone());
    Ok(m)
}

/// A dedicated connection (console: MULTI, SELECT and WATCH stay in it); no
/// response timeout, blocking commands wait for their own.
async fn dedicated(c: &RedisClient, db: u8) -> Result<MultiplexedConnection> {
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(None);
    client_for(c, db)?
        .get_multiplexed_async_connection_with_config(&config)
        .await
        .map_err(|e| friendly(&c.client, e))
}

/// A command from its arguments (the name first).
fn command(argv: &[Vec<u8>]) -> Cmd {
    let mut cmd = Cmd::new();
    for a in argv {
        cmd.arg(a.as_slice());
    }
    cmd
}

/// Run one command on `con`; server errors are errors.
async fn send(con: &mut impl ConnectionLike, c: &RedisClient, cmd: &Cmd) -> Result<Value> {
    match con.req_packed_command(cmd).await {
        Ok(Value::ServerError(e)) => Err(anyhow!("{ENGINE} : {e}")),
        Ok(v) => Ok(v),
        Err(e) => Err(friendly(&c.client, e)),
    }
}

async fn send_argv(
    con: &mut impl ConnectionLike,
    c: &RedisClient,
    argv: &[Vec<u8>],
) -> Result<Value> {
    send(con, c, &command(argv)).await
}

fn args(parts: &[&[u8]]) -> Vec<Vec<u8>> {
    parts.iter().map(|p| p.to_vec()).collect()
}

pub async fn test(c: &RedisClient) -> Result<()> {
    let mut con = manager(c, c.default_db).await?;
    send_argv(&mut con, c, &args(&[b"PING"])).await?;
    Ok(())
}

// --------------------------------------------------------- reply values

/// Bytes of a scalar reply.
fn bytes_of(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::BulkString(b) => Some(b.clone()),
        Value::SimpleString(s) => Some(s.as_bytes().to_vec()),
        Value::VerbatimString { text, .. } => Some(text.as_bytes().to_vec()),
        Value::Int(i) => Some(i.to_string().into_bytes()),
        Value::Double(d) => Some(cli::format_score(*d).into_bytes()),
        Value::Okay => Some(b"OK".to_vec()),
        _ => None,
    }
}

fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        other => std::str::from_utf8(&bytes_of(other)?).ok()?.parse().ok(),
    }
}

fn list_of(v: &Value) -> Option<&[Value]> {
    match v {
        Value::Array(i) | Value::Set(i) => Some(i),
        _ => None,
    }
}

fn bytes_list(v: &Value) -> Option<Vec<Vec<u8>>> {
    list_of(v)?.iter().map(bytes_of).collect()
}

/// `[k, v, k, v]`, `[[k, v], …]` or a map, as byte pairs.
fn byte_pairs(v: &Value) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    if let Value::Map(m) = v {
        return m
            .iter()
            .map(|(k, v)| Some((bytes_of(k)?, bytes_of(v)?)))
            .collect();
    }
    let items = list_of(v)?;
    if !items.is_empty()
        && items
            .iter()
            .all(|i| list_of(i).is_some_and(|p| p.len() == 2))
    {
        return items
            .iter()
            .map(|i| {
                let p = list_of(i)?;
                Some((bytes_of(&p[0])?, bytes_of(&p[1])?))
            })
            .collect();
    }
    if items.len() % 2 != 0 {
        return None;
    }
    items
        .chunks(2)
        .map(|c| Some((bytes_of(&c[0])?, bytes_of(&c[1])?)))
        .collect()
}

fn scored(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Option<Vec<(Vec<u8>, f64)>> {
    pairs
        .into_iter()
        .map(|(m, s)| Some((m, parse_score(std::str::from_utf8(&s).ok()?).ok()?)))
        .collect()
}

fn stream_entries(v: &Value) -> Option<Vec<StreamEntry>> {
    list_of(v)?
        .iter()
        .map(|e| {
            let [id, fields] = list_of(e)? else {
                return None;
            };
            Some(StreamEntry {
                id: String::from_utf8(bytes_of(id)?).ok()?,
                fields: byte_pairs(fields)?,
            })
        })
        .collect()
}

/// `[cursor, elements]` of a SCAN-family reply.
fn scan_reply(v: &Value) -> Option<(u64, &Value)> {
    let [cursor, elements] = list_of(v)? else {
        return None;
    };
    Some((u64::try_from(int_of(cursor)?).ok()?, elements))
}

// ------------------------------------------------------------- browsing

/// One `SchemaInfo` per database ("db0"…; `tables` empty): `CONFIG GET
/// databases` (16 when refused); the configured database is always listed.
/// (`key_counts` gives the `INFO keyspace` counts.)
pub async fn databases(c: &RedisClient) -> Result<Vec<SchemaInfo>> {
    let mut con = manager(c, c.default_db).await?;
    let count = send_argv(&mut con, c, &args(&[b"CONFIG", b"GET", b"databases"]))
        .await
        .ok()
        .and_then(|v| byte_pairs(&v))
        .and_then(|p| {
            p.first()
                .and_then(|(_, n)| std::str::from_utf8(n).ok()?.parse::<u16>().ok())
        })
        .unwrap_or(16)
        .clamp(1, 256);
    let mut dbs: Vec<u16> = (0..count).collect();
    if u16::from(c.default_db) >= count {
        dbs.push(u16::from(c.default_db));
    }
    Ok(dbs
        .into_iter()
        .map(|i| SchemaInfo {
            name: format!("db{i}"),
            tables: Vec::new(),
            expanded: false,
        })
        .collect())
}

/// Keys per database (`INFO keyspace`); empty databases are absent.
pub async fn key_counts(c: &RedisClient) -> Result<BTreeMap<u8, u64>> {
    let mut con = manager(c, c.default_db).await?;
    let info = send_argv(&mut con, c, &args(&[b"INFO", b"keyspace"])).await?;
    Ok(parse_keyspace(&String::from_utf8_lossy(
        &bytes_of(&info).unwrap_or_default(),
    )))
}

/// `db0:keys=12,expires=0,avg_ttl=0` lines.
fn parse_keyspace(text: &str) -> BTreeMap<u8, u64> {
    text.lines()
        .filter_map(|line| {
            let (db, rest) = line.trim().split_once(':')?;
            let keys = rest
                .split(',')
                .find_map(|kv| kv.strip_prefix("keys="))?
                .parse()
                .ok()?;
            Some((db_index(db)?, keys))
        })
        .collect()
}

/// Type, TTL and size of `keys` (two pipelines); vanished keys are dropped.
async fn infos(
    con: &mut ConnectionManager,
    c: &RedisClient,
    keys: Vec<Vec<u8>>,
) -> Result<Vec<KeyInfo>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut p = ::redis::pipe();
    for k in &keys {
        p.cmd("TYPE").arg(k.as_slice()).cmd("TTL").arg(k.as_slice());
    }
    let replies: Vec<Value> = p
        .query_async(con)
        .await
        .map_err(|e| friendly(&c.client, e))?;
    let mut found = Vec::new();
    for (key, pair) in keys.into_iter().zip(replies.chunks(2)) {
        let kind = bytes_of(&pair[0]).map(|t| String::from_utf8_lossy(&t).into_owned());
        let ttl = pair.get(1).and_then(int_of).unwrap_or(-1);
        match kind.as_deref() {
            None | Some("none") => continue,
            Some(kind) if ttl != -2 => found.push(KeyInfo {
                key,
                kind: RedisType::from_name(kind),
                ttl: (ttl >= 0).then_some(ttl),
                size: None,
            }),
            _ => continue,
        }
    }
    let sized: Vec<usize> = (0..found.len())
        .filter(|&i| !matches!(found[i].kind, RedisType::Other(_)))
        .collect();
    if !sized.is_empty() {
        let mut p = ::redis::pipe();
        for &i in &sized {
            let name = match found[i].kind {
                RedisType::String => "STRLEN",
                RedisType::List => "LLEN",
                RedisType::Set => "SCARD",
                RedisType::ZSet => "ZCARD",
                RedisType::Hash => "HLEN",
                _ => "XLEN",
            };
            p.cmd(name).arg(found[i].key.as_slice());
        }
        // A size is a nicety: a module or ACL refusal leaves it unknown.
        if let Ok(sizes) = p.query_async::<Vec<Value>>(con).await {
            for (&i, size) in sized.iter().zip(&sizes) {
                found[i].size = int_of(size).and_then(|s| u64::try_from(s).ok());
            }
        }
    }
    Ok(found)
}

/// One SCAN step, with each key's type, TTL and size (pipelined).
pub async fn scan(
    c: &RedisClient,
    db: u8,
    pattern: &str,
    kind: Option<&RedisType>,
    cursor: u64,
    count: usize,
) -> Result<ScanPage> {
    let mut con = manager(c, db).await?;
    let (next, keys) = scan_step(&mut con, c, pattern, kind, cursor, count).await?;
    Ok(ScanPage {
        keys: infos(&mut con, c, keys).await?,
        cursor: next,
    })
}

async fn scan_step(
    con: &mut impl ConnectionLike,
    c: &RedisClient,
    pattern: &str,
    kind: Option<&RedisType>,
    cursor: u64,
    count: usize,
) -> Result<(u64, Vec<Vec<u8>>)> {
    let pattern = if pattern.trim().is_empty() {
        "*"
    } else {
        pattern
    };
    let mut cmd = Cmd::new();
    cmd.arg("SCAN")
        .arg(cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(count.max(1));
    if let Some(kind) = kind {
        cmd.arg("TYPE").arg(kind.name());
    }
    let reply = send(con, c, &cmd).await?;
    scan_reply(&reply)
        .and_then(|(next, keys)| Some((next, bytes_list(keys)?)))
        .ok_or_else(|| anyhow!("{ENGINE} : réponse SCAN inattendue"))
}

/// Every key matching `pattern` (SCAN to the end, duplicates removed).
pub(crate) async fn scan_all(c: &RedisClient, db: u8, pattern: &str) -> Result<Vec<Vec<u8>>> {
    let mut con = manager(c, db).await?;
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    let mut cursor = 0;
    loop {
        let (next, page) = scan_step(&mut con, c, pattern, None, cursor, SCAN_COUNT).await?;
        keys.extend(page.into_iter().filter(|k| seen.insert(k.clone())));
        if next == 0 {
            return Ok(keys);
        }
        cursor = next;
    }
}

pub async fn key_info(c: &RedisClient, db: u8, key: &[u8]) -> Result<Option<KeyInfo>> {
    let mut con = manager(c, db).await?;
    Ok(infos(&mut con, c, vec![key.to_vec()]).await?.pop())
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RedisValue {
    String(Vec<u8>),
    List(Vec<Vec<u8>>),
    Set(Vec<Vec<u8>>),
    ZSet(Vec<(Vec<u8>, f64)>),
    Hash(Vec<(Vec<u8>, Vec<u8>)>),
    Stream(Vec<StreamEntry>),
}

impl RedisValue {
    pub fn kind(&self) -> RedisType {
        match self {
            Self::String(_) => RedisType::String,
            Self::List(_) => RedisType::List,
            Self::Set(_) => RedisType::Set,
            Self::ZSet(_) => RedisType::ZSet,
            Self::Hash(_) => RedisType::Hash,
            Self::Stream(_) => RedisType::Stream,
        }
    }

    /// Elements (bytes for a string).
    pub fn len(&self) -> usize {
        match self {
            Self::String(b) => b.len(),
            Self::List(i) | Self::Set(i) => i.len(),
            Self::ZSet(i) => i.len(),
            Self::Hash(i) => i.len(),
            Self::Stream(i) => i.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValuePage {
    pub value: RedisValue,
    /// Offset of the first element (list, zset, stream; 0 otherwise), as
    /// read: not derived from `total`, which may be stale.
    pub from: u64,
    /// Elements in the whole value.
    pub total: u64,
    /// Where the next page starts: an offset (list, zset, stream) or a
    /// SCAN cursor (set, hash); `None` = last page.
    pub next: Option<u64>,
}

fn unexpected(what: &str) -> anyhow::Error {
    anyhow!("{ENGINE} : réponse inattendue à {what}")
}

/// Up to `limit` elements of `key` from `from` (offset or cursor).
pub async fn read_value(
    c: &RedisClient,
    db: u8,
    key: &[u8],
    from: u64,
    limit: usize,
) -> Result<ValuePage> {
    let info = key_info(c, db, key)
        .await?
        .ok_or_else(|| anyhow!("{ENGINE} : la clé {} n'existe pas", display_bytes(key)))?;
    let mut con = manager(c, db).await?;
    let limit = limit.max(1) as u64;
    let k = key.to_vec();
    let num = |n: u64| n.to_string().into_bytes();
    let by_offset = |len: usize, total: u64| {
        let end = from + len as u64;
        (len as u64 == limit && end < total).then_some(end)
    };
    Ok(match info.kind {
        RedisType::String => {
            let v = send_argv(&mut con, c, &[b"GET".to_vec(), k]).await?;
            let b = bytes_of(&v).ok_or_else(|| unexpected("GET"))?;
            ValuePage {
                total: b.len() as u64,
                value: RedisValue::String(b),
                from: 0,
                next: None,
            }
        }
        RedisType::List => {
            let total = info.size.unwrap_or_default();
            let v = send_argv(
                &mut con,
                c,
                &[b"LRANGE".to_vec(), k, num(from), num(from + limit - 1)],
            )
            .await?;
            let items = bytes_list(&v).ok_or_else(|| unexpected("LRANGE"))?;
            ValuePage {
                next: by_offset(items.len(), total),
                total,
                value: RedisValue::List(items),
                from,
            }
        }
        RedisType::ZSet => {
            let total = info.size.unwrap_or_default();
            let v = send_argv(
                &mut con,
                c,
                &[
                    b"ZRANGE".to_vec(),
                    k,
                    num(from),
                    num(from + limit - 1),
                    b"WITHSCORES".to_vec(),
                ],
            )
            .await?;
            let items = byte_pairs(&v)
                .and_then(scored)
                .ok_or_else(|| unexpected("ZRANGE"))?;
            ValuePage {
                next: by_offset(items.len(), total),
                total,
                value: RedisValue::ZSet(items),
                from,
            }
        }
        RedisType::Set | RedisType::Hash => {
            let set = info.kind == RedisType::Set;
            let name: &[u8] = if set { b"SSCAN" } else { b"HSCAN" };
            let v = send_argv(
                &mut con,
                c,
                &[name.to_vec(), k, num(from), b"COUNT".to_vec(), num(limit)],
            )
            .await?;
            let (cursor, elements) = scan_reply(&v).ok_or_else(|| unexpected("SCAN"))?;
            let value = if set {
                RedisValue::Set(bytes_list(elements).ok_or_else(|| unexpected("SSCAN"))?)
            } else {
                RedisValue::Hash(byte_pairs(elements).ok_or_else(|| unexpected("HSCAN"))?)
            };
            ValuePage {
                value,
                total: info.size.unwrap_or_default(),
                from: 0,
                next: (cursor != 0).then_some(cursor),
            }
        }
        RedisType::Stream => {
            let total = info.size.unwrap_or_default();
            let v = send_argv(
                &mut con,
                c,
                &[
                    b"XRANGE".to_vec(),
                    k,
                    b"-".to_vec(),
                    b"+".to_vec(),
                    b"COUNT".to_vec(),
                    num(from + limit),
                ],
            )
            .await?;
            let entries: Vec<StreamEntry> = stream_entries(&v)
                .ok_or_else(|| unexpected("XRANGE"))?
                .into_iter()
                .skip(from as usize)
                .collect();
            ValuePage {
                next: by_offset(entries.len(), total),
                total,
                value: RedisValue::Stream(entries),
                from,
            }
        }
        RedisType::Other(name) => {
            bail!("{ENGINE} : type « {name} » non pris en charge (lecture seule)")
        }
    })
}

/// The whole value of `key`; `None` when the key is gone.
pub(crate) async fn read_all(c: &RedisClient, db: u8, key: &[u8]) -> Result<Option<RedisValue>> {
    const PAGE: usize = 10_000;
    if key_info(c, db, key).await?.is_none() {
        return Ok(None);
    }
    let mut page = read_value(c, db, key, 0, PAGE).await?;
    let mut value = page.value;
    while let Some(from) = page.next {
        page = read_value(c, db, key, from, PAGE).await?;
        match (&mut value, page.value.clone()) {
            (RedisValue::List(a), RedisValue::List(b))
            | (RedisValue::Set(a), RedisValue::Set(b)) => a.extend(b),
            (RedisValue::ZSet(a), RedisValue::ZSet(b)) => a.extend(b),
            (RedisValue::Hash(a), RedisValue::Hash(b)) => a.extend(b),
            (RedisValue::Stream(a), RedisValue::Stream(b)) => a.extend(b),
            _ => bail!("{ENGINE} : la clé {} a changé de type", display_bytes(key)),
        }
    }
    // SSCAN / HSCAN may return an element twice.
    match &mut value {
        RedisValue::Set(items) => {
            let mut seen = HashSet::new();
            items.retain(|i| seen.insert(i.clone()));
        }
        RedisValue::Hash(pairs) => {
            let mut seen = HashSet::new();
            pairs.retain(|(f, _)| seen.insert(f.clone()));
        }
        _ => {}
    }
    Ok(Some(value))
}

// ---------------------------------------------------------------- grids

fn column(name: &str, type_name: &str, primary: bool) -> Column {
    Column {
        name: name.to_string(),
        type_name: type_name.to_string(),
        nullable: false,
        is_primary_key: primary,
    }
}

/// Fields of a stream entry as console arguments (`name "Ada L" n 1`).
fn fields_text(fields: &[(Vec<u8>, Vec<u8>)]) -> String {
    fields
        .iter()
        .flat_map(|(f, v)| [quote_arg(f), quote_arg(v)])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Field / value pairs (hash, stream entry).
pub(crate) type Pairs = Vec<(Vec<u8>, Vec<u8>)>;

fn parse_fields(text: &str) -> Result<Pairs, String> {
    let tokens = tokenize(text)?;
    if tokens.is_empty() || tokens.len() % 2 != 0 {
        return Err("Champs : « champ valeur [champ valeur …] » attendu".into());
    }
    Ok(tokens
        .chunks(2)
        .map(|p| (p[0].clone(), p[1].clone()))
        .collect())
}

/// Grid of a value; `#` numbers list elements from `offset`.
fn value_grid(v: &RedisValue, offset: u64) -> QueryResult {
    let (columns, rows): (Vec<Column>, Vec<Vec<String>>) = match v {
        RedisValue::String(b) => (
            vec![column("valeur", "string", false)],
            vec![vec![display_bytes(b)]],
        ),
        RedisValue::List(items) => (
            vec![
                column("#", "integer", true),
                column("valeur", "string", false),
            ],
            items
                .iter()
                .enumerate()
                .map(|(i, b)| vec![(offset + i as u64).to_string(), display_bytes(b)])
                .collect(),
        ),
        RedisValue::Set(items) => (
            vec![column("membre", "string", true)],
            items.iter().map(|b| vec![display_bytes(b)]).collect(),
        ),
        RedisValue::ZSet(items) => (
            vec![
                column("membre", "string", true),
                column("score", "double", false),
            ],
            items
                .iter()
                .map(|(m, s)| vec![display_bytes(m), cli::format_score(*s)])
                .collect(),
        ),
        RedisValue::Hash(pairs) => (
            vec![
                column("champ", "string", true),
                column("valeur", "string", false),
            ],
            pairs
                .iter()
                .map(|(f, v)| vec![display_bytes(f), display_bytes(v)])
                .collect(),
        ),
        RedisValue::Stream(entries) => (
            vec![
                column("id", "string", true),
                column("champs", "string", false),
            ],
            entries
                .iter()
                .map(|e| vec![e.id.clone(), fields_text(&e.fields)])
                .collect(),
        ),
    };
    QueryResult {
        primary_key: columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.clone())
            .collect(),
        columns,
        rows,
        ..Default::default()
    }
}

/// Grid of a value; columns: valeur | #, valeur | membre | membre, score |
/// champ, valeur | id, champs. (`#` counts from 0 in `v`; `page_result`
/// numbers a page from its offset.)
pub fn value_result(v: &RedisValue) -> QueryResult {
    value_grid(v, 0)
}

/// Offset of a list / zset / stream page's first element (`from`).
pub fn page_offset(page: &ValuePage) -> u64 {
    match page.value {
        RedisValue::List(_) | RedisValue::ZSet(_) | RedisValue::Stream(_) => page.from,
        _ => 0,
    }
}

/// `value_result` of a page, list indexes counted from the page's offset
/// (the indexes `LSET` takes).
pub fn page_result(page: &ValuePage) -> QueryResult {
    value_grid(&page.value, page_offset(page))
}

/// One change to a key.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyEdit {
    SetString(Vec<u8>),
    /// `LSET` of element `index`, if it still holds `old` (else the list
    /// changed since it was read: refused).
    ListSet {
        index: i64,
        old: Vec<u8>,
        value: Vec<u8>,
    },
    ListPush {
        left: bool,
        value: Vec<u8>,
    },
    /// Removal of element `index`, if it still holds `old`.
    ListRemove {
        index: i64,
        old: Vec<u8>,
    },
    SetAdd(Vec<u8>),
    SetRemove(Vec<u8>),
    ZAdd {
        member: Vec<u8>,
        score: f64,
    },
    ZRemove(Vec<u8>),
    HSet {
        field: Vec<u8>,
        value: Vec<u8>,
    },
    HDel(Vec<u8>),
    XAdd {
        id: Option<String>,
        fields: Vec<(Vec<u8>, Vec<u8>)>,
    },
    XDel(String),
    /// Seconds to live; `None` = persistent.
    Expire(Option<i64>),
    Rename {
        to: Vec<u8>,
    },
    Delete,
}

/// Value a removed list element is first set to (`LSET`), then removed by
/// (`LREM`): list elements have no identity but their index.
fn removal_sentinel() -> Vec<u8> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "\u{0}storingUnicorns:removed:{nanos}:{}\u{0}",
        std::process::id()
    )
    .into_bytes()
}

/// `LSET` of element ARGV[1] to ARGV[3] if it still is ARGV[2]; 0 when the
/// list changed (an index alone could hit another element).
const LIST_SET_SCRIPT: &str =
    "if redis.call('LINDEX', KEYS[1], ARGV[1]) ~= ARGV[2] then return 0 end \
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3]) return 1";

/// Removal of element ARGV[1] if it still is ARGV[2]: set to the sentinel
/// ARGV[3], then `LREM` it; 0 when the list changed.
const LIST_REMOVE_SCRIPT: &str =
    "if redis.call('LINDEX', KEYS[1], ARGV[1]) ~= ARGV[2] then return 0 end \
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3]) return redis.call('LREM', KEYS[1], 1, ARGV[3])";

/// The command (argv) of an edit (its first, when there are several).
pub fn edit_to_command(key: &[u8], e: &KeyEdit) -> Vec<Vec<u8>> {
    edit_to_commands(key, e).swap_remove(0)
}

/// The commands of an edit, run in order (in one MULTI when several).
/// `ListSet` / `ListRemove` are `EVAL` scripts checking the element first.
pub fn edit_to_commands(key: &[u8], e: &KeyEdit) -> Vec<Vec<Vec<u8>>> {
    let k = key.to_vec();
    let b = |s: &str| s.as_bytes().to_vec();
    let one = |v: Vec<Vec<u8>>| vec![v];
    match e {
        KeyEdit::SetString(v) => one(vec![b("SET"), k, v.clone(), b("KEEPTTL")]),
        KeyEdit::ListSet { index, old, value } => one(vec![
            b("EVAL"),
            b(LIST_SET_SCRIPT),
            b("1"),
            k,
            b(&index.to_string()),
            old.clone(),
            value.clone(),
        ]),
        KeyEdit::ListPush { left, value } => one(vec![
            b(if *left { "LPUSH" } else { "RPUSH" }),
            k,
            value.clone(),
        ]),
        KeyEdit::ListRemove { index, old } => one(vec![
            b("EVAL"),
            b(LIST_REMOVE_SCRIPT),
            b("1"),
            k,
            b(&index.to_string()),
            old.clone(),
            removal_sentinel(),
        ]),
        KeyEdit::SetAdd(m) => one(vec![b("SADD"), k, m.clone()]),
        KeyEdit::SetRemove(m) => one(vec![b("SREM"), k, m.clone()]),
        KeyEdit::ZAdd { member, score } => one(vec![
            b("ZADD"),
            k,
            b(&cli::format_score(*score)),
            member.clone(),
        ]),
        KeyEdit::ZRemove(m) => one(vec![b("ZREM"), k, m.clone()]),
        KeyEdit::HSet { field, value } => one(vec![b("HSET"), k, field.clone(), value.clone()]),
        KeyEdit::HDel(f) => one(vec![b("HDEL"), k, f.clone()]),
        KeyEdit::XAdd { id, fields } => {
            let mut argv = vec![b("XADD"), k, b(id.as_deref().unwrap_or("*"))];
            for (f, v) in fields {
                argv.push(f.clone());
                argv.push(v.clone());
            }
            one(argv)
        }
        KeyEdit::XDel(id) => one(vec![b("XDEL"), k, b(id)]),
        KeyEdit::Expire(Some(s)) => one(vec![b("EXPIRE"), k, b(&s.to_string())]),
        KeyEdit::Expire(None) => one(vec![b("PERSIST"), k]),
        KeyEdit::Rename { to } => one(vec![b("RENAMENX"), k, to.clone()]),
        KeyEdit::Delete => one(vec![b("DEL"), k]),
    }
}

/// A grid cell as bytes (Redis has no NULL).
fn cell_bytes(cell: Option<&String>, column: &str) -> Result<Vec<u8>, String> {
    match cell {
        None => Err(format!("{column} : valeur requise")),
        Some(c) if is_null(c) || is_absent(c) => {
            Err(format!("{column} : Redis n'a pas de valeur NULL"))
        }
        Some(c) => parse_display(c).map_err(|e| format!("{column} : {e}")),
    }
}

fn score_cell(cell: Option<&String>) -> Result<f64, String> {
    match cell {
        None => Ok(0.0),
        Some(c) if is_null(c) || is_absent(c) => Ok(0.0),
        Some(c) => parse_score(c),
    }
}

/// Index of a list row of `page`, checked against the page's element: `#`
/// counted from the page's offset (`page_result`) or from 0
/// (`value_result`).
fn list_index(page: &ValuePage, row: &[String]) -> Result<i64, String> {
    let RedisValue::List(items) = &page.value else {
        return Err("liste attendue".into());
    };
    let shown: u64 = row
        .first()
        .and_then(|c| c.parse().ok())
        .ok_or("# invalide")?;
    let value = cell_bytes(row.get(1), "valeur")?;
    let offset = page_offset(page);
    let at = |i: u64| items.get(i as usize) == Some(&value);
    let index = if shown >= offset && at(shown - offset) {
        shown
    } else if at(shown) {
        offset + shown
    } else {
        return Err(format!(
            "L'élément #{shown} a changé entre-temps : recharger la valeur"
        ));
    };
    i64::try_from(index).map_err(|e| e.to_string())
}

/// The value a console grid of `kind` shows (a single-key read, see
/// `Source::Key`), as the `ValuePage` [`grid_edits`] checks rows against:
/// list elements from the `valeur` column, offset by the first row's `#`
/// (`LRANGE k 5 9` shows #5..#9); the other types only need their kind.
pub fn console_page(kind: &RedisType, result: &QueryResult) -> Result<ValuePage, String> {
    let mut offset = 0;
    let value = match kind {
        RedisType::String => RedisValue::String(Vec::new()),
        RedisType::List => {
            if let Some(first) = result.rows.first() {
                offset = first
                    .first()
                    .and_then(|c| c.parse().ok())
                    .ok_or("# invalide")?;
            }
            RedisValue::List(
                result
                    .rows
                    .iter()
                    .map(|r| parse_display(r.get(1).map_or("", String::as_str)))
                    .collect::<Result<_, _>>()?,
            )
        }
        RedisType::Set => RedisValue::Set(Vec::new()),
        RedisType::ZSet => RedisValue::ZSet(Vec::new()),
        RedisType::Hash => RedisValue::Hash(Vec::new()),
        RedisType::Stream => RedisValue::Stream(Vec::new()),
        RedisType::Other(t) => return Err(format!("Type {t} en lecture seule")),
    };
    let end = offset + value.len() as u64;
    Ok(ValuePage {
        value,
        total: end,
        from: offset,
        next: Some(end),
    })
}

/// The edits of a console grid's pending changes (see [`console_page`]);
/// deleting the row of a string deletes the key.
pub fn console_edits(
    key: &[u8],
    kind: &RedisType,
    result: &QueryResult,
    changes: &RowChanges,
) -> Result<Vec<KeyEdit>, String> {
    if *kind == RedisType::String
        && !changes.deletes.is_empty()
        && changes.inserts.is_empty()
        && changes.updates.is_empty()
    {
        return Ok(vec![KeyEdit::Delete]);
    }
    let info = KeyInfo {
        key: key.to_vec(),
        kind: kind.clone(),
        ttl: None,
        size: None,
    };
    grid_edits(&info, &console_page(kind, result)?, changes)
}

/// The edits of a value grid's pending changes (rows of `value_result` /
/// `page_result`). Order: updates, removals (list: from the last index),
/// insertions.
pub fn grid_edits(
    key: &KeyInfo,
    page: &ValuePage,
    changes: &RowChanges,
) -> Result<Vec<KeyEdit>, String> {
    let mut edits = Vec::new();
    let mut removals = Vec::new();
    let mut inserts = Vec::new();
    match &page.value {
        RedisValue::String(_) => {
            if !changes.inserts.is_empty() || !changes.deletes.is_empty() {
                return Err(
                    "Une chaîne n'a qu'une valeur : la modifier, ou supprimer la clé".into(),
                );
            }
            for (_, new) in &changes.updates {
                edits.push(KeyEdit::SetString(cell_bytes(new.first(), "valeur")?));
            }
        }
        RedisValue::List(_) => {
            for (old, new) in &changes.updates {
                if old.first() != new.first() {
                    return Err("Le # d'un élément ne se modifie pas".into());
                }
                let index = list_index(page, old)?;
                if old.get(1) != new.get(1) {
                    edits.push(KeyEdit::ListSet {
                        index,
                        old: cell_bytes(old.get(1), "valeur")?,
                        value: cell_bytes(new.get(1), "valeur")?,
                    });
                }
            }
            let mut indexes = changes
                .deletes
                .iter()
                .map(|row| Ok((list_index(page, row)?, cell_bytes(row.get(1), "valeur")?)))
                .collect::<Result<Vec<_>, String>>()?;
            // From the last: a removal doesn't move the ones before it.
            indexes.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            indexes.dedup_by_key(|(i, _)| *i);
            removals.extend(
                indexes
                    .into_iter()
                    .map(|(index, old)| KeyEdit::ListRemove { index, old }),
            );
            for row in &changes.inserts {
                inserts.push(KeyEdit::ListPush {
                    left: false,
                    value: cell_bytes(row.get(1).and_then(Option::as_ref), "valeur")?,
                });
            }
        }
        RedisValue::Set(_) => {
            for (old, new) in &changes.updates {
                let (from, to) = (
                    cell_bytes(old.first(), "membre")?,
                    cell_bytes(new.first(), "membre")?,
                );
                if from != to {
                    edits.push(KeyEdit::SetRemove(from));
                    edits.push(KeyEdit::SetAdd(to));
                }
            }
            for row in &changes.deletes {
                removals.push(KeyEdit::SetRemove(cell_bytes(row.first(), "membre")?));
            }
            for row in &changes.inserts {
                inserts.push(KeyEdit::SetAdd(cell_bytes(
                    row.first().and_then(Option::as_ref),
                    "membre",
                )?));
            }
        }
        RedisValue::ZSet(_) => {
            for (old, new) in &changes.updates {
                let from = cell_bytes(old.first(), "membre")?;
                let to = cell_bytes(new.first(), "membre")?;
                let score = score_cell(new.get(1))?;
                if from != to {
                    edits.push(KeyEdit::ZRemove(from));
                } else if old.get(1) == new.get(1) {
                    continue;
                }
                edits.push(KeyEdit::ZAdd { member: to, score });
            }
            for row in &changes.deletes {
                removals.push(KeyEdit::ZRemove(cell_bytes(row.first(), "membre")?));
            }
            for row in &changes.inserts {
                inserts.push(KeyEdit::ZAdd {
                    member: cell_bytes(row.first().and_then(Option::as_ref), "membre")?,
                    score: score_cell(row.get(1).and_then(Option::as_ref))?,
                });
            }
        }
        RedisValue::Hash(_) => {
            let value = |c: Option<&String>| match c {
                None => Ok(Vec::new()),
                Some(_) => cell_bytes(c, "valeur"),
            };
            for (old, new) in &changes.updates {
                let from = cell_bytes(old.first(), "champ")?;
                let to = cell_bytes(new.first(), "champ")?;
                if from != to {
                    edits.push(KeyEdit::HDel(from));
                } else if old.get(1) == new.get(1) {
                    continue;
                }
                edits.push(KeyEdit::HSet {
                    field: to,
                    value: value(new.get(1))?,
                });
            }
            for row in &changes.deletes {
                removals.push(KeyEdit::HDel(cell_bytes(row.first(), "champ")?));
            }
            for row in &changes.inserts {
                inserts.push(KeyEdit::HSet {
                    field: cell_bytes(row.first().and_then(Option::as_ref), "champ")?,
                    value: value(row.get(1).and_then(Option::as_ref))?,
                });
            }
        }
        RedisValue::Stream(_) => {
            if !changes.updates.is_empty() {
                return Err(
                    "Une entrée de flux ne se modifie pas : la supprimer puis en ajouter une"
                        .into(),
                );
            }
            for row in &changes.deletes {
                let id = row.first().ok_or("id manquant")?;
                removals.push(KeyEdit::XDel(id.clone()));
            }
            for row in &changes.inserts {
                let id = row
                    .first()
                    .and_then(Option::as_ref)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && s != "*" && !is_null(s) && !is_absent(s));
                let fields = row
                    .get(1)
                    .and_then(Option::as_ref)
                    .ok_or("champs : valeur requise")?;
                inserts.push(KeyEdit::XAdd {
                    id,
                    fields: parse_fields(fields)?,
                });
            }
        }
    }
    let _ = key; // the type comes from the page; the key is the caller's
    edits.extend(removals);
    edits.extend(inserts);
    Ok(edits)
}

/// Run `cmds` (atomically when several).
async fn run_commands(
    con: &mut ConnectionManager,
    c: &RedisClient,
    cmds: &[Vec<Vec<u8>>],
) -> Result<Value> {
    if let [one] = cmds {
        return send_argv(con, c, one).await;
    }
    let mut p = ::redis::pipe();
    p.atomic();
    for argv in cmds {
        p.add_command(command(argv));
    }
    let replies: Vec<Value> = p
        .query_async(con)
        .await
        .map_err(|e| friendly(&c.client, e))?;
    replies.into_iter().last().ok_or_else(|| unexpected("EXEC"))
}

/// Apply `edits` to `key` in order; returns how many were applied. After a
/// `Rename`, the next edits apply to the new name.
pub async fn apply_edits(c: &RedisClient, db: u8, key: &[u8], edits: &[KeyEdit]) -> Result<usize> {
    let mut con = manager(c, db).await?;
    let mut key = key.to_vec();
    for (done, e) in edits.iter().enumerate() {
        let reply = run_commands(&mut con, c, &edit_to_commands(&key, e)).await;
        let check = reply.and_then(|r| {
            let zero = int_of(&r) == Some(0);
            match e {
                KeyEdit::Rename { to } if zero => {
                    bail!("la clé {} existe déjà", display_bytes(to))
                }
                KeyEdit::Expire(Some(_)) if zero => {
                    bail!("la clé {} n'existe pas", display_bytes(&key))
                }
                KeyEdit::ListSet { index, .. } | KeyEdit::ListRemove { index, .. } if zero => {
                    bail!("la liste a changé (élément #{index}), rechargez")
                }
                KeyEdit::XDel(id) if zero => bail!("entrée {id} introuvable"),
                _ => Ok(()),
            }
        });
        if let Err(err) = check {
            bail!("{done} appliquée(s), échec : {err}");
        }
        if let KeyEdit::Rename { to } = e {
            key = to.clone();
        }
    }
    Ok(edits.len())
}

/// The commands writing `value` to a missing `key`.
fn value_commands(key: &[u8], value: &RedisValue) -> Result<Vec<Vec<Vec<u8>>>, String> {
    let k = key.to_vec();
    let b = |s: &str| s.as_bytes().to_vec();
    if value.is_empty() && !matches!(value, RedisValue::String(_)) {
        return Err(format!(
            "valeur vide : un(e) {} vide n'existe pas en Redis",
            value.kind().name()
        ));
    }
    let chunked = |name: &str, args: Vec<Vec<u8>>, per: usize| -> Vec<Vec<Vec<u8>>> {
        args.chunks(WRITE_CHUNK * per)
            .map(|chunk| {
                let mut argv = vec![b(name), k.clone()];
                argv.extend(chunk.iter().cloned());
                argv
            })
            .collect()
    };
    Ok(match value {
        RedisValue::String(v) => vec![vec![b("SET"), k.clone(), v.clone()]],
        RedisValue::List(items) => chunked("RPUSH", items.clone(), 1),
        RedisValue::Set(items) => chunked("SADD", items.clone(), 1),
        RedisValue::ZSet(items) => chunked(
            "ZADD",
            items
                .iter()
                .flat_map(|(m, s)| [b(&cli::format_score(*s)), m.clone()])
                .collect(),
            2,
        ),
        RedisValue::Hash(pairs) => chunked(
            "HSET",
            pairs
                .iter()
                .flat_map(|(f, v)| [f.clone(), v.clone()])
                .collect(),
            2,
        ),
        RedisValue::Stream(entries) => entries
            .iter()
            .map(|e| {
                if e.fields.is_empty() {
                    return Err(format!("entrée {} sans champ", e.id));
                }
                let id = if e.id.trim().is_empty() {
                    "*"
                } else {
                    e.id.trim()
                };
                let mut argv = vec![b("XADD"), k.clone(), b(id)];
                for (f, v) in &e.fields {
                    argv.push(f.clone());
                    argv.push(v.clone());
                }
                Ok(argv)
            })
            .collect::<Result<_, String>>()?,
    })
}

/// Write `key` in one MULTI (replacing it when `replace`); returns whether
/// it existed.
pub(crate) async fn write_key(
    c: &RedisClient,
    db: u8,
    key: &[u8],
    value: &RedisValue,
    ttl: Option<i64>,
    replace: bool,
) -> Result<bool> {
    if key.is_empty() {
        bail!("nom de clé vide");
    }
    let mut cmds = value_commands(key, value).map_err(anyhow::Error::msg)?;
    let mut con = manager(c, db).await?;
    let exists = send_argv(&mut con, c, &[b"EXISTS".to_vec(), key.to_vec()]).await?;
    let existed = int_of(&exists).unwrap_or(0) > 0;
    if existed && !replace {
        bail!("la clé {} existe déjà", display_bytes(key));
    }
    if existed {
        cmds.insert(0, vec![b"DEL".to_vec(), key.to_vec()]);
    }
    if let Some(ttl) = ttl.filter(|t| *t > 0) {
        cmds.push(vec![
            b"EXPIRE".to_vec(),
            key.to_vec(),
            ttl.to_string().into_bytes(),
        ]);
    }
    let mut p = ::redis::pipe();
    p.atomic();
    for argv in &cmds {
        p.add_command(command(argv)).ignore();
    }
    p.query_async::<()>(&mut con)
        .await
        .map_err(|e| friendly(&c.client, e))?;
    Ok(existed)
}

/// Create `key` (refused when it exists).
pub async fn create_key(
    c: &RedisClient,
    db: u8,
    key: &[u8],
    value: &RedisValue,
    ttl: Option<i64>,
) -> Result<()> {
    write_key(c, db, key, value, ttl, false).await.map(|_| ())
}

pub async fn flush_db(c: &RedisClient, db: u8) -> Result<()> {
    let mut con = manager(c, db).await?;
    send_argv(&mut con, c, &args(&[b"FLUSHDB"])).await?;
    Ok(())
}

/// A node of the key tree: a prefix group, or a key (`key` indexes the
/// keys given to `key_tree`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyNode {
    pub label: String,
    /// Full prefix of a group (`user:42:`), the key itself for a key.
    pub prefix: String,
    pub children: Vec<KeyNode>,
    pub key: Option<usize>,
    /// Keys under this node.
    pub count: usize,
}

// -------------------------------------------------------------- console

/// Byte ranges of the commands of a console script: one per line, blank
/// lines and `#` / `//` comments skipped.
pub fn units(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        let body = line.trim_end_matches('\r');
        let trimmed = body.trim_start();
        let lead = body.len() - trimmed.len();
        let trimmed = trimmed.trim_end();
        if !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with("//") {
            out.push(start + lead..start + lead + trimmed.len());
        }
        start += line.len() + 1;
    }
    out
}

/// The units of `range` (all when `None`; an empty range = the unit at it).
fn selected(text: &str, range: Option<Range<usize>>) -> Vec<Range<usize>> {
    units(text)
        .into_iter()
        .filter(|u| match &range {
            None => true,
            Some(r) if r.is_empty() => u.start <= r.start && r.start <= u.end,
            Some(r) => u.start < r.end && r.start < u.end,
        })
        .collect()
}

/// Commands to confirm among the units of `range` (all when `None`).
pub fn warnings(text: &str, range: Option<Range<usize>>) -> Vec<Warning> {
    selected(text, range)
        .into_iter()
        .filter_map(|u| {
            let unit = &text[u];
            match classify(&tokenize(unit).ok()?) {
                Safety::Confirm(reason) => Some(Warning {
                    unit: unit.to_string(),
                    reason,
                }),
                _ => None,
            }
        })
        .collect()
}

/// The console's connection and the database it is on.
struct Console {
    con: MultiplexedConnection,
    db: u8,
}

pub async fn run_script(
    c: &RedisClient,
    text: &str,
    range: Option<Range<usize>>,
    scope: &mut NoSqlScope,
    max_rows: usize,
) -> Vec<NoSqlOutcome> {
    let mut out = Vec::new();
    let mut console: Option<Console> = None;
    for u in selected(text, range) {
        let unit = &text[u];
        let started = Instant::now();
        let result = run_unit(c, &mut console, unit, scope, max_rows.max(1)).await;
        let (result, source) = match result {
            Ok((r, s)) => (Ok(r), s),
            Err(e) => (Err(e), None),
        };
        let failed = result.is_err();
        out.push(NoSqlOutcome {
            read_only: (!failed).then(|| names_only_note(unit)).flatten(),
            outcome: StatementOutcome {
                sql: unit.to_string(),
                result,
                elapsed_ms: started.elapsed().as_millis(),
            },
            source,
        });
        if failed {
            break;
        }
    }
    out
}

/// Why `HSCAN … NOVALUES` / `ZSCAN … NOSCORES` rows can't be edited.
fn names_only_note(unit: &str) -> Option<String> {
    let argv = tokenize(unit).ok()?;
    let name = String::from_utf8_lossy(argv.first()?).to_ascii_uppercase();
    let flag = argv
        .iter()
        .skip(2)
        .find(|a| a.eq_ignore_ascii_case(b"NOVALUES") || a.eq_ignore_ascii_case(b"NOSCORES"))?;
    matches!(name.as_str(), "HSCAN" | "ZSCAN").then(|| {
        format!(
            "Lecture seule : {} ne renvoie que les noms",
            String::from_utf8_lossy(flag).to_ascii_uppercase()
        )
    })
}

async fn run_unit(
    c: &RedisClient,
    console: &mut Option<Console>,
    unit: &str,
    scope: &mut NoSqlScope,
    max_rows: usize,
) -> Result<(QueryResult, Option<Source>), String> {
    let argv = tokenize(unit)?;
    if argv.is_empty() {
        return Ok((QueryResult::default(), None));
    }
    let safety = classify(&argv);
    if let Safety::Refuse(reason) = safety {
        return Err(reason);
    }
    if console.is_none() {
        let db = parse_db(&scope.database, c.default_db).map_err(|e| e.to_string())?;
        let con = dedicated(c, db).await.map_err(|e| e.to_string())?;
        *console = Some(Console { con, db });
    }
    let Some(Console { con, db }) = console.as_mut() else {
        unreachable!()
    };
    let started = Instant::now();
    if safety == Safety::RewriteScan {
        let mut result = keys_by_scan(con, c, &argv, max_rows)
            .await
            .map_err(|e| e.to_string())?;
        result.execution_time_ms = started.elapsed().as_millis();
        return Ok((result, None));
    }
    let reply = send_argv(con, c, &argv).await.map_err(|e| e.to_string())?;
    let name = String::from_utf8_lossy(&argv[0]).to_ascii_uppercase();
    if name == "SELECT" {
        if let Some(n) = argv
            .get(1)
            .and_then(|a| std::str::from_utf8(a).ok()?.trim().parse::<u8>().ok())
        {
            *db = n;
            scope.database = n.to_string();
        }
    }
    let (mut result, source) = match key_read(&argv, &reply) {
        Some((value, offset, note)) => {
            let mut grid = value_grid(&value, offset);
            if let (Some(note), Some(c)) = (note, grid.columns.first_mut()) {
                c.name = format!("{} (curseur suivant : {note})", c.name);
            }
            let source = Source::Key {
                db: *db,
                key: argv[1].clone(),
                kind: value.kind(),
            };
            (grid, Some(source))
        }
        None => (reply_to_result(&argv, &reply), None),
    };
    if result.rows.len() > max_rows {
        result.rows.truncate(max_rows);
        result.truncated = true;
    }
    result.execution_time_ms = started.elapsed().as_millis();
    Ok((result, source))
}

/// A single-key read whose grid can be edited: the value read, the offset of
/// its first element, and the next cursor of a SCAN-family read.
fn key_read(argv: &[Vec<u8>], reply: &Value) -> Option<(RedisValue, u64, Option<u64>)> {
    let name = String::from_utf8_lossy(argv.first()?).to_ascii_uppercase();
    argv.get(1)?;
    let has = |w: &[u8]| argv.iter().skip(2).any(|a| a.eq_ignore_ascii_case(w));
    let arg_u64 =
        |i: usize| -> Option<u64> { std::str::from_utf8(argv.get(i)?).ok()?.parse().ok() };
    match name.as_str() {
        "GET" if argv.len() == 2 => Some((
            RedisValue::String(match reply {
                Value::BulkString(b) => b.clone(),
                _ => return None,
            }),
            0,
            None,
        )),
        "HGETALL" if argv.len() == 2 => Some((RedisValue::Hash(byte_pairs(reply)?), 0, None)),
        "SMEMBERS" if argv.len() == 2 => Some((RedisValue::Set(bytes_list(reply)?), 0, None)),
        // A negative start would need LLEN to number the elements.
        "LRANGE" if argv.len() == 4 => {
            Some((RedisValue::List(bytes_list(reply)?), arg_u64(2)?, None))
        }
        "ZRANGE" | "ZREVRANGE" | "ZRANGEBYSCORE" | "ZREVRANGEBYSCORE" if has(b"WITHSCORES") => {
            Some((RedisValue::ZSet(scored(byte_pairs(reply)?)?), 0, None))
        }
        "XRANGE" | "XREVRANGE" => Some((RedisValue::Stream(stream_entries(reply)?), 0, None)),
        // Names only (`NOVALUES` / `NOSCORES`): not the value's shape.
        "HSCAN" | "ZSCAN" if has(b"NOVALUES") || has(b"NOSCORES") => None,
        "SSCAN" | "HSCAN" | "ZSCAN" => {
            let (cursor, elements) = scan_reply(reply)?;
            let value = match name.as_str() {
                "SSCAN" => RedisValue::Set(bytes_list(elements)?),
                "HSCAN" => RedisValue::Hash(byte_pairs(elements)?),
                _ => RedisValue::ZSet(scored(byte_pairs(elements)?)?),
            };
            Some((value, 0, Some(cursor)))
        }
        _ => None,
    }
}

/// `KEYS pattern`, run as SCAN MATCH (non-blocking) up to `max_rows` keys.
async fn keys_by_scan(
    con: &mut MultiplexedConnection,
    c: &RedisClient,
    argv: &[Vec<u8>],
    max_rows: usize,
) -> Result<QueryResult> {
    let [_, pattern] = argv else {
        bail!("KEYS : un motif attendu (KEYS motif)");
    };
    let pattern = String::from_utf8_lossy(pattern).into_owned();
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    let mut cursor = 0;
    let mut truncated = false;
    loop {
        let (next, page) = scan_step(con, c, &pattern, None, cursor, SCAN_COUNT).await?;
        keys.extend(page.into_iter().filter(|k| seen.insert(k.clone())));
        if keys.len() >= max_rows && next != 0 || keys.len() > max_rows {
            truncated = true;
            break;
        }
        if next == 0 {
            break;
        }
        cursor = next;
    }
    keys.truncate(max_rows);
    let mut rows: Vec<Vec<String>> = keys.iter().map(|k| vec![display_bytes(k)]).collect();
    rows.sort();
    Ok(QueryResult {
        columns: vec![column("clé (KEYS exécuté avec SCAN)", "string", false)],
        rows,
        truncated,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::NULL_CELL;

    #[test]
    fn display_bytes_round_trips() {
        for b in [
            b"plain text".to_vec(),
            b"".to_vec(),
            "héllo wörld".as_bytes().to_vec(),
            vec![0xff, 0x00, 0x41, 0x80],
            b"line\nbreak\ttab\r".to_vec(),
            b"\"quoted\"".to_vec(),
            b"\"".to_vec(),
            b"back\\slash \\x41".to_vec(),
            vec![0x7f, 0x07, 0x08, 0x1b],
            [b"caf".as_slice(), "é".as_bytes(), &[0xc3]].concat(),
        ] {
            let shown = display_bytes(&b);
            assert_eq!(parse_display(&shown).unwrap(), b, "{shown}");
        }
    }

    #[test]
    fn display_bytes_forms() {
        assert_eq!(display_bytes(b"user:1"), "user:1");
        assert_eq!(display_bytes(b"a \\x41"), "a \\x41");
        assert_eq!(display_bytes(&[0xff, 0x00]), r#""\xff\x00""#);
        assert_eq!(display_bytes(b"a\nb"), r#""a\nb""#);
        assert_eq!(display_bytes("é\u{1}".as_bytes()), r#""é\x01""#);
        assert_eq!(display_bytes(b"\"x\""), r#""\"x\"""#);
        assert_eq!(parse_display("not \"quoted").unwrap(), b"not \"quoted");
        assert!(parse_display("\"open").is_err());
        assert!(parse_display("\"a\" \"b\"").is_err());
        assert!(parse_display("\"a\" ").is_err());
    }

    #[test]
    fn scores() {
        assert_eq!(parse_score("1.5"), Ok(1.5));
        assert_eq!(parse_score("+inf"), Ok(f64::INFINITY));
        assert_eq!(parse_score("-INF"), Ok(f64::NEG_INFINITY));
        assert!(parse_score("abc").is_err());
        assert!(parse_score("nan").is_err());
    }

    #[test]
    fn types_by_name() {
        for t in [
            RedisType::String,
            RedisType::List,
            RedisType::Set,
            RedisType::ZSet,
            RedisType::Hash,
            RedisType::Stream,
            RedisType::Other("ReJSON-RL".into()),
        ] {
            assert_eq!(RedisType::from_name(t.name()), t);
        }
        assert_eq!(db_index("db12"), Some(12));
        assert_eq!(db_index("x"), None);
    }

    #[test]
    fn keyspace_counts() {
        let text = "# Keyspace\r\ndb0:keys=12,expires=1,avg_ttl=0\r\ndb15:keys=3,expires=0\r\n";
        assert_eq!(parse_keyspace(text), BTreeMap::from([(0, 12), (15, 3)]));
    }

    fn strs(argv: Vec<Vec<u8>>) -> Vec<String> {
        argv.into_iter()
            .map(|a| String::from_utf8_lossy(&a).into_owned())
            .collect()
    }

    #[test]
    fn edit_commands() {
        let k = b"k";
        let cases: Vec<(KeyEdit, &str)> = vec![
            (KeyEdit::SetString(b"v".to_vec()), "SET k v KEEPTTL"),
            (
                KeyEdit::ListPush {
                    left: true,
                    value: b"v".to_vec(),
                },
                "LPUSH k v",
            ),
            (
                KeyEdit::ListPush {
                    left: false,
                    value: b"v".to_vec(),
                },
                "RPUSH k v",
            ),
            (KeyEdit::SetAdd(b"m".to_vec()), "SADD k m"),
            (KeyEdit::SetRemove(b"m".to_vec()), "SREM k m"),
            (
                KeyEdit::ZAdd {
                    member: b"m".to_vec(),
                    score: 1.5,
                },
                "ZADD k 1.5 m",
            ),
            (
                KeyEdit::ZAdd {
                    member: b"m".to_vec(),
                    score: f64::NEG_INFINITY,
                },
                "ZADD k -inf m",
            ),
            (KeyEdit::ZRemove(b"m".to_vec()), "ZREM k m"),
            (
                KeyEdit::HSet {
                    field: b"f".to_vec(),
                    value: b"v".to_vec(),
                },
                "HSET k f v",
            ),
            (KeyEdit::HDel(b"f".to_vec()), "HDEL k f"),
            (
                KeyEdit::XAdd {
                    id: None,
                    fields: vec![(b"a".to_vec(), b"1".to_vec())],
                },
                "XADD k * a 1",
            ),
            (
                KeyEdit::XAdd {
                    id: Some("5-1".into()),
                    fields: vec![(b"a".to_vec(), b"1".to_vec())],
                },
                "XADD k 5-1 a 1",
            ),
            (KeyEdit::XDel("1-0".into()), "XDEL k 1-0"),
            (KeyEdit::Expire(Some(60)), "EXPIRE k 60"),
            (KeyEdit::Expire(None), "PERSIST k"),
            (KeyEdit::Rename { to: b"k2".to_vec() }, "RENAMENX k k2"),
            (KeyEdit::Delete, "DEL k"),
        ];
        for (edit, expected) in cases {
            assert_eq!(strs(edit_to_command(k, &edit)).join(" "), expected);
            assert_eq!(edit_to_commands(k, &edit).len(), 1);
        }
        // List edits by index check the element first (one EVAL each).
        let set = KeyEdit::ListSet {
            index: 2,
            old: b"o".to_vec(),
            value: b"v".to_vec(),
        };
        let set = edit_to_commands(k, &set);
        assert_eq!(set.len(), 1);
        let argv = strs(set[0].clone());
        assert_eq!(argv[0], "EVAL");
        assert!(argv[1].contains("LINDEX") && argv[1].contains("LSET"));
        assert_eq!(argv[2..], ["1", "k", "2", "o", "v"]);
        let remove = KeyEdit::ListRemove {
            index: 3,
            old: b"o".to_vec(),
        };
        let remove = edit_to_commands(k, &remove);
        assert_eq!(remove.len(), 1);
        let argv = strs(remove[0].clone());
        assert!(argv[1].contains("LINDEX") && argv[1].contains("LREM"));
        assert_eq!(argv[2..6], ["1", "k", "3", "o"]);
        assert!(argv[6].contains("storingUnicorns:removed"), "sentinel");
    }

    fn info(kind: RedisType) -> KeyInfo {
        KeyInfo {
            key: b"k".to_vec(),
            kind,
            ttl: None,
            size: None,
        }
    }

    /// A page ending at `next` (or `total`).
    fn page(value: RedisValue, total: u64, next: Option<u64>) -> ValuePage {
        let from = next.unwrap_or(total).saturating_sub(value.len() as u64);
        ValuePage {
            value,
            total,
            from,
            next,
        }
    }

    fn row(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    fn new_row(cells: &[Option<&str>]) -> Vec<Option<String>> {
        cells.iter().map(|c| c.map(str::to_string)).collect()
    }

    fn names(r: &QueryResult) -> Vec<&str> {
        r.columns.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn value_results_per_type() {
        let r = value_result(&RedisValue::String(vec![0xff]));
        assert_eq!(names(&r), ["valeur"]);
        assert_eq!(r.rows, [[r#""\xff""#]]);
        let r = value_result(&RedisValue::List(vec![b"a".to_vec(), b"b".to_vec()]));
        assert_eq!(names(&r), ["#", "valeur"]);
        assert_eq!(r.rows, [["0", "a"], ["1", "b"]]);
        assert_eq!(r.primary_key, ["#"]);
        let r = value_result(&RedisValue::Set(vec![b"m".to_vec()]));
        assert_eq!(names(&r), ["membre"]);
        let r = value_result(&RedisValue::ZSet(vec![
            (b"m".to_vec(), 2.5),
            (b"n".to_vec(), f64::INFINITY),
        ]));
        assert_eq!(names(&r), ["membre", "score"]);
        assert_eq!(r.rows, [["m", "2.5"], ["n", "+inf"]]);
        let r = value_result(&RedisValue::Hash(vec![(b"f".to_vec(), b"v".to_vec())]));
        assert_eq!(names(&r), ["champ", "valeur"]);
        assert_eq!(r.primary_key, ["champ"]);
        let r = value_result(&RedisValue::Stream(vec![StreamEntry {
            id: "1-0".into(),
            fields: vec![(b"name".to_vec(), b"Ada L".to_vec())],
        }]));
        assert_eq!(names(&r), ["id", "champs"]);
        assert_eq!(r.rows, [["1-0", r#"name "Ada L""#]]);
        // Pages are numbered from their offset.
        let p = page(
            RedisValue::List(vec![b"x".to_vec(), b"y".to_vec()]),
            10,
            Some(6),
        );
        assert_eq!(page_offset(&p), 4);
        assert_eq!(page_result(&p).rows[0][0], "4");
        let last = page(RedisValue::List(vec![b"z".to_vec()]), 10, None);
        assert_eq!(page_offset(&last), 9);
        // The offset read, not one derived from a stale length (the list
        // shrank to 3 elements after LLEN said 10).
        let stale = ValuePage {
            value: RedisValue::List(vec![b"h".to_vec(), b"i".to_vec()]),
            total: 3,
            from: 7,
            next: None,
        };
        assert_eq!(page_offset(&stale), 7);
        assert_eq!(page_result(&stale).rows[0][0], "7");
    }

    #[test]
    fn grid_edits_of_a_string() {
        let p = page(RedisValue::String(b"old".to_vec()), 3, None);
        let changes = RowChanges {
            updates: vec![(row(&["old"]), row(&["\"n\\xffew\""]))],
            ..Default::default()
        };
        assert_eq!(
            grid_edits(&info(RedisType::String), &p, &changes).unwrap(),
            [KeyEdit::SetString(b"n\xffew".to_vec())]
        );
        let insert = RowChanges {
            inserts: vec![new_row(&[Some("x")])],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::String), &p, &insert).is_err());
        let null = RowChanges {
            updates: vec![(row(&["old"]), row(&[NULL_CELL]))],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::String), &p, &null).is_err());
    }

    #[test]
    fn console_edits_follow_the_grid_shown() {
        // `LRANGE k 5 6` shows #5 and #6.
        let grid = value_grid(&RedisValue::List(vec![b"f".to_vec(), b"g".to_vec()]), 5);
        assert_eq!(grid.rows[0][0], "5");
        let page = console_page(&RedisType::List, &grid).unwrap();
        assert_eq!(page_offset(&page), 5);
        let changes = RowChanges {
            updates: vec![(row(&["6", "g"]), row(&["6", "G"]))],
            deletes: vec![row(&["5", "f"])],
            ..Default::default()
        };
        assert_eq!(
            console_edits(b"k", &RedisType::List, &grid, &changes).unwrap(),
            [
                KeyEdit::ListSet {
                    index: 6,
                    old: b"g".to_vec(),
                    value: b"G".to_vec()
                },
                KeyEdit::ListRemove {
                    index: 5,
                    old: b"f".to_vec()
                },
            ]
        );
        // The row of a string deleted: the key.
        let get = value_grid(&RedisValue::String(b"v".to_vec()), 0);
        let delete = RowChanges {
            deletes: vec![row(&["v"])],
            ..Default::default()
        };
        assert_eq!(
            console_edits(b"k", &RedisType::String, &get, &delete).unwrap(),
            [KeyEdit::Delete]
        );
        let hash = value_grid(&RedisValue::Hash(vec![(b"f".to_vec(), b"1".to_vec())]), 0);
        let set = RowChanges {
            updates: vec![(row(&["f", "1"]), row(&["f", "2"]))],
            ..Default::default()
        };
        assert_eq!(
            console_edits(b"k", &RedisType::Hash, &hash, &set).unwrap(),
            [KeyEdit::HSet {
                field: b"f".to_vec(),
                value: b"2".to_vec()
            }]
        );
        assert!(console_page(&RedisType::Other("ReJSON-RL".into()), &hash).is_err());
    }

    #[test]
    fn grid_edits_of_a_list() {
        // Second page: elements 2..4 of 6.
        let items = vec![b"c".to_vec(), b"d".to_vec(), b"e".to_vec()];
        let p = page(RedisValue::List(items), 6, Some(5));
        let changes = RowChanges {
            updates: vec![(row(&["3", "d"]), row(&["3", "D"]))],
            deletes: vec![row(&["2", "c"]), row(&["4", "e"])],
            inserts: vec![new_row(&[None, Some("z")])],
        };
        assert_eq!(
            grid_edits(&info(RedisType::List), &p, &changes).unwrap(),
            [
                KeyEdit::ListSet {
                    index: 3,
                    old: b"d".to_vec(),
                    value: b"D".to_vec()
                },
                KeyEdit::ListRemove {
                    index: 4,
                    old: b"e".to_vec()
                },
                KeyEdit::ListRemove {
                    index: 2,
                    old: b"c".to_vec()
                },
                KeyEdit::ListPush {
                    left: false,
                    value: b"z".to_vec()
                },
            ]
        );
        // Rows numbered from 0 (value_result) are found too.
        let relative = RowChanges {
            updates: vec![(row(&["1", "d"]), row(&["1", "D"]))],
            ..Default::default()
        };
        assert_eq!(
            grid_edits(&info(RedisType::List), &p, &relative).unwrap(),
            [KeyEdit::ListSet {
                index: 3,
                old: b"d".to_vec(),
                value: b"D".to_vec()
            }]
        );
        // A stale row is refused.
        let stale = RowChanges {
            deletes: vec![row(&["3", "zz"])],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::List), &p, &stale).is_err());
        let renumber = RowChanges {
            updates: vec![(row(&["3", "d"]), row(&["9", "d"]))],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::List), &p, &renumber).is_err());
    }

    #[test]
    fn grid_edits_of_sets_zsets_and_hashes() {
        let set = page(RedisValue::Set(vec![b"a".to_vec(), b"b".to_vec()]), 2, None);
        let changes = RowChanges {
            updates: vec![(row(&["a"]), row(&["A"]))],
            deletes: vec![row(&["b"])],
            inserts: vec![new_row(&[Some("c")])],
        };
        assert_eq!(
            grid_edits(&info(RedisType::Set), &set, &changes).unwrap(),
            [
                KeyEdit::SetRemove(b"a".to_vec()),
                KeyEdit::SetAdd(b"A".to_vec()),
                KeyEdit::SetRemove(b"b".to_vec()),
                KeyEdit::SetAdd(b"c".to_vec()),
            ]
        );
        let z = page(
            RedisValue::ZSet(vec![(b"m".to_vec(), 1.0), (b"n".to_vec(), 2.0)]),
            2,
            None,
        );
        let changes = RowChanges {
            updates: vec![
                (row(&["m", "1"]), row(&["m", "5"])),
                (row(&["n", "2"]), row(&["o", "2"])),
            ],
            deletes: vec![],
            inserts: vec![
                new_row(&[Some("p"), None]),
                new_row(&[Some("q"), Some("-inf")]),
            ],
        };
        assert_eq!(
            grid_edits(&info(RedisType::ZSet), &z, &changes).unwrap(),
            [
                KeyEdit::ZAdd {
                    member: b"m".to_vec(),
                    score: 5.0
                },
                KeyEdit::ZRemove(b"n".to_vec()),
                KeyEdit::ZAdd {
                    member: b"o".to_vec(),
                    score: 2.0
                },
                KeyEdit::ZAdd {
                    member: b"p".to_vec(),
                    score: 0.0
                },
                KeyEdit::ZAdd {
                    member: b"q".to_vec(),
                    score: f64::NEG_INFINITY
                },
            ]
        );
        let bad = RowChanges {
            updates: vec![(row(&["m", "1"]), row(&["m", "x"]))],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::ZSet), &z, &bad).is_err());
        let h = page(
            RedisValue::Hash(vec![(b"f".to_vec(), b"v".to_vec())]),
            1,
            None,
        );
        let changes = RowChanges {
            updates: vec![(row(&["f", "v"]), row(&["g", "w"]))],
            deletes: vec![row(&["x", "y"])],
            inserts: vec![new_row(&[Some("h"), None])],
        };
        assert_eq!(
            grid_edits(&info(RedisType::Hash), &h, &changes).unwrap(),
            [
                KeyEdit::HDel(b"f".to_vec()),
                KeyEdit::HSet {
                    field: b"g".to_vec(),
                    value: b"w".to_vec()
                },
                KeyEdit::HDel(b"x".to_vec()),
                KeyEdit::HSet {
                    field: b"h".to_vec(),
                    value: vec![]
                },
            ]
        );
        let no_field = RowChanges {
            inserts: vec![new_row(&[None, Some("v")])],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::Hash), &h, &no_field).is_err());
    }

    #[test]
    fn grid_edits_of_a_stream() {
        let s = page(
            RedisValue::Stream(vec![StreamEntry {
                id: "1-0".into(),
                fields: vec![],
            }]),
            1,
            None,
        );
        let changes = RowChanges {
            updates: vec![],
            deletes: vec![row(&["1-0", "a 1"])],
            inserts: vec![
                new_row(&[None, Some(r#"name "Ada L""#)]),
                new_row(&[Some("9-1"), Some("a 1")]),
            ],
        };
        assert_eq!(
            grid_edits(&info(RedisType::Stream), &s, &changes).unwrap(),
            [
                KeyEdit::XDel("1-0".into()),
                KeyEdit::XAdd {
                    id: None,
                    fields: vec![(b"name".to_vec(), b"Ada L".to_vec())]
                },
                KeyEdit::XAdd {
                    id: Some("9-1".into()),
                    fields: vec![(b"a".to_vec(), b"1".to_vec())]
                },
            ]
        );
        let odd = RowChanges {
            inserts: vec![new_row(&[None, Some("a")])],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::Stream), &s, &odd).is_err());
        let update = RowChanges {
            updates: vec![(row(&["1-0", "a 1"]), row(&["1-0", "a 2"]))],
            ..Default::default()
        };
        assert!(grid_edits(&info(RedisType::Stream), &s, &update).is_err());
    }

    #[test]
    fn units_are_lines() {
        let text = "GET a\r\n\n  # comment\n// other\n  SET b \"x y\"  \nPING";
        let u = units(text);
        let got: Vec<&str> = u.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(got, ["GET a", "SET b \"x y\"", "PING"]);
        assert!(units("").is_empty());
    }

    #[test]
    fn warnings_of_the_selection() {
        let text = "GET a\nFLUSHDB\nCONFIG SET x 1\nKEYS *";
        let all = warnings(text, None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].unit, "FLUSHDB");
        assert!(all[0].reason.contains("base courante"));
        // Only the first line.
        assert!(warnings(text, Some(0..5)).is_empty());
        // A cursor (empty range) on the second line.
        assert_eq!(warnings(text, Some(8..8)).len(), 1);
        // Unparsable lines are the run's business.
        assert!(warnings("FLUSHALL \"", None).is_empty());
    }

    #[test]
    fn key_reads_are_editable() {
        let a = |l: &str| tokenize(l).unwrap();
        let bulk = |s: &str| Value::BulkString(s.as_bytes().to_vec());
        assert_eq!(
            key_read(&a("GET k"), &bulk("v")),
            Some((RedisValue::String(b"v".to_vec()), 0, None))
        );
        assert_eq!(key_read(&a("GET k"), &Value::Nil), None);
        assert_eq!(
            key_read(&a("LRANGE k 5 9"), &Value::Array(vec![bulk("x")])),
            Some((RedisValue::List(vec![b"x".to_vec()]), 5, None))
        );
        assert_eq!(key_read(&a("LRANGE k -5 -1"), &Value::Array(vec![])), None);
        assert_eq!(
            key_read(
                &a("ZRANGE k 0 -1 WITHSCORES"),
                &Value::Array(vec![bulk("m"), bulk("2")])
            ),
            Some((RedisValue::ZSet(vec![(b"m".to_vec(), 2.0)]), 0, None))
        );
        assert_eq!(
            key_read(&a("ZRANGE k 0 -1"), &Value::Array(vec![bulk("m")])),
            None
        );
        assert_eq!(
            key_read(
                &a("SSCAN k 0 COUNT 100"),
                &Value::Array(vec![bulk("12"), Value::Array(vec![bulk("m")])])
            ),
            Some((RedisValue::Set(vec![b"m".to_vec()]), 0, Some(12)))
        );
        assert_eq!(
            key_read(
                &a("XRANGE k - + COUNT 100"),
                &Value::Array(vec![Value::Array(vec![
                    bulk("1-0"),
                    Value::Array(vec![bulk("a"), bulk("1")])
                ])])
            ),
            Some((
                RedisValue::Stream(vec![StreamEntry {
                    id: "1-0".into(),
                    fields: vec![(b"a".to_vec(), b"1".to_vec())]
                }]),
                0,
                None
            ))
        );
        assert_eq!(
            key_read(&a("HGETALL k"), &Value::Array(vec![bulk("f"), bulk("v")])),
            Some((
                RedisValue::Hash(vec![(b"f".to_vec(), b"v".to_vec())]),
                0,
                None
            ))
        );
        assert_eq!(key_read(&a("MGET a b"), &Value::Array(vec![])), None);
        // Names only: read-only (no value shape).
        let names = Value::Array(vec![
            bulk("0"),
            Value::Array(vec![bulk("f1"), bulk("f2"), bulk("f3")]),
        ]);
        assert_eq!(key_read(&a("HSCAN k 0 NOVALUES"), &names), None);
        assert_eq!(key_read(&a("ZSCAN k 0 NOSCORES"), &names), None);
        assert!(
            key_read(&a("HSCAN k 0"), &names).is_none(),
            "odd: not pairs"
        );
    }

    #[test]
    fn value_commands_write_each_type() {
        let cmds = value_commands(b"k", &RedisValue::ZSet(vec![(b"m".to_vec(), 1.0)])).unwrap();
        assert_eq!(strs(cmds[0].clone()), ["ZADD", "k", "1", "m"]);
        let big = RedisValue::List((0..2500).map(|i| i.to_string().into_bytes()).collect());
        assert_eq!(value_commands(b"k", &big).unwrap().len(), 3);
        assert!(value_commands(b"k", &RedisValue::Set(vec![])).is_err());
        assert!(value_commands(b"k", &RedisValue::String(vec![])).is_ok());
        let s = RedisValue::Stream(vec![StreamEntry {
            id: String::new(),
            fields: vec![(b"a".to_vec(), b"1".to_vec())],
        }]);
        assert_eq!(
            strs(value_commands(b"k", &s).unwrap()[0].clone()),
            ["XADD", "k", "*", "a", "1"]
        );
    }

    #[tokio::test]
    async fn offline_errors_are_french() {
        let conn = crate::engine::ops::test_support::redis_offline();
        let c = conn.redis().unwrap();
        let err = test(c).await.unwrap_err().to_string();
        assert!(err.starts_with("Redis : "), "{err}");
        let mut scope = NoSqlScope {
            database: "0".into(),
        };
        let out = run_script(c, "PING", None, &mut scope, 10).await;
        assert!(out[0].outcome.result.is_err());
        // Refused before connecting.
        let out = run_script(c, "MONITOR\nPING", None, &mut scope, 10).await;
        assert_eq!(out.len(), 1);
        assert!(out[0]
            .outcome
            .result
            .as_ref()
            .unwrap_err()
            .contains("MONITOR"));
        let out = run_script(c, "GET \"open", None, &mut scope, 10).await;
        assert!(out[0]
            .outcome
            .result
            .as_ref()
            .unwrap_err()
            .contains("Guillemets"));
    }
}
