use anyhow::Result;
use futures_util::TryStreamExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config, QueryItem, QueryStream};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::engine::models::{
    Column, ConnectionConfig, DatabaseType, ForeignKeyInfo, IncomingForeignKey, IndexInfo,
    QueryResult, SchemaInfo, NULL_CELL,
};

use crate::engine::sql::statements::split_statements;

use super::utils::{
    group_foreign_keys, group_incoming_foreign_keys, group_indexes, group_tables_by_schema, is_dml,
    leading_keyword, skip_leading_comments, split_qualified,
};

/// A tiberius client over TCP.
pub type TdsClient = Client<Compat<TcpStream>>;

/// The connection's single tiberius client, shared by every operation and
/// used by one at a time.
///
/// An operation interrupted between sending its batch and reading the whole
/// response (task aborted by a GUI cancel, timeout…) leaves the client out of
/// step with the server: the next query would first wait for the abandoned
/// batch to finish (and a transaction it opened would then commit or stay
/// open). `lock` therefore marks the client dirty until the operation calls
/// `settle`; a client still dirty at the next `lock` is dropped (the server
/// rolls back the session's transaction) and replaced by a new connection
/// made from the saved configuration (Azure included, token re-acquired).
#[derive(Clone)]
pub struct SqlServerClient(Arc<Shared>);

struct Shared {
    config: ConnectionConfig,
    slot: Mutex<Slot>,
    /// Sessions opened so far (the first one and each reconnection).
    opened: Arc<AtomicU64>,
}

struct Slot {
    /// `None` after a failed reconnection.
    client: Option<TdsClient>,
    dirty: bool,
}

/// Exclusive use of the client, from `SqlServerClient::lock`.
pub struct ClientGuard<'a> {
    slot: MutexGuard<'a, Slot>,
}

impl SqlServerClient {
    pub fn new(config: ConnectionConfig, client: TdsClient) -> Self {
        Self::counted(config, client, Arc::default())
    }

    /// A client adding its sessions (`client` and each reconnection) to
    /// `opened`.
    pub fn counted(config: ConnectionConfig, client: TdsClient, opened: Arc<AtomicU64>) -> Self {
        opened.fetch_add(1, Ordering::SeqCst);
        Self(Arc::new(Shared {
            config,
            slot: Mutex::new(Slot {
                client: Some(client),
                dirty: false,
            }),
            opened,
        }))
    }

    /// Wait for the client, reconnecting first if the previous operation was
    /// interrupted. The client is marked dirty until `ClientGuard::settle`.
    pub async fn lock(&self) -> Result<ClientGuard<'_>> {
        let mut slot = self.0.slot.lock().await;
        if slot.dirty || slot.client.is_none() {
            if slot.dirty {
                tracing::warn!("SQL Server: previous operation interrupted, reconnecting");
            }
            // Closing the old connection makes the server roll back its work.
            slot.client = None;
            slot.client = Some(reopen(&self.0.config).await?);
            self.0.opened.fetch_add(1, Ordering::SeqCst);
        }
        slot.dirty = true;
        Ok(ClientGuard { slot })
    }
}

/// A new client for `config`, with the authentication of its database type,
/// for a reconnection or a dedicated session: never a browser sign-in (it
/// would block the operations waiting for the client, with no way to
/// cancel it).
pub async fn reopen(config: &ConnectionConfig) -> Result<TdsClient> {
    match config.db_type {
        DatabaseType::Azure => super::azure::open(config, false).await,
        _ => open(config).await,
    }
}

impl ClientGuard<'_> {
    /// The operation is over: unless it failed with an I/O or protocol error
    /// (the response may be partly unread), the client is in step again.
    /// A server error (bad SQL, constraint…) is reported after the request
    /// was answered, so the client stays usable.
    pub fn settle<T>(mut self, outcome: &Result<T>) {
        self.slot.dirty = match outcome {
            Ok(_) => false,
            Err(e) => !is_server_error(e),
        };
    }
}

impl std::ops::Deref for ClientGuard<'_> {
    type Target = TdsClient;

    fn deref(&self) -> &TdsClient {
        self.slot.client.as_ref().expect("connected by lock")
    }
}

impl std::ops::DerefMut for ClientGuard<'_> {
    fn deref_mut(&mut self) -> &mut TdsClient {
        self.slot.client.as_mut().expect("connected by lock")
    }
}

/// Whether `e` is an error reported by the server (as opposed to I/O,
/// protocol or decoding errors).
fn is_server_error(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<tiberius::error::Error>(),
        Some(tiberius::error::Error::Server(_))
    )
}

