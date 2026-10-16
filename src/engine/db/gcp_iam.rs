//! Google Cloud IAM database authentication for Cloud SQL (PostgreSQL /
//! MySQL): the user's Google account signs in with an OAuth2 token from
//! `gcloud`, sent as the password over TLS. Tokens last about an hour, so a
//! task per pool hands it a fresh one before the current one expires.
//!
//! Tokens are secrets: never logged, never in an error message, never in a
//! `Debug` output.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use sqlx::pool::PoolOptions;
use sqlx::{Connection, Database, Pool};

use crate::engine::models::{ConnectionConfig, DatabaseType};

/// Lifetime assumed for a token whose expiry can't be read (Google access
/// tokens last one hour).
const TOKEN_LIFETIME: Duration = Duration::from_secs(55 * 60);
/// How long `gcloud` may take to answer.
const GCLOUD_TIMEOUT: Duration = Duration::from_secs(30);

/// Check the form: GcpIam needs a user (French messages).
pub fn validate(db_type: &DatabaseType, username: &str) -> Result<(), String> {
    if !matches!(db_type, DatabaseType::Postgres | DatabaseType::MySQL) {
        return Err("L'IAM Google Cloud n'est disponible que pour PostgreSQL et MySQL.".into());
    }
    let account = username.trim();
    if account.is_empty() {
        return Err("Indique ton compte Google".into());
    }
    if !account.contains('@') {
        return Err(
            "Le compte Google doit être une adresse e-mail (prenom.nom@domaine.com ou \
             compte de service)."
                .into(),
        );
    }
    if !account_chars_ok(account) {
        return Err("Le compte Google contient des caractères non autorisés.".into());
    }
    Ok(())
}

/// The database user name the IAM account must use for this engine
/// (PostgreSQL: as typed; MySQL: the part before '@'), for a form hint.
/// A service account's `.gserviceaccount.com` is dropped for PostgreSQL, as
/// Cloud SQL names its user `sa@project.iam`.
pub fn database_user(db_type: &DatabaseType, account: &str) -> String {
    let account = account.trim();
    match db_type {
        DatabaseType::MySQL => account.split('@').next().unwrap_or_default().to_string(),
        _ => {
            const SUFFIX: &str = ".gserviceaccount.com";
            let cut = account.len().saturating_sub(SUFFIX.len());
            match account.get(cut..) {
                Some(end) if end.eq_ignore_ascii_case(SUFFIX) && account.contains('@') => {
                    account[..cut].to_string()
                }
                _ => account.to_string(),
            }
        }
    }
}

/// Characters of an e-mail address: the account goes on `gcloud`'s command
/// line (through `cmd` on Windows), so nothing a shell would interpret.
fn account_chars_ok(account: &str) -> bool {
    !account.is_empty()
        && account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
}

/// The Google account of an IAM connection, checked.
pub(super) fn account(config: &ConnectionConfig) -> Result<String> {
    let account = config.username.as_deref().unwrap_or_default();
    validate(&config.db_type, account).map_err(anyhow::Error::msg)?;
    Ok(account.trim().to_string())
}

/// What to do when the server refuses the token: the IAM user is missing on
/// the instance or has no rights. `account` is the Google account.
pub fn login_hint(error: &str, db_type: &DatabaseType, account: &str) -> Option<String> {
    let m = error.to_ascii_lowercase();
    if !(m.contains("authentication failed") || m.contains("access denied")) {
        return None;
    }
    Some(format!(
        "L'utilisateur IAM doit exister sur l'instance (`gcloud sql users create … \
         --type=cloud_iam_user`) et avoir des droits (GRANT) ; nom attendu : {}",
        database_user(db_type, account)
    ))
}

// ========== Tokens ==========

/// An OAuth2 token and when it stops being valid. `Debug` hides the token.
#[derive(Clone)]
pub struct Token {
    secret: String,
    pub expires_at: Instant,
}