/// Open a client with SQL Server authentication.
pub async fn open(config: &ConnectionConfig) -> Result<TdsClient> {
    let mut tib_config = Config::new();
    tib_config.host(config.host.as_deref().unwrap_or("localhost"));
    tib_config.port(config.port.unwrap_or(1433));
    tib_config.database(&config.database);
    tib_config.authentication(AuthMethod::sql_server(
        config.username.as_deref().unwrap_or("sa"),
        config.password.as_deref().unwrap_or(""),
    ));
    tib_config.trust_cert();

    let tcp = TcpStream::connect(tib_config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Ok(Client::connect(tib_config, tcp.compat_write()).await?)
}

/// Connect to SQL Server
pub async fn connect(config: &ConnectionConfig) -> Result<SqlServerClient> {
    Ok(SqlServerClient::new(config.clone(), open(config).await?))
}

/// A result column as the app describes it.
fn column_of(c: &tiberius::Column) -> Column {
    Column {
        name: c.name().to_string(),
        type_name: format!("{:?}", c.column_type()),
        nullable: true,
        is_primary_key: false,
    }
}

/// Convert fetched rows into a `QueryResult`.
fn rows_to_result(rows: &[tiberius::Row]) -> QueryResult {
    if rows.is_empty() {
        return QueryResult::default();
    }

    let columns: Vec<Column> = rows[0].columns().iter().map(column_of).collect();

    let data: Vec<Vec<String>> = rows
        .iter()
        .map(|row| (0..columns.len()).map(|i| get_value(row, i)).collect())
        .collect();

    QueryResult {
        columns,
        rows: data,
        rows_affected: rows.len() as u64,
        ..QueryResult::default()
    }
}

/// Whether `stmt` contains the word `OUTPUT` outside string literals, quoted
/// identifiers and comments. `INSERT/UPDATE/DELETE/MERGE ... OUTPUT` returns
/// rows, so it must keep the row-fetching path.
fn has_output_clause(stmt: &str) -> bool {
    has_word(stmt, "OUTPUT")
}

/// Whether `stmt` contains `word` (any case) outside string literals, quoted
/// identifiers and comments.
fn has_word(stmt: &str, word: &str) -> bool {
    code_tokens(stmt)
        .iter()
        .any(|t| t.eq_ignore_ascii_case(word))
}

/// The tokens of `stmt`, comments left out: words (`@var`, `#temp`
/// included), each string literal or quoted identifier whole, and every
/// other non-blank character on its own.
fn code_tokens(stmt: &str) -> Vec<&str> {
    let is_word_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$');
    let mut tokens = Vec::new();
    let mut rest = stmt;
    while let Some(c) = rest.chars().next() {
        let quote_end = match c {
            '\'' => Some('\''),
            '"' => Some('"'),
            '[' => Some(']'),
            _ => None,
        };
        let len = if let Some(end) = quote_end {
            rest[1..].find(end).map_or(rest.len(), |at| at + 2)
        } else if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
            continue;
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
            continue;
        } else if is_word_char(c) {
            rest.find(|ch| !is_word_char(ch)).unwrap_or(rest.len())
        } else {
            c.len_utf8()
        };
        if !c.is_whitespace() {
            tokens.push(&rest[..len]);
        }
        rest = &rest[len..];
    }
    tokens
}

/// Transaction control or variable handling: statements that return no rows.
fn is_rowless_control(stmt: &str) -> bool {
    let keyword = leading_keyword(stmt);
    let after = &skip_leading_comments(stmt)[keyword.len()..];
    match keyword.as_str() {
        "COMMIT" | "ROLLBACK" | "SAVE" | "DECLARE" => true,
        "BEGIN" => matches!(
            leading_keyword(after).as_str(),
            "TRAN" | "TRANSACTION" | "DISTRIBUTED"
        ),
        "SET" => after.trim_start().starts_with('@'),
        _ => false,
    }
}

/// Whether running `statements` can only report affected-row counts, never
/// rows: at least one DML statement without `OUTPUT`, the others transaction
/// control or variables. tiberius' `QueryStream` (`simple_query`) drops the
/// DONE row counts, so such batches go through `Client::execute`, which
/// reports them.
fn counts_only<S: AsRef<str>>(statements: &[S]) -> bool {
    let mut any_dml = false;
    for stmt in statements.iter().map(AsRef::as_ref) {
        if is_dml(stmt) && !has_output_clause(stmt) {
            any_dml = true;
        } else if !is_rowless_control(stmt) {
            return false;
        }
    }
    any_dml
}

/// Run `batch` with `Client::execute` and sum the affected-row counts. It goes
/// through `sp_executesql`: the whole text is still one batch, so variables
/// declared in it stay visible across its statements.
///
/// Under `SET NOCOUNT ON` the server reports no counts (tiberius then sees
/// zeros): a zero total is checked against the session's `NOCOUNT` and the
/// count is then flagged unknown rather than shown as "0 rows".
async fn execute_counting(client: &mut TdsClient, batch: &str) -> Result<QueryResult> {
    let affected = client.execute(batch, &[]).await?.total();
    let count_unknown = affected == 0 && {
        let rows = run_batch(client, "SELECT CAST(@@OPTIONS & 512 AS int)").await?;
        let nocount: Option<i32> = rows
            .first()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get(0));
        nocount.is_some_and(|bit| bit != 0)
    };
    Ok(QueryResult {
        rows_affected: affected,
        count_unknown,
        ..QueryResult::default()
    })
}

/// The result of a batch that returned no result set and whose affected-row
/// counts tiberius' `QueryStream` does not report.
fn executed() -> QueryResult {
    QueryResult {
        count_unknown: true,
        ..QueryResult::default()
    }
}

/// Execute a query on SQL Server, keeping at most `max_rows` rows of the first
/// result set (all when `None`). DML that returns no rows goes through
/// `Client::execute` so that `rows_affected` is the real count.
pub async fn execute_query_limited(
    client: &SqlServerClient,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    let statements: Vec<String> = split_statements(query)
        .into_iter()
        .map(|(_, _, stmt)| stmt)
        .collect();
    let mut client = client.lock().await?;
    let outcome = async {
        if counts_only(&statements) {
            return execute_counting(&mut client, query).await;
        }
        let stream = client.simple_query(query).await?;
        let first = result_sets(stream, max_rows, true)
            .await?
            .into_iter()
            .next();
        Ok(first.unwrap_or_else(executed))
    }
    .await;
    client.settle(&outcome);
    outcome
}

/// Join transaction statements into a single T-SQL batch. Running the whole
/// block as ONE batch keeps batch-scoped constructs (e.g. `DECLARE @var`)
/// visible across all statements — running them as separate batches would drop
/// the variables between statements.
///
/// Unless the block sets `XACT_ABORT` itself, it runs with `XACT_ABORT ON`
/// (`with_xact_abort`): otherwise most errors (constraint violations…) only
/// end their statement, the batch goes on and its `COMMIT` commits the
/// statements that succeeded. `execute_transaction` then restores the
/// session's own setting.
fn build_tsql_batch(statements: &[String]) -> String {
    let block = statements.join(";\n");
    if sets_xact_abort(statements) {
        block
    } else {
        with_xact_abort(&block)
    }
}

/// Execute a transaction block as one T-SQL batch. The single tiberius client
/// is locked for the whole block (atomicity, no interleaving). Statements
/// include the user's `BEGIN`/`COMMIT`/`ROLLBACK`. If the batch fails, any
/// transaction it left open is rolled back and the error is returned.
pub async fn execute_transaction(
    client: &SqlServerClient,
    statements: &[String],
) -> Result<QueryResult> {
    let batch = build_tsql_batch(statements);
    let mut client = client.lock().await?;
    let original = if sets_xact_abort(statements) {
        None
    } else {
        match xact_abort_on(&mut client).await {
            Ok(on) => Some(on),
            Err(e) => {
                let failed = Err(e);
                client.settle(&failed);
                return failed;
            }
        }
    };

    let outcome = async {
        if counts_only(statements) && counts_safely(statements) {
            return execute_counting(&mut client, &batch).await;
        }
        let stream = client.simple_query(&batch).await?;
        let results = stream.into_results().await?;
        // Return the last result set that produced rows (typically the final
        // SELECT); otherwise an empty result.
        let rows = results.into_iter().rev().find(|r| !r.is_empty());
        Ok(rows.map_or_else(executed, |rows| rows_to_result(&rows)))
    }
    .await;
    finish(client, outcome, true, original).await
}

/// Whether `stmt` opens a transaction (`BEGIN TRAN[SACTION]`,
/// `BEGIN DISTRIBUTED …`), as opposed to a `BEGIN … END` block or
/// `BEGIN TRY`.
pub(crate) fn is_tsql_transaction_start(stmt: &str) -> bool {
    leading_keyword(stmt) == "BEGIN" && is_rowless_control(stmt)
}

/// Whether `statements` set `XACT_ABORT` themselves: the user's setting is
/// then left alone.
fn sets_xact_abort(statements: &[String]) -> bool {
    statements.iter().any(|s| {
        let tokens = code_tokens(s);
        tokens
            .windows(2)
            .any(|w| w[0].eq_ignore_ascii_case("SET") && w[1].eq_ignore_ascii_case("XACT_ABORT"))
    })
}

/// Whether a script batch run in auto-commit mode must run with
/// `XACT_ABORT ON` (`with_xact_abort`): it opens a transaction (otherwise
/// most errors only end their statement and the batch's `COMMIT` would
/// commit the statements that succeeded) and does not set `XACT_ABORT`
/// itself. A batch starting with `CREATE`/`ALTER` (a procedure, a view…
/// must come first in its batch) is left alone.
fn needs_xact_abort(statements: &[String]) -> bool {
    let first = statements.first().map(|s| leading_keyword(s));
    statements.iter().any(|s| is_tsql_transaction_start(s))
        && !sets_xact_abort(statements)
        && !matches!(first.as_deref(), Some("CREATE" | "ALTER"))
}

/// `batch` with `XACT_ABORT ON` set on its first line, so the line numbers
/// the server reports are unchanged. Nothing is appended: an early `RETURN`
/// would skip it. The caller restores the session's setting afterwards, in
/// a separate request (`finish`).
fn with_xact_abort(batch: &str) -> String {
    format!("SET XACT_ABORT ON; {batch}")
}

/// Whether the session has `XACT_ABORT` on.
async fn xact_abort_on(client: &mut TdsClient) -> Result<bool> {
    let rows = run_batch(client, "SELECT CAST(@@OPTIONS & 16384 AS int)").await?;
    let bit: Option<i32> = rows
        .first()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(0));
    Ok(bit.is_some_and(|bit| bit != 0))
}

/// What to send after a batch run on the shared client: on a server error
/// with `rollback`, roll back the transaction it may have left open (so
/// later queries aren't poisoned); when it ran with `XACT_ABORT ON` forced
/// over a session that had it off (`original` `Some(false)`), turn it off
/// again. Other errors may leave the response unread: the client is then
/// replaced (the server rolls back), nothing is sent. If the cleanup fails
/// too, the client is left dirty and replaced on next use.
async fn finish<T>(
    mut client: ClientGuard<'_>,
    outcome: Result<T>,
    rollback: bool,
    original: Option<bool>,
) -> Result<T> {
    let server_error = outcome.as_ref().err().is_some_and(is_server_error);
    if outcome.is_err() && !server_error {
        client.settle(&outcome);
        return outcome;
    }
    let mut cleanup = String::new();
    if server_error && rollback {
        cleanup.push_str("IF @@TRANCOUNT > 0 ROLLBACK;");
    }
    if original == Some(false) {
        cleanup.push_str("SET XACT_ABORT OFF;");
    }
    if cleanup.is_empty() {
        client.settle(&outcome);
    } else {
        let done = run_batch(&mut client, &cleanup).await.map(drop);
        client.settle(&done);
    }
    outcome
}

/// Whether `statements` can go through `execute_counting`
/// (`sp_executesql`), which fails (error 266) when it returns with another
/// transaction count than it started with: the batch must end as many
/// transactions as it opens, and not `ROLLBACK`, which ends all of them,
/// the caller's too. Then the count is unchanged whatever it was on entry
/// (a manual transaction, a `BEGIN TRAN` in an earlier batch).
fn counts_safely(statements: &[String]) -> bool {
    let mut balance = 0i32;
    for stmt in statements {
        let tokens = code_tokens(stmt);
        if tokens.iter().any(|t| t.eq_ignore_ascii_case("ROLLBACK")) {
            return false;
        }
        match leading_keyword(stmt).as_str() {
            "BEGIN" if is_tsql_transaction_start(stmt) => balance += 1,
            "COMMIT" => balance -= 1,
            _ => {}
        }
    }
    balance == 0
}