impl Token {
    pub fn new(secret: impl Into<String>, expires_at: Instant) -> Self {
        Self {
            secret: secret.into(),
            expires_at,
        }
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("secret", &"<masqué>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub type TokenFuture = Pin<Box<dyn Future<Output = Result<Token>> + Send>>;
/// Gives a token for a Google account (`gcloud_provider`, or a fake in
/// tests).
pub type Provider = Arc<dyn Fn(String) -> TokenFuture + Send + Sync>;

/// Tokens from `gcloud`, cached per account until shortly before expiry.
pub fn gcloud_provider() -> Provider {
    Arc::new(|account| Box::pin(async move { cached_token(&account, run_gcloud).await }))
}

/// What a `gcloud` run gave back.
#[derive(Clone, Default)]
struct CmdOutput {
    success: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// `gcloud` arguments for a Cloud SQL login token. Never `auth
/// print-access-token` instead: that token opens all of Google Cloud, and
/// `Require` TLS doesn't verify who receives it.
fn token_args(account: &str) -> Vec<String> {
    ["sql", "generate-login-token"]
        .iter()
        .map(|s| s.to_string())
        .chain([format!("--account={account}"), "--quiet".to_string()])
        .collect()
}

/// The program and arguments that run `gcloud args` (through `cmd` on
/// Windows, where `gcloud` is a script, as `azure::run_az`).
fn command_line(args: &[String], windows: bool) -> (&'static str, Vec<String>) {
    if windows {
        let mut all = vec!["/C".to_string(), "gcloud".to_string()];
        all.extend(args.iter().cloned());
        ("cmd", all)
    } else {
        ("gcloud", args.to_vec())
    }
}

async fn run_gcloud(args: Vec<String>) -> std::io::Result<CmdOutput> {
    let (program, args) = command_line(&args, cfg!(target_os = "windows"));
    let mut command = tokio::process::Command::new(program);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GCLOUD_TIMEOUT, command.output())
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    Ok(CmdOutput {
        success: output.status.success(),
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Why `gcloud` gave no token.
#[derive(Debug, PartialEq)]
enum Failure {
    NotInstalled,
    NotSignedIn,
    /// This SDK has no `sql generate-login-token`.
    UnknownCommand,
    Other(String),
}

fn classify(out: &CmdOutput) -> Failure {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let m = stderr.to_lowercase();
    // `cmd` can't find `gcloud`: 9009, "is not recognized…" (or localized).
    if out.code == Some(9009)
        || m.contains("is not recognized")
        || m.contains("n'est pas reconnu")
        || m.contains("command not found")
    {
        return Failure::NotInstalled;
    }
    if m.contains("invalid choice") && m.contains("generate-login-token") {
        return Failure::UnknownCommand;
    }
    const SIGNED_OUT: [&str; 9] = [
        "gcloud auth login",
        "does not have any valid credentials",
        "no credentialed accounts",
        "do not currently have an active account",
        "reauthentication",
        "re-authenticate",
        "invalid_grant",
        "refresh your current auth tokens",
        "not been authenticated",
    ];
    if SIGNED_OUT.iter().any(|s| m.contains(s)) {
        return Failure::NotSignedIn;
    }
    Failure::Other(short_error(&stderr, out.code))
}

/// `gcloud`'s message, short: its first line without the `ERROR: (gcloud.…)`
/// prefix.
fn short_error(stderr: &str, code: Option<i32>) -> String {
    const MAX: usize = 300;
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let line = line.strip_prefix("ERROR:").unwrap_or(line).trim_start();
    let line = match line.strip_prefix("(gcloud") {
        Some(rest) => rest
            .split_once(')')
            .map_or(line, |(_, msg)| msg.trim_start()),
        None => line,
    };
    if line.is_empty() {
        return match code {
            Some(c) => format!("code de sortie {c}"),
            None => "interrompu".into(),
        };
    }
    match line.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
    }
}

fn explain(failure: Failure, account: &str) -> anyhow::Error {
    match failure {
        Failure::NotInstalled => anyhow!(not_installed()),
        Failure::NotSignedIn => anyhow!(
            "Session Google Cloud absente ou expirée. Lance `gcloud auth login` (compte {account})."
        ),
        Failure::UnknownCommand => anyhow!(
            "Cette version de gcloud ne sait pas générer de jeton : mets à jour le SDK \
             Google Cloud (`gcloud components update`)."
        ),
        Failure::Other(msg) => anyhow!("gcloud n'a pas fourni de jeton : {msg}"),
    }
}

fn not_installed() -> &'static str {
    "gcloud introuvable : installe le SDK Google Cloud ou ajoute-le au PATH"
}

fn launch_error(e: std::io::Error) -> anyhow::Error {
    match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!(not_installed()),
        std::io::ErrorKind::TimedOut => {
            anyhow!("gcloud n'a pas répondu en {} s.", GCLOUD_TIMEOUT.as_secs())
        }
        _ => anyhow!("Impossible de lancer gcloud : {e}"),
    }
}

/// The token printed by `gcloud` (its last non-empty line). The output is
/// never quoted in the error: it could hold a token.
fn parse_token(stdout: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(stdout);
    let token = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_graphic()) {
        bail!("gcloud n'a pas renvoyé de jeton lisible.");
    }
    Ok(token.to_string())
}

/// When `token` expires: its `exp` claim when it is a JWT, else
/// `TOKEN_LIFETIME` from `now` (`now_unix` = the same instant, in seconds
/// since the epoch).
fn expiry(token: &str, now: Instant, now_unix: u64) -> Instant {
    match jwt_exp(token) {
        Some(exp) => now + Duration::from_secs(exp.saturating_sub(now_unix)),
        None => now + TOKEN_LIFETIME,
    }
}

fn jwt_exp(token: &str) -> Option<u64> {
    let mut parts = token.split('.');
    let (_, payload, _, None) = (parts.next()?, parts.next()?, parts.next()?, parts.next()) else {
        return None;
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("exp")?
        .as_u64()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A fresh token for `account` from `gcloud`, run by `run` (an SDK without
/// `sql generate-login-token` must be updated).
async fn fetch_with<R, F>(account: &str, run: R) -> Result<Token>
where
    R: Fn(Vec<String>) -> F,
    F: Future<Output = std::io::Result<CmdOutput>>,
{
    if !account_chars_ok(account) {
        bail!("Le compte Google contient des caractères non autorisés.");
    }
    tracing::info!("Requesting a Cloud SQL IAM token from gcloud for {account}");
    let out = run(token_args(account)).await.map_err(launch_error)?;
    if !out.success {
        return Err(explain(classify(&out), account));
    }
    let secret = parse_token(&out.stdout)?;
    let expires_at = expiry(&secret, Instant::now(), now_unix());
    Ok(Token::new(secret, expires_at))
}

/// How long before expiry a token is replaced.
const RENEW_BEFORE: Duration = Duration::from_secs(5 * 60);

fn tokens() -> MutexGuard<'static, HashMap<String, Token>> {
    static TOKENS: OnceLock<Mutex<HashMap<String, Token>>> = OnceLock::new();
    TOKENS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Whether a cached token can still be handed out at `now`: not within
/// `RENEW_BEFORE` of its expiry, so a renewal always gets a new one.
fn usable(token: &Token, now: Instant) -> bool {
    token.expires_at > now + RENEW_BEFORE
}

async fn cached_token<R, F>(account: &str, run: R) -> Result<Token>
where
    R: Fn(Vec<String>) -> F,
    F: Future<Output = std::io::Result<CmdOutput>>,
{
    if let Some(token) = tokens().get(account).filter(|t| usable(t, Instant::now())) {
        return Ok(token.clone());
    }
    let token = fetch_with(account, run).await?;
    tokens().insert(account.to_string(), token.clone());
    Ok(token)
}

// ========== Pool with renewed tokens ==========

/// When the renewal task acts.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// Renew this long before the token expires…
    pub renew_before: Duration,
    /// …but never sooner than this after the last renewal.
    pub min_delay: Duration,
    /// Wait before trying again after a first failed renewal, doubled
    /// after each further failure…
    pub retry: Duration,
    /// …up to this.
    pub max_retry: Duration,
}

impl Timing {
    pub const DEFAULT: Timing = Timing {
        renew_before: RENEW_BEFORE,
        min_delay: Duration::from_secs(30),
        retry: Duration::from_secs(30),
        max_retry: Duration::from_secs(5 * 60),
    };

    /// The wait after `failures` failed renewals in a row (at least 1).
    fn retry_after(&self, failures: u32) -> Duration {
        let factor = 1u32
            .checked_shl(failures.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.retry.saturating_mul(factor).min(self.max_retry)
    }

    fn delay(&self, expires_at: Instant, now: Instant) -> Duration {
        expires_at
            .saturating_duration_since(now)
            .saturating_sub(self.renew_before)
            .max(self.min_delay)
    }
}

/// Options for a token: the connection's options with the token as password.
pub(crate) type ConnectOptionsOf<DB> = <<DB as Database>::Connection as Connection>::Options;

/// A pool signed in with `account`'s token from `provider`; until the pool
/// is closed, a task gives it a new token before the current one expires
/// (connections already open stay valid: the token is only checked at
/// login).
pub(crate) async fn connect_renewing<DB, F>(
    pool: PoolOptions<DB>,
    account: String,
    provider: Provider,
    timing: Timing,
    options: F,
) -> Result<Pool<DB>>
where
    DB: Database,
    F: Fn(&Token) -> ConnectOptionsOf<DB> + Send + Sync + 'static,
{
    let token = provider(account.clone()).await?;
    let pool = pool.connect_with(options(&token)).await?;
    tokio::spawn(renew(
        pool.clone(),
        account,
        provider,
        timing,
        token.expires_at,
        options,
    ));
    Ok(pool)
}

async fn renew<DB, F>(
    pool: Pool<DB>,
    account: String,
    provider: Provider,
    timing: Timing,
    expires_at: Instant,
    options: F,
) where
    DB: Database,
    F: Fn(&Token) -> ConnectOptionsOf<DB> + Send + Sync + 'static,
{
    let mut wait = timing.delay(expires_at, Instant::now());
    let mut failures = 0u32;
    loop {
        tokio::select! {
            biased;
            _ = pool.close_event() => return,
            _ = tokio::time::sleep(wait) => {}
        }
        if pool.is_closed() {
            return;
        }
        let renewed = tokio::select! {
            biased;
            _ = pool.close_event() => return,
            r = provider(account.clone()) => r,
        };
        wait = match renewed {
            Ok(token) => {
                failures = 0;
                pool.set_connect_options(options(&token));
                tracing::debug!("Cloud SQL IAM token renewed for {account}");
                timing.delay(token.expires_at, Instant::now())
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                let wait = timing.retry_after(failures);
                // Warn once; the repeats of a lasting failure are noise.
                if failures == 1 {
                    tracing::warn!(
                        "Renewing the Cloud SQL IAM token for {account} failed ({e:#}); \
                         retrying in {wait:?}"
                    );
                } else {
                    tracing::debug!(
                        "Renewing the Cloud SQL IAM token for {account} failed again \
                         ({failures} times: {e:#}); retrying in {wait:?}"
                    );
                }
                wait
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn validate_needs_postgres_or_mysql_and_an_email() {
        assert_eq!(
            validate(&DatabaseType::Postgres, "jane@example.com"),
            Ok(())
        );
        assert_eq!(validate(&DatabaseType::MySQL, " jane@example.com "), Ok(()));
        for db in [
            DatabaseType::SQLite,
            DatabaseType::SQLServer,
            DatabaseType::Azure,
        ] {
            let e = validate(&db, "jane@example.com").unwrap_err();
            assert!(e.contains("PostgreSQL et MySQL"), "{e}");
        }
        // The forms show this one as is.
        assert_eq!(
            validate(&DatabaseType::Postgres, "  ").unwrap_err(),
            "Indique ton compte Google"
        );
        let e = validate(&DatabaseType::Postgres, "jane").unwrap_err();
        assert!(e.contains("adresse e-mail"), "{e}");
        let e = validate(&DatabaseType::Postgres, "jane@x.com & calc").unwrap_err();
        assert!(e.contains("caractères"), "{e}");
    }

    #[test]
    fn database_user_per_engine() {
        let pg = DatabaseType::Postgres;
        let my = DatabaseType::MySQL;
        assert_eq!(database_user(&pg, " jane@example.com "), "jane@example.com");
        assert_eq!(database_user(&my, "jane@example.com"), "jane");
        // Service accounts: PostgreSQL drops `.gserviceaccount.com`.
        let sa = "sa@proj.iam.gserviceaccount.com";
        assert_eq!(database_user(&pg, sa), "sa@proj.iam");
        assert_eq!(database_user(&pg, "sa@proj.iam"), "sa@proj.iam");
        assert_eq!(database_user(&my, sa), "sa");
        assert_eq!(database_user(&my, ""), "");
    }

    #[test]
    fn login_hint_names_the_expected_user() {
        let e = "error returned from database: password authentication failed for user \"jane@example.com\"";
        let hint = login_hint(e, &DatabaseType::Postgres, "jane@example.com").unwrap();
        assert!(hint.contains("cloud_iam_user"), "{hint}");
        assert!(hint.contains("GRANT"), "{hint}");
        assert!(hint.ends_with("nom attendu : jane@example.com"), "{hint}");
        let e =
            "error returned from database: 1045 (28000): Access denied for user 'jane'@'1.2.3.4'";
        let hint = login_hint(e, &DatabaseType::MySQL, "jane@example.com").unwrap();
        assert!(hint.ends_with("nom attendu : jane"), "{hint}");
        assert_eq!(
            login_hint("connection refused", &DatabaseType::MySQL, "j@x.c"),
            None
        );
    }

    #[test]
    fn token_debug_hides_the_secret() {
        let t = Token::new("ya29.secret", Instant::now());
        let shown = format!("{t:?}");
        assert!(!shown.contains("ya29"), "{shown}");
        assert_eq!(t.secret(), "ya29.secret");
    }

    #[test]
    fn command_asks_for_a_login_token_for_the_account() {
        assert_eq!(
            token_args("jane@example.com"),
            [
                "sql",
                "generate-login-token",
                "--account=jane@example.com",
                "--quiet"
            ]
        );
        let args = token_args("j@x.c");
        let (program, all) = command_line(&args, true);
        assert_eq!(program, "cmd");
        assert_eq!(all[..2], ["/C", "gcloud"]);
        assert_eq!(all[2..], args[..]);
        assert_eq!(command_line(&args, false), ("gcloud", args.clone()));
    }

    fn failed(code: i32, stderr: &str) -> CmdOutput {
        CmdOutput {
            success: false,
            code: Some(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn failures_are_classified() {
        assert_eq!(
            classify(&failed(
                1,
                "'gcloud' is not recognized as an internal or external command,"
            )),
            Failure::NotInstalled
        );
        assert_eq!(
            classify(&failed(
                1,
                "'gcloud' n'est pas reconnu en tant que commande interne"
            )),
            Failure::NotInstalled
        );
        assert_eq!(classify(&failed(9009, "")), Failure::NotInstalled);
        assert_eq!(
            classify(&failed(
                2,
                "ERROR: (gcloud.sql) Invalid choice: 'generate-login-token'.\nMaybe you meant:"
            )),
            Failure::UnknownCommand
        );
        for signed_out in [
            "ERROR: (gcloud.sql.generate-login-token) There was a problem refreshing your current auth tokens: Reauthentication failed. Please run:\n\n  $ gcloud auth login",
            "ERROR: (gcloud.auth.print-access-token) Your current active account [jane@example.com] does not have any valid credentials",
            "ERROR: (gcloud.auth.print-access-token) You do not currently have an active account selected.",
            "invalid_grant: Bad Request",
        ] {
            assert_eq!(classify(&failed(1, signed_out)), Failure::NotSignedIn, "{signed_out}");
        }
        assert_eq!(
            classify(&failed(
                1,
                "\nERROR: (gcloud.sql.generate-login-token) Something broke.\nmore"
            )),
            Failure::Other("Something broke.".into())
        );
        assert_eq!(
            classify(&failed(3, "  ")),
            Failure::Other("code de sortie 3".into())
        );
        let long = format!("ERROR: {}", "é".repeat(400));
        let Failure::Other(msg) = classify(&failed(1, &long)) else {
            panic!()
        };
        assert_eq!(msg.chars().count(), 301);
    }

    #[test]
    fn failures_are_explained_in_french() {
        let e = explain(Failure::NotInstalled, "j@x.c").to_string();
        assert_eq!(
            e,
            "gcloud introuvable : installe le SDK Google Cloud ou ajoute-le au PATH"
        );
        let e = explain(Failure::NotSignedIn, "jane@example.com").to_string();
        assert!(
            e.contains("Lance `gcloud auth login` (compte jane@example.com)"),
            "{e}"
        );
        let e = explain(Failure::Other("boom".into()), "j@x.c").to_string();
        assert!(e.ends_with("boom"), "{e}");
        let e = launch_error(std::io::ErrorKind::NotFound.into()).to_string();
        assert!(e.starts_with("gcloud introuvable"), "{e}");
        let e = launch_error(std::io::ErrorKind::TimedOut.into()).to_string();
        assert!(e.contains("30 s"), "{e}");
    }

    #[test]
    fn token_is_the_last_line_and_never_quoted() {
        assert_eq!(parse_token(b"ya29.abc\r\n").unwrap(), "ya29.abc");
        assert_eq!(parse_token(b"notice\nya29.abc\n\n").unwrap(), "ya29.abc");
        for bad in [&b""[..], b"  \n", b"ya29 abc"] {
            let e = parse_token(bad).unwrap_err().to_string();
            assert!(!e.contains("ya29"), "{e}");
        }
    }

    fn jwt(payload: &str) -> String {
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.sig", b64("{\"alg\":\"RS256\"}"), b64(payload))
    }

    #[test]
    fn expiry_reads_a_jwt_exp_or_assumes_55_minutes() {
        let now = Instant::now();
        assert_eq!(expiry("ya29.opaque", now, 1_000), now + TOKEN_LIFETIME);
        assert_eq!(
            expiry(&jwt("{\"exp\":1600}"), now, 1_000),
            now + Duration::from_secs(600)
        );
        // Already expired: renew at once.
        assert_eq!(expiry(&jwt("{\"exp\":10}"), now, 1_000), now);
        // No usable `exp`.
        assert_eq!(
            expiry(&jwt("{\"sub\":\"x\"}"), now, 1_000),
            now + TOKEN_LIFETIME
        );
        assert_eq!(expiry("a.!!!.c", now, 1_000), now + TOKEN_LIFETIME);
    }

    #[test]
    fn renewal_comes_five_minutes_early_but_not_before_30_s() {
        let t = Timing::DEFAULT;
        let now = Instant::now();
        let min = Duration::from_secs(60);
        assert_eq!(t.delay(now + 55 * min, now), 50 * min);
        assert_eq!(t.delay(now + 5 * min, now), Duration::from_secs(30));
        assert_eq!(t.delay(now, now), Duration::from_secs(30));
        assert_eq!(t.delay(now + 6 * min, now), min);
        // A cached token is not handed out within those 5 minutes.
        assert!(usable(&Token::new("t", now + 6 * min), now));
        assert!(!usable(&Token::new("t", now + 5 * min), now));
    }

    #[test]
    fn failed_renewals_back_off_up_to_5_minutes() {
        let t = Timing::DEFAULT;
        let s = Duration::from_secs;
        let waits: Vec<_> = (1..=7).map(|n| t.retry_after(n)).collect();
        assert_eq!(
            waits,
            [s(30), s(60), s(120), s(240), s(300), s(300), s(300)]
        );
        assert_eq!(t.retry_after(u32::MAX), s(300));
    }

    fn ok(stdout: &str) -> CmdOutput {
        CmdOutput {
            success: true,
            code: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[tokio::test]
    async fn an_sdk_without_login_tokens_must_be_updated() {
        // No fallback to `auth print-access-token`: a general Google Cloud
        // token must not travel over TLS that doesn't verify the server.
        let calls = Mutex::new(Vec::new());
        let e = fetch_with("jane@example.com", |args: Vec<String>| {
            calls.lock().unwrap().push(args[..2].join(" "));
            async {
                Ok(failed(
                    2,
                    "ERROR: (gcloud.sql) Invalid choice: 'generate-login-token'.",
                ))
            }
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("mets à jour le SDK Google Cloud"), "{e}");
        assert_eq!(*calls.lock().unwrap(), ["sql generate-login-token"]);
    }

    #[tokio::test]
    async fn fetch_reports_errors_without_retrying() {
        let calls = AtomicUsize::new(0);
        let e = fetch_with("jane@example.com", |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Ok(failed(
                    1,
                    "ERROR: (gcloud.sql.generate-login-token) Reauthentication failed.",
                ))
            }
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("gcloud auth login"), "{e}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let e = fetch_with("j@x.c", |_| async {
            Err(std::io::ErrorKind::NotFound.into())
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(e.starts_with("gcloud introuvable"), "{e}");
        // An account that could inject into the command line never runs.
        let e = fetch_with("j@x.c&calc", |_| async {
            unreachable!("must not run");
            #[allow(unreachable_code)]
            Ok(ok(""))
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(e.contains("caractères"), "{e}");
    }

    #[tokio::test]
    async fn tokens_are_cached_per_account() {
        let calls = AtomicUsize::new(0);
        let run = |_: Vec<String>| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(ok(&format!("tok{n}"))) }
        };
        let a = cached_token("cache-a@example.com", run).await.unwrap();
        let again = cached_token("cache-a@example.com", run).await.unwrap();
        assert_eq!(a.secret(), again.secret());
        let b = cached_token("cache-b@example.com", run).await.unwrap();
        assert_ne!(a.secret(), b.secret());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Close to expiry: a new token.
        tokens().get_mut("cache-a@example.com").unwrap().expires_at = Instant::now();
        let renewed = cached_token("cache-a@example.com", run).await.unwrap();
        assert_ne!(renewed.secret(), a.secret());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