/// Read the result sets of `stream` (only the first one when `first_only`),
/// keeping at most `max_rows` rows of each (`truncated` set when more were
/// available). An empty result set keeps its columns.
///
/// The stream is always read to the end, extra rows being discarded right
/// away (only the kept rows use memory). Dropping a partially read
/// `QueryStream` would leave the rest of the response on the wire: tiberius
/// then flushes it packet by packet at the start of the next query on this
/// shared client, logging a warning for every packet, and a later error in the
/// batch would surface on that unrelated query. Draining here keeps the client
/// clean and reports errors where they belong.
async fn result_sets(
    mut stream: QueryStream<'_>,
    max_rows: Option<usize>,
    first_only: bool,
) -> Result<Vec<QueryResult>> {
    let mut sets: Vec<QueryResult> = Vec::new();
    let mut skipping = false;
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(_) if first_only && !sets.is_empty() => skipping = true,
            QueryItem::Metadata(meta) => sets.push(QueryResult {
                columns: meta.columns().iter().map(column_of).collect(),
                ..QueryResult::default()
            }),
            QueryItem::Row(_) if skipping => {}
            QueryItem::Row(row) => {
                let Some(set) = sets.last_mut() else {
                    continue;
                };
                if max_rows.is_some_and(|max| set.rows.len() >= max) {
                    set.truncated = true;
                } else {
                    let values = (0..set.columns.len()).map(|i| get_value(&row, i));
                    set.rows.push(values.collect());
                }
            }
        }
    }
    for set in &mut sets {
        set.rows_affected = set.rows.len() as u64;
    }
    Ok(sets)
}

/// Run one batch of a script in a single round trip, as SSMS does: its
/// variables are visible to all its statements. Returns one result per
/// result set (each capped at `max_rows`), or, when it returns none, one
/// result with the affected-row count (counted only for DML-only batches,
/// see `counts_only` and `counts_safely`; flagged unknown otherwise).
///
/// `auto`: auto-commit mode, on the connection's shared client. A batch
/// opening a transaction then runs with `XACT_ABORT ON`
/// (`needs_xact_abort`), the session's setting restored afterwards, and,
/// when it fails, a transaction it left open is rolled back (`finish`). In
/// manual transaction mode (`auto` false) the batch is sent as is and the
/// open transaction is the user's: it is left as the server left it.
pub async fn execute_batch(
    client: &SqlServerClient,
    batch: &str,
    max_rows: Option<usize>,
    auto: bool,
) -> Result<Vec<QueryResult>> {
    let statements: Vec<String> = split_statements(batch)
        .into_iter()
        .map(|(_, _, stmt)| stmt)
        .collect();
    let guard = auto && needs_xact_abort(&statements);
    let guarded = guard.then(|| with_xact_abort(batch));
    let text = guarded.as_deref().unwrap_or(batch);
    let mut client = client.lock().await?;
    let original = if guard {
        match xact_abort_on(&mut client).await {
            Ok(on) => Some(on),
            Err(e) => {
                let failed = Err(e);
                client.settle(&failed);
                return failed;
            }
        }
    } else {
        None
    };
    let outcome = async {
        if counts_only(&statements) && counts_safely(&statements) {
            return Ok(vec![execute_counting(&mut client, text).await?]);
        }
        let stream = client.simple_query(text).await?;
        let mut sets = result_sets(stream, max_rows, false).await?;
        if sets.is_empty() {
            sets.push(executed());
        }
        Ok(sets)
    }
    .await;
    finish(client, outcome, auto, original).await
}

/// Roll back the transaction a script left open on the shared client.
/// Returns whether there was one.
pub async fn rollback_open_transaction(client: &SqlServerClient) -> Result<bool> {
    let mut client = client.lock().await?;
    let outcome = run_batch(
        &mut client,
        "IF @@TRANCOUNT > 0 BEGIN ROLLBACK; SELECT 1 END ELSE SELECT 0",
    )
    .await;
    client.settle(&outcome);
    let open: Option<i32> = outcome?
        .first()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(0));
    Ok(open == Some(1))
}

/// The message and line of the server error `e` raised by the batch text
/// itself (not by a procedure it called): the line counts from the batch's
/// first line.
pub fn batch_error_line(e: &anyhow::Error) -> Option<(String, u32)> {
    match e.downcast_ref::<tiberius::error::Error>() {
        Some(tiberius::error::Error::Server(t)) if t.procedure().is_empty() && t.line() > 0 => {
            Some((t.message().to_string(), t.line()))
        }
        _ => None,
    }
}

/// Whether a batch statement always returns one result set: a `SELECT`
/// that neither assigns variables (`SELECT TOP 1 @x = …`, `@y += …`
/// anywhere: a comparison `@v = 1` is taken for one too, which only loses
/// the match) nor fills a table (`SELECT … INTO`).
pub(crate) fn is_result_select(stmt: &str) -> bool {
    if leading_keyword(stmt) != "SELECT" {
        return false;
    }
    let tokens = code_tokens(stmt);
    let assigns = tokens.windows(3).any(|w| {
        let variable = w[0].starts_with('@') && !w[0].starts_with("@@");
        let compound = w[1].len() == 1 && "+-*/%&|^".contains(w[1]) && w[2] == "=";
        variable && (w[1] == "=" || compound)
    });
    !assigns && !tokens.iter().any(|t| t.eq_ignore_ascii_case("INTO"))
}

/// Whether `batch` holds control flow (`IF`, `ELSE`, `WHILE`, `BEGIN … END`
/// blocks, `TRY` / `CATCH`, `GOTO`, `RETURN`…): which of its statements
/// run, and so which `SELECT` produced which result set, is then unknown.
/// `BEGIN TRAN` is not control flow.
pub(crate) fn has_control_flow(batch: &str) -> bool {
    let tokens = code_tokens(batch);
    tokens.iter().enumerate().any(|(i, t)| {
        let word = t.to_ascii_uppercase();
        match word.as_str() {
            "IF" | "ELSE" | "WHILE" | "TRY" | "CATCH" | "GOTO" | "RETURN" | "BREAK"
            | "CONTINUE" => true,
            "BEGIN" => !tokens.get(i + 1).is_some_and(|next| {
                ["TRAN", "TRANSACTION", "DISTRIBUTED"]
                    .iter()
                    .any(|kw| next.eq_ignore_ascii_case(kw))
            }),
            _ => false,
        }
    })
}

/// Run `sql` as one batch and read its whole response.
async fn run_batch(client: &mut TdsClient, sql: &str) -> Result<Vec<Vec<tiberius::Row>>> {
    Ok(client.simple_query(sql).await?.into_results().await?)
}

/// Run one statement and return the number of rows it affected
/// (`@@ROWCOUNT`, which does not count the rows changed by triggers).
async fn rows_affected_by(client: &mut TdsClient, stmt: &str) -> Result<u64> {
    let results = run_batch(client, &format!("{stmt};\nSELECT @@ROWCOUNT")).await?;
    let count: Option<i32> = results
        .last()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get(0));
    Ok(count.unwrap_or(0).max(0) as u64)
}

/// Run `statements` one by one in a transaction on the shared client.
/// Statement `i` must affect exactly `expected[i]` rows when that is
/// `Some`; on a different count or any error the transaction is rolled back
/// and the error returned. Returns the total number of affected rows.
pub async fn execute_checked_batch(
    client: &SqlServerClient,
    statements: &[String],
    expected: &[Option<u64>],
) -> Result<u64> {
    let mut client = client.lock().await?;
    let outcome = async {
        run_batch(&mut client, "BEGIN TRANSACTION").await?;
        let mut total = 0;
        for (i, stmt) in statements.iter().enumerate() {
            let affected = rows_affected_by(&mut client, stmt).await?;
            if let Some(want) = expected.get(i).copied().flatten() {
                if affected != want {
                    return Err(super::utils::row_count_error(affected));
                }
            }
            total += affected;
        }
        run_batch(&mut client, "COMMIT").await?;
        Ok(total)
    }
    .await;
    let in_step = match &outcome {
        Ok(_) => true,
        // A count mismatch or a server error: the response was read.
        Err(e) => e.downcast_ref::<tiberius::error::Error>().is_none() || is_server_error(e),
    };
    if outcome.is_err() && in_step {
        let cleanup = run_batch(&mut client, "IF @@TRANCOUNT > 0 ROLLBACK").await;
        client.settle(&cleanup);
    } else {
        // Other errors may leave the response unread: the client is
        // replaced (and the server rolls the transaction back).
        client.settle(&outcome);
    }
    outcome
}

/// Get tables grouped by schema
pub async fn get_tables_by_schema(client: &SqlServerClient) -> Result<Vec<SchemaInfo>> {
    let mut client = client.lock().await?;
    let rows = async {
        let stream = client
            .simple_query(
                "SELECT TABLE_SCHEMA, TABLE_NAME FROM INFORMATION_SCHEMA.TABLES 
                 WHERE TABLE_TYPE = 'BASE TABLE' 
                 ORDER BY TABLE_SCHEMA, TABLE_NAME",
            )
            .await?;
        Ok(stream.into_first_result().await?)
    }
    .await;
    client.settle(&rows);
    let rows = rows?;
    let tuples: Vec<(String, String)> = rows
        .iter()
        .filter_map(|row| {
            let schema = row.get::<&str, _>(0)?.to_string();
            let table = row.get::<&str, _>(1)?.to_string();
            Some((schema, table))
        })
        .collect();
    Ok(group_tables_by_schema(tuples))
}

/// Rows of a metadata `query` taking the schema (`@P1`, NULL for the
/// default one) and the table name (`@P2`).
async fn query_rows(
    client: &SqlServerClient,
    query: &str,
    schema: &Option<String>,
    table: &str,
) -> Result<Vec<tiberius::Row>> {
    let mut client = client.lock().await?;
    let outcome = async {
        Ok(client
            .query(query, &[&schema.as_deref(), &table])
            .await?
            .into_first_result()
            .await?)
    }
    .await;
    client.settle(&outcome);
    outcome
}

/// `(schema, table)` of a possibly qualified/bracketed name. `None` schema:
/// the queries use the user's default schema (`SCHEMA_NAME()`).
fn split_table(table_name: &str) -> (Option<String>, String) {
    split_qualified(table_name)
}

/// Columns of a table as `(name, type, nullable, primary key)`, in order.
/// The type carries its length/precision (`nvarchar(50)`, `varchar(max)`,
/// `decimal(10,2)`) so it can be used in DDL.
async fn column_rows(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<(String, String, bool, bool)>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT c.COLUMN_NAME,
                c.DATA_TYPE + CASE
                    WHEN c.DATA_TYPE IN ('char', 'varchar', 'nchar', 'nvarchar', 'binary', 'varbinary')
                        THEN '(' + CASE WHEN c.CHARACTER_MAXIMUM_LENGTH = -1 THEN 'max'
                                        ELSE CAST(c.CHARACTER_MAXIMUM_LENGTH AS varchar(10)) END + ')'
                    WHEN c.DATA_TYPE IN ('decimal', 'numeric')
                        THEN '(' + CAST(c.NUMERIC_PRECISION AS varchar(10)) + ','
                                 + CAST(c.NUMERIC_SCALE AS varchar(10)) + ')'
                    ELSE '' END,
                c.IS_NULLABLE,
                CASE WHEN pk.COLUMN_NAME IS NOT NULL THEN 1 ELSE 0 END
         FROM INFORMATION_SCHEMA.COLUMNS c
         LEFT JOIN (
             SELECT ku.TABLE_SCHEMA, ku.TABLE_NAME, ku.COLUMN_NAME
             FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
             JOIN INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
               ON tc.CONSTRAINT_SCHEMA = ku.CONSTRAINT_SCHEMA
              AND tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME
             WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
         ) pk ON c.TABLE_SCHEMA = pk.TABLE_SCHEMA AND c.TABLE_NAME = pk.TABLE_NAME
             AND c.COLUMN_NAME = pk.COLUMN_NAME
         WHERE c.TABLE_SCHEMA = COALESCE(@P1, SCHEMA_NAME()) AND c.TABLE_NAME = @P2
         ORDER BY c.ORDINAL_POSITION";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let type_name: &str = row.try_get(1)?.unwrap_or("");
        let nullable: &str = row.try_get(2)?.unwrap_or("YES");
        let is_pk: i32 = row.try_get(3)?.unwrap_or(0);
        columns.push((
            name.to_string(),
            type_name.to_string(),
            nullable == "YES",
            is_pk == 1,
        ));
    }
    Ok(columns)
}

/// Get column nullability information for a table
pub async fn get_column_nullability(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<std::collections::HashMap<String, bool>> {
    Ok(column_rows(client, table_name)
        .await?
        .into_iter()
        .map(|(name, _, nullable, _)| (name, nullable))
        .collect())
}

/// Get primary key columns for a table, in key order
pub async fn get_primary_keys(client: &SqlServerClient, table_name: &str) -> Result<Vec<String>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT ku.COLUMN_NAME
         FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE ku
         JOIN INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
           ON tc.CONSTRAINT_SCHEMA = ku.CONSTRAINT_SCHEMA
          AND tc.CONSTRAINT_NAME = ku.CONSTRAINT_NAME
         WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
           AND ku.TABLE_SCHEMA = COALESCE(@P1, SCHEMA_NAME()) AND ku.TABLE_NAME = @P2
         ORDER BY ku.ORDINAL_POSITION";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut primary_keys = Vec::new();
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        primary_keys.push(name.to_string());
    }

    Ok(primary_keys)
}

/// Get column names for a table (for autocompletion)
#[allow(dead_code)]
pub async fn get_table_columns(client: &SqlServerClient, table_name: &str) -> Result<Vec<String>> {
    Ok(column_rows(client, table_name)
        .await?
        .into_iter()
        .map(|(name, ..)| name)
        .collect())
}

/// Get full column details for a table (for schema modification)
pub async fn get_table_column_details(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<crate::engine::models::Column>> {
    Ok(column_rows(client, table_name)
        .await?
        .into_iter()
        .map(
            |(name, type_name, nullable, is_primary_key)| crate::engine::models::Column {
                name,
                type_name,
                nullable,
                is_primary_key,
            },
        )
        .collect())
}

/// Get the indexes of a table from `sys.indexes` (heaps and INCLUDE columns
/// are left out).
pub async fn get_indexes(client: &SqlServerClient, table_name: &str) -> Result<Vec<IndexInfo>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT i.name, c.name, i.is_unique, i.is_primary_key
         FROM sys.indexes i
         JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
         JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
         JOIN sys.tables t ON t.object_id = i.object_id
         JOIN sys.schemas s ON s.schema_id = t.schema_id
         WHERE s.name = COALESCE(@P1, SCHEMA_NAME()) AND t.name = @P2
           AND i.name IS NOT NULL AND ic.is_included_column = 0
         ORDER BY i.is_primary_key DESC, i.name, ic.key_ordinal";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let column: &str = row.try_get(1)?.unwrap_or("");
        let unique: bool = row.try_get(2)?.unwrap_or(false);
        let primary: bool = row.try_get(3)?.unwrap_or(false);
        out.push((name.to_string(), column.to_string(), unique, primary));
    }
    Ok(group_indexes(out))
}

/// Get the foreign keys of a table from `sys.foreign_keys`. The referenced
/// table is always qualified with its schema (`schema.table`).
pub async fn get_foreign_keys(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<ForeignKeyInfo>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT fk.name, pc.name, rs.name, rt.name, rc.name
         FROM sys.foreign_keys fk
         JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
         JOIN sys.tables t ON t.object_id = fk.parent_object_id
         JOIN sys.schemas s ON s.schema_id = t.schema_id
         JOIN sys.columns pc
              ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
         JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id
         JOIN sys.schemas rs ON rs.schema_id = rt.schema_id
         JOIN sys.columns rc
              ON rc.object_id = fkc.referenced_object_id
             AND rc.column_id = fkc.referenced_column_id
         WHERE s.name = COALESCE(@P1, SCHEMA_NAME()) AND t.name = @P2
         ORDER BY fk.name, fkc.constraint_column_id";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let column: &str = row.try_get(1)?.unwrap_or("");
        let ref_schema: &str = row.try_get(2)?.unwrap_or("");
        let ref_table: &str = row.try_get(3)?.unwrap_or("");
        let ref_column: &str = row.try_get(4)?.unwrap_or("");
        out.push((
            name.to_string(),
            column.to_string(),
            format!("{ref_schema}.{ref_table}"),
            ref_column.to_string(),
        ));
    }
    Ok(group_foreign_keys(out))
}

/// Get the foreign keys of other tables pointing at a table, from
/// `sys.foreign_keys` + `sys.foreign_key_columns` (`referenced_object_id` =
/// the table). The referencing table is always qualified with its schema
/// (`schema.table`).
pub async fn get_incoming_foreign_keys(
    client: &SqlServerClient,
    table_name: &str,
) -> Result<Vec<IncomingForeignKey>> {
    let (schema, table) = split_table(table_name);
    let query = "SELECT fk.name, s.name, t.name, pc.name, rc.name
         FROM sys.foreign_keys fk
         JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
         JOIN sys.tables t ON t.object_id = fk.parent_object_id
         JOIN sys.schemas s ON s.schema_id = t.schema_id
         JOIN sys.columns pc
              ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
         JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id
         JOIN sys.schemas rs ON rs.schema_id = rt.schema_id
         JOIN sys.columns rc
              ON rc.object_id = fkc.referenced_object_id
             AND rc.column_id = fkc.referenced_column_id
         WHERE rs.name = COALESCE(@P1, SCHEMA_NAME()) AND rt.name = @P2
         ORDER BY s.name, t.name, fk.name, fkc.constraint_column_id";

    let rows = query_rows(client, query, &schema, &table).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: &str = row.try_get(0)?.unwrap_or("");
        let own_schema: &str = row.try_get(1)?.unwrap_or("");
        let own_table: &str = row.try_get(2)?.unwrap_or("");
        let column: &str = row.try_get(3)?.unwrap_or("");
        let ref_column: &str = row.try_get(4)?.unwrap_or("");
        out.push((
            name.to_string(),
            format!("{own_schema}.{own_table}"),
            column.to_string(),
            ref_column.to_string(),
        ));
    }
    Ok(group_incoming_foreign_keys(out))
}

/// Test the connection
pub async fn test(client: &SqlServerClient) -> Result<()> {
    let mut client = client.lock().await?;
    let outcome = async {
        client
            .simple_query("SELECT 1")
            .await?
            .into_results()
            .await?;
        Ok(())
    }
    .await;
    client.settle(&outcome);
    outcome
}

/// Text of a cell, decoded from its TDS type (so that tinyint, smallint,
/// real, uniqueidentifier… are not mistaken for NULL).
fn get_value(row: &tiberius::Row, index: usize) -> String {
    use tiberius::ColumnData;
    fn text<T: ToString>(v: &Option<T>) -> Option<String> {
        v.as_ref().map(ToString::to_string)
    }
    let Some((_, data)) = row.cells().nth(index) else {
        return NULL_CELL.to_string();
    };
    let value = match data {
        ColumnData::U8(v) => text(v),
        ColumnData::I16(v) => text(v),
        ColumnData::I32(v) => text(v),
        ColumnData::I64(v) => text(v),
        ColumnData::F32(v) => text(v),
        ColumnData::F64(v) => text(v),
        ColumnData::Bit(v) => text(v),
        ColumnData::Guid(v) => text(v),
        ColumnData::Numeric(v) => text(v),
        ColumnData::String(v) => v.as_deref().map(str::to_string),
        ColumnData::Xml(v) => v.as_deref().map(ToString::to_string),
        ColumnData::Binary(v) => v
            .as_deref()
            .map(|b| String::from_utf8_lossy(b).into_owned()),
        // Date/time types: no offset unless the column has one.
        _ => row
            .try_get::<chrono::NaiveDateTime, _>(index)
            .ok()
            .flatten()
            .map(|v| v.to_string())
            .or_else(|| {
                row.try_get::<chrono::DateTime<chrono::FixedOffset>, _>(index)
                    .ok()
                    .flatten()
                    .map(|v| v.to_rfc3339())
            })
            .or_else(|| {
                row.try_get::<chrono::NaiveDate, _>(index)
                    .ok()
                    .flatten()
                    .map(|v| v.to_string())
            })
            .or_else(|| {
                row.try_get::<chrono::NaiveTime, _>(index)
                    .ok()
                    .flatten()
                    .map(|v| v.to_string())
            }),
    };
    value.unwrap_or_else(|| NULL_CELL.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_tsql_batch, code_tokens, counts_only, counts_safely, has_control_flow,
        has_output_clause, is_result_select, is_rowless_control, is_tsql_transaction_start,
        needs_xact_abort, with_xact_abort,
    };

    fn stmts(sql: &str) -> Vec<String> {
        crate::engine::sql::statements::split_statements(sql)
            .into_iter()
            .map(|s| s.2)
            .collect()
    }

    #[test]
    fn tokens_skip_comments_and_keep_literals_whole() {
        assert_eq!(
            code_tokens("SELECT [a b], 'it''s' -- c\n/* d */ FROM t WHERE @x>=1"),
            ["SELECT", "[a b]", ",", "'it'", "'s'", "FROM", "t", "WHERE", "@x", ">", "=", "1"]
        );
    }

    #[test]
    fn transaction_start_is_begin_tran_only() {
        assert!(is_tsql_transaction_start("BEGIN TRANSACTION"));
        assert!(is_tsql_transaction_start("-- x\nbegin tran t1"));
        assert!(is_tsql_transaction_start("BEGIN DISTRIBUTED TRANSACTION"));
        assert!(!is_tsql_transaction_start("BEGIN TRY"));
        assert!(!is_tsql_transaction_start("BEGIN\nSELECT 1"));
        assert!(!is_tsql_transaction_start("COMMIT"));
    }

    #[test]
    fn batches_opening_a_transaction_abort_on_error() {
        let batch = "-- note\nBEGIN TRAN;\nINSERT INTO t VALUES (1);\nCOMMIT;";
        assert!(needs_xact_abort(&stmts(batch)));
        // Same line numbers: the setting is on the first line, nothing is
        // appended (an early RETURN would skip it).
        let guarded = with_xact_abort(batch);
        assert_eq!(guarded, format!("SET XACT_ABORT ON; {batch}"));
        assert_eq!(guarded.lines().count(), batch.lines().count());
        assert!(!needs_xact_abort(&stmts("SELECT 1")));
        assert!(!needs_xact_abort(&stmts("BEGIN TRY SELECT 1")));
        let proc = "CREATE PROCEDURE p AS BEGIN TRAN; COMMIT;";
        assert!(!needs_xact_abort(&stmts(proc)));
        // The user's own setting is left alone.
        let own = "SET XACT_ABORT OFF;\nBEGIN TRAN;\nCOMMIT;";
        assert!(!needs_xact_abort(&stmts(own)));
    }

    #[test]
    fn counted_batches_keep_the_transaction_count() {
        assert!(counts_safely(&stmts("INSERT INTO t VALUES (1)")));
        assert!(counts_safely(&stmts(
            "BEGIN TRAN; INSERT INTO t VALUES (1); COMMIT"
        )));
        assert!(!counts_safely(&stmts(
            "BEGIN TRAN; INSERT INTO t VALUES (1)"
        )));
        assert!(!counts_safely(&stmts("INSERT INTO t VALUES (1); COMMIT")));
        // ROLLBACK ends every open transaction, the caller's too.
        assert!(!counts_safely(&stmts(
            "BEGIN TRAN; UPDATE t SET a = 1; ROLLBACK"
        )));
        assert!(!counts_safely(&stmts(
            "SAVE TRAN s; UPDATE t SET a = 1; ROLLBACK TRAN s"
        )));
        assert!(counts_safely(&stmts("UPDATE t SET a = 'ROLLBACK'")));
    }

    #[test]
    fn result_selects() {
        assert!(is_result_select("SELECT * FROM t"));
        assert!(is_result_select("-- c\nselect @x AS x"));
        assert!(is_result_select("SELECT @a, @b"));
        assert!(is_result_select("SELECT 'INTO' AS w"));
        assert!(is_result_select("SELECT [into] FROM t"));
        assert!(is_result_select("SELECT a FROM t WHERE @x >= 1"));
        assert!(!is_result_select("SELECT @x = 1"));
        assert!(!is_result_select("SELECT TOP 1 @id = id FROM a"));
        assert!(!is_result_select("SELECT DISTINCT @a = x, @b = y FROM t"));
        assert!(!is_result_select("SELECT @x += n FROM t"));
        assert!(!is_result_select("SELECT * INTO #t FROM t"));
        assert!(!is_result_select("INSERT INTO t SELECT 1"));
        assert!(!is_result_select("IF 1 = 1 SELECT 1"));
        assert!(!is_result_select(
            "WITH c AS (SELECT 1 AS a) SELECT a FROM c"
        ));
    }

    #[test]
    fn control_flow() {
        for batch in [
            "IF 1 = 1 SELECT 1",
            "SELECT 1; END ELSE SELECT 2",
            "WHILE @i < 3 SET @i += 1",
            "BEGIN TRY SELECT 1 END TRY BEGIN CATCH SELECT 2 END CATCH",
            "BEGIN\nSELECT 1\nEND",
            "GOTO done",
            "RETURN",
        ] {
            assert!(has_control_flow(batch), "{batch}");
        }
        for batch in [
            "BEGIN TRAN; UPDATE t SET a = 1; COMMIT; SELECT * FROM t",
            "begin transaction x; commit",
            "BEGIN DISTRIBUTED TRANSACTION",
            "SELECT 'IF', [else], CASE WHEN a = 1 THEN 2 END FROM t -- IF\n",
            "SELECT TRY_CAST(a AS int) FROM t",
        ] {
            assert!(!has_control_flow(batch), "{batch}");
        }
    }

    #[test]
    fn detects_output_clause() {
        assert!(has_output_clause(
            "INSERT INTO t (a) OUTPUT inserted.id VALUES (1)"
        ));
        assert!(has_output_clause("delete from t output deleted.*"));
        assert!(has_output_clause("UPDATE t SET a = 1 OUTPUT"));
        assert!(!has_output_clause("UPDATE t SET a = 'OUTPUT'"));
        assert!(!has_output_clause("UPDATE t SET a = 'it''s OUTPUT'"));
        assert!(!has_output_clause("UPDATE t SET [output] = 1"));
        assert!(!has_output_clause("UPDATE t SET \"Output\" = 1"));
        assert!(!has_output_clause("UPDATE t SET a = @output"));
        assert!(!has_output_clause("UPDATE t SET outputs = 1 -- OUTPUT\n"));
        assert!(!has_output_clause("UPDATE t /* OUTPUT */ SET a = 1"));
    }

    #[test]
    fn rowless_control_statements() {
        assert!(is_rowless_control("BEGIN TRAN"));
        assert!(is_rowless_control("begin transaction"));
        assert!(is_rowless_control("COMMIT"));
        assert!(is_rowless_control("ROLLBACK TRANSACTION"));
        assert!(is_rowless_control("DECLARE @x INT = 5"));
        assert!(is_rowless_control("SET @x = 6"));
        assert!(!is_rowless_control("SET SHOWPLAN_ALL ON"));
        assert!(!is_rowless_control("BEGIN TRY"));
        assert!(!is_rowless_control("SELECT 1"));
    }

    #[test]
    fn counts_only_requires_dml_without_rows() {
        assert!(counts_only(&["UPDATE t SET a = 1"]));
        assert!(counts_only(&[
            "-- c\nMERGE t USING s ON 1 = 1 WHEN MATCHED THEN DELETE"
        ]));
        assert!(counts_only(&[
            "BEGIN TRAN",
            "DECLARE @x INT = 5",
            "DELETE FROM t WHERE a = @x",
            "COMMIT",
        ]));
        assert!(!counts_only(&[
            "INSERT INTO t OUTPUT inserted.* VALUES (1)"
        ]));
        assert!(!counts_only(&["UPDATE t SET a = 1", "SELECT * FROM t"]));
        assert!(!counts_only(&["BEGIN TRAN", "COMMIT"]));
        assert!(!counts_only(&["SELECT 1"]));
        assert!(!counts_only::<&str>(&[]));
    }

    #[test]
    fn declare_and_usage_stay_in_one_batch() {
        let stmts = [
            "BEGIN TRAN".to_string(),
            "DECLARE @x INT = 5".to_string(),
            "SELECT @x".to_string(),
            "COMMIT".to_string(),
        ];
        // The whole block must be a single batch string so that @x declared in
        // one statement stays in scope for the SELECT that uses it.
        assert_eq!(
            build_tsql_batch(&stmts),
            "SET XACT_ABORT ON; BEGIN TRAN;\nDECLARE @x INT = 5;\nSELECT @x;\nCOMMIT"
        );
        // A block setting XACT_ABORT itself is sent as is.
        let own = ["SET XACT_ABORT OFF".to_string(), "BEGIN TRAN".to_string()];
        assert_eq!(build_tsql_batch(&own), "SET XACT_ABORT OFF;\nBEGIN TRAN");
    }
}
