//! Azure SQL sign-in with the user's own Entra ID account through the
//! browser (OAuth 2.0 authorization code + PKCE), like SSMS's "Active
//! Directory Interactive".

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::engine::models::{AzureAuthMethod, ConnectionConfig, DatabaseType};
use crate::engine::secrets::{self, SecretStore};

use super::azure::{is_guid, AzureCloud};

/// Check the tenant and application (client) ID of an interactive
/// connection; a French message for the form otherwise.
///
/// The tenant is a GUID or a domain; `common` / `organizations` are refused
/// since an organization's application is bound to its own tenant.
pub fn validate_interactive(tenant: &str, client_id: &str) -> Result<(), String> {
    let tenant = tenant.trim();
    if tenant.is_empty() {
        return Err("Le tenant est obligatoire".into());
    }
    let lower = tenant.to_ascii_lowercase();
    if lower == "common" || lower == "organizations" {
        return Err(format!(
            "Indique le tenant de ton organisation (GUID ou domaine), pas « {lower} »"
        ));
    }
    if !is_guid(client_id.trim()) {
        return Err("L'ID client de l'application doit être un GUID".into());
    }
    Ok(())
}

// ========== PKCE ==========

/// PKCE S256 challenge of `verifier`: SHA-256, base64url without padding
/// (RFC 7636).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `bytes` random bytes from the OS, base64url without padding.
fn random_urlsafe(bytes: usize) -> anyhow::Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf)
        .map_err(|e| anyhow::anyhow!("Générateur aléatoire du système indisponible : {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

// ========== Target ==========

/// Where and for what a token is requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Entra ID authority of the server's cloud, e.g.
    /// `https://login.microsoftonline.com`.
    pub authority: String,
    pub tenant: String,
    pub client_id: String,
    /// `<Azure SQL resource>/.default offline_access openid profile`.
    pub scope: String,
    /// The account typed in the connection, suggested to Entra ID.
    pub login_hint: Option<String>,
}

impl Target {
    /// The target of an interactive connection: cloud from its host, tenant
    /// and application checked (`validate_interactive`), account from its
    /// username.
    pub fn for_config(config: &ConnectionConfig) -> Result<Target, String> {
        let tenant = config.tenant_id.as_deref().unwrap_or_default().trim();
        let client_id = config.azure_client_id.as_deref().unwrap_or_default().trim();
        validate_interactive(tenant, client_id)?;
        let cloud = AzureCloud::from_host(config.host.as_deref().unwrap_or_default());
        let login_hint = config
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        Ok(Target {
            authority: format!("https://{}", cloud.authority_host()),
            tenant: tenant.to_string(),
            client_id: client_id.to_ascii_lowercase(),
            scope: format!("{} offline_access openid profile", cloud.sql_scope()),
            login_hint,
        })
    }

    /// `<authority>/<tenant>/oauth2/v2.0/<endpoint>`, the tenant kept in
    /// one path segment.
    fn endpoint(&self, endpoint: &str) -> Url {
        let mut url = Url::parse(&self.authority).expect("the authority is a valid URL");
        url.path_segments_mut()
            .expect("the authority is an http(s) URL")
            .pop_if_empty()
            .extend([self.tenant.as_str(), "oauth2", "v2.0", endpoint]);
        url
    }
}

/// Key of `t`'s refresh token in the memory cache and the OS keyring.
pub fn secret_key(t: &Target) -> String {
    format!(
        "entra:{}:{}:{}",
        t.tenant.to_lowercase(),
        t.client_id.to_lowercase(),
        t.login_hint
            .as_deref()
            .map_or("-".into(), str::to_lowercase)
    )
}

// ========== Authorization request ==========

/// The browser URL that starts the sign-in.
pub fn authorize_url(t: &Target, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let mut url = t.endpoint("authorize");
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("client_id", &t.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &t.scope)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("prompt", "select_account");
        if let Some(hint) = &t.login_hint {
            q.append_pair("login_hint", hint);
        }
    }
    url.into()
}

// ========== Errors ==========

/// Why a sign-in failed. Never carries a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// A request to the local listener that isn't the sign-in redirect
    /// (`/favicon.ico`…): ignored.
    NotACallback,
    /// The redirect's `state` isn't the one sent.
    StateMismatch,
    /// The user cancelled in the browser (`access_denied`).
    Denied { description: String },
    /// An OAuth error from Entra ID.
    Server {
        code: String,
        description: String,
        /// `error_codes` of a token response (AADSTS numbers).
        codes: Vec<u64>,
    },
    /// The browser sign-in wasn't finished in time.
    Timeout,
    /// Entra ID couldn't be reached.
    Network(String),
    /// An answer that isn't a token response.
    InvalidResponse(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&explain(self))
    }
}

impl std::error::Error for AuthError {}

/// French message for `err`, Entra ID's own message in parentheses.
pub fn explain(err: &AuthError) -> String {
    match err {
        AuthError::Server {
            code,
            description,
            codes,
        } => {
            // Entra ID appends trace and correlation IDs on further lines.
            let first_line = description.lines().next().unwrap_or_default().trim();
            let original = if first_line.is_empty() {
                code.as_str()
            } else {
                first_line
            };
            let has = |n: u64| codes.contains(&n) || aadsts_codes(description).contains(&n);
            let message = if has(65001) || code == "consent_required" {
                "L'application n'a pas l'autorisation d'accéder à Azure SQL : un administrateur \
                 doit accorder la permission déléguée « Azure SQL Database / user_impersonation »."
            } else if has(50011) {
                "L'URI de redirection `http://localhost` n'est pas déclarée : ajoute-la dans la \
                 plateforme « Applications mobiles et de bureau » de l'application."
            } else if has(7000218) {
                "Active « Autoriser les flux clients publics » dans l'application."
            } else if has(700016) {
                "Application introuvable dans ce tenant : vérifie l'ID client et le tenant."
            } else if has(90002) {
                "Tenant introuvable : vérifie le tenant."
            } else {
                "Échec de la connexion Azure AD."
            };
            format!("{message} ({original})")
        }
        AuthError::Denied { description } => {
            let first_line = description.lines().next().unwrap_or_default().trim();
            if first_line.is_empty() {
                "Connexion annulée dans le navigateur.".into()
            } else {
                format!("Connexion annulée dans le navigateur. ({first_line})")
            }
        }
        AuthError::Timeout => "Connexion non terminée dans le navigateur après 5 minutes.".into(),
        AuthError::StateMismatch => {
            "Réponse de connexion inattendue (state différent) : relance la connexion.".into()
        }
        AuthError::NotACallback => "Réponse de connexion inattendue.".into(),
        AuthError::Network(e) => format!("Impossible de joindre Entra ID ({e})"),
        AuthError::InvalidResponse(e) => format!("Réponse d'Entra ID illisible ({e})"),
    }
}

/// The numbers of the `AADSTS<n>` codes in `text`.
fn aadsts_codes(text: &str) -> Vec<u64> {
    text.match_indices("AADSTS")
        .filter_map(|(i, m)| {
            let digits: String = text[i + m.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().ok()
        })
        .collect()
}

// ========== Redirect ==========

/// The authorization code of the redirect whose request target (`/?code=…`)
/// is `request_target`.
pub fn parse_callback(request_target: &str, expected_state: &str) -> Result<String, AuthError> {
    let base = Url::parse("http://localhost/").expect("valid base URL");
    let url = base
        .join(request_target)
        .map_err(|_| AuthError::NotACallback)?;
    if url.path() != "/" {
        return Err(AuthError::NotACallback);
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    let (code, error) = (param("code"), param("error"));
    if code.is_none() && error.is_none() {
        return Err(AuthError::NotACallback);
    }
    if param("state").as_deref() != Some(expected_state) {
        return Err(AuthError::StateMismatch);
    }
    if let Some(error) = error {
        let description = param("error_description").unwrap_or_default();
        return Err(if error == "access_denied" {
            AuthError::Denied { description }
        } else {
            AuthError::Server {
                code: error,
                description,
                codes: Vec::new(),
            }
        });
    }
    Ok(code.unwrap_or_default())
}

// ========== Tokens ==========

/// Tokens of a successful token response.
#[derive(Clone)]
pub struct Tokens {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_at: SystemTime,
}

/// Tokens stay out of logs.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access", &"<redacted>")
            .field("refresh", &self.refresh.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Parse the answer of the token endpoint (`status`, JSON `body`) received
/// at `now`.
pub fn parse_token_response(status: u16, body: &str, now: SystemTime) -> Result<Tokens, AuthError> {
    // No `Debug`: it holds tokens.
    #[derive(Deserialize)]
    struct Body {
        access_token: Option<String>,
        refresh_token: Option<String>,
        /// Seconds, a number (v2 endpoint) or a string (v1).
        expires_in: Option<serde_json::Value>,
        error: Option<String>,
        error_description: Option<String>,
        #[serde(default)]
        error_codes: Vec<u64>,
    }

    // The body may hold tokens: never put it in an error.
    let body: Body = serde_json::from_str(body)
        .map_err(|_| AuthError::InvalidResponse(format!("HTTP {status}")))?;
    if let Some(code) = body.error {
        return Err(AuthError::Server {
            code,
            description: body.error_description.unwrap_or_default(),
            codes: body.error_codes,
        });
    }
    match body.access_token {
        Some(access) if (200..300).contains(&status) => {
            let seconds = match body.expires_in {
                Some(serde_json::Value::Number(n)) => n.as_u64(),
                Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
                _ => None,
            };
            Ok(Tokens {
                access,
                refresh: body.refresh_token.filter(|r| !r.is_empty()),
                // Unknown lifetime: don't cache the token.
                expires_at: now + Duration::from_secs(seconds.unwrap_or(0)),
            })
        }
        _ => Err(AuthError::InvalidResponse(format!(
            "HTTP {status}, pas de jeton d'accès"
        ))),
    }
}

// ========== Decision ==========

/// An access token is reused while it stays valid this long.
const MIN_VALIDITY: Duration = Duration::from_secs(5 * 60);

/// How to get the next access token.
pub enum Next {
    UseCached,
    Refresh(String),
    Interactive,
}

/// Reuse `cached` while it stays valid at least 5 minutes, else renew it
/// with `refresh`, else sign in through the browser.
pub fn next_step(cached: Option<&Tokens>, refresh: Option<&str>, now: SystemTime) -> Next {
    if cached.is_some_and(|t| t.expires_at >= now + MIN_VALIDITY) {
        return Next::UseCached;
    }
    match refresh {
        Some(r) if !r.is_empty() => Next::Refresh(r.to_string()),
        _ => Next::Interactive,
    }
}

// ========== Interaction with the interface ==========

/// What the interface is told about a browser sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interaction {
    /// The sign-in to `connection` waits for the browser, opened at `url`
    /// (show it as a link to copy: the browser may not have opened).
    Waiting { connection: String, url: String },
    /// The wait for `connection` is over: signed in, failed or cancelled.
    Done { connection: String },
    /// To tell the user once, e.g. `secrets::UNAVAILABLE_WARNING` when the
    /// refresh token can't outlive the session.
    Warning { connection: String, message: String },
}

type Handler = Box<dyn Fn(Interaction) + Send + Sync>;

static HANDLER: RwLock<Option<Handler>> = RwLock::new(None);

/// Send sign-in interactions to `f` (replaces the previous handler). Without
/// a handler, nothing is reported.
pub fn set_interaction_handler(f: impl Fn(Interaction) + Send + Sync + 'static) {
    *HANDLER.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
}

fn emit(interaction: Interaction) {
    if let Some(handler) = HANDLER.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        handler(interaction);
    }
}

/// Reports the end of the wait however it ends, cancellation included (the
/// connection's future dropped).
struct DoneOnDrop<'a> {
    notify: &'a (dyn Fn(Interaction) + Send + Sync),
    connection: &'a str,
}

impl Drop for DoneOnDrop<'_> {
    fn drop(&mut self) {
        (self.notify)(Interaction::Done {
            connection: self.connection.to_string(),
        });
    }
}

// ========== Flow ==========

/// How long the user has to sign in in the browser.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Reading one request on the redirect listener.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest request accepted on the redirect listener.
const MAX_REQUEST: usize = 32 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole token request.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);

const SIGNED_IN: &str = "Connexion réussie, vous pouvez fermer cet onglet.";

/// Access tokens of the session and one lock per target.
#[derive(Default)]
struct Session {
    tokens: Mutex<HashMap<String, Tokens>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Session {
    fn tokens(&self) -> MutexGuard<'_, HashMap<String, Tokens>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_default()
            .clone()
    }
}

fn session() -> &'static Session {
    static SESSION: OnceLock<Session> = OnceLock::new();
    SESSION.get_or_init(Session::default)
}

/// What the flow talks to, replaced in tests.
struct Deps<'a> {
    /// Replaces the cloud's authority.
    authority: Option<String>,
    open_browser: Box<dyn Fn(&str) + Send + Sync>,
    notify: Box<dyn Fn(Interaction) + Send + Sync>,
    store: &'static dyn SecretStore,
    session: &'a Session,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    timeout: Duration,
}

/// An access token for Azure SQL with the Entra ID account of `config`
/// (`AzureAuthMethod::InteractiveBrowser`): the session's while it is valid,
/// else renewed with the refresh token (memory, then OS keyring), else
/// through the browser. The error is a French message for the user.
pub async fn token(config: &ConnectionConfig) -> anyhow::Result<String> {
    // The first use probes the OS keyring, which may block.
    let store = tokio::task::spawn_blocking(secrets::store).await?;
    let deps = Deps {
        authority: None,
        open_browser: Box::new(open_in_browser),
        notify: Box::new(emit),
        store,
        session: session(),
        now: Box::new(SystemTime::now),
        timeout: SIGN_IN_TIMEOUT,
    };
    let access = token_with(config, &deps).await?;
    if let Some(message) = secrets::take_warning() {
        emit(Interaction::Warning {
            connection: config.name.clone(),
            message: message.to_string(),
        });
    }
    Ok(access)
}

async fn token_with(config: &ConnectionConfig, deps: &Deps<'_>) -> anyhow::Result<String> {
    let mut target = Target::for_config(config).map_err(anyhow::Error::msg)?;
    if let Some(authority) = &deps.authority {
        target.authority = authority.clone();
    }
    let key = secret_key(&target);
    // One acquisition at a time per target: simultaneous reconnections
    // share one browser sign-in and one refresh-token rotation.
    let lock = deps.session.lock_for(&key);
    let _turn = lock.lock().await;

    let now = (deps.now)();
    let cached = deps.session.tokens().get(&key).cloned();
    let mut step = next_step(
        cached.as_ref(),
        cached.as_ref().and_then(|t| t.refresh.as_deref()),
        now,
    );
    if matches!(step, Next::Interactive) {
        // Nothing in memory (first connection of the session): the refresh
        // token kept in the OS keyring.
        let stored = load_secret(deps.store, &key).await;
        step = next_step(None, stored.as_deref(), now);
    }
    match step {
        Next::UseCached => {
            if let Some(t) = cached {
                return Ok(t.access);
            }
        }
        Next::Refresh(refresh) => match redeem(&target, Grant::Refresh(&refresh), deps).await {
            Ok(tokens) => {
                tracing::info!("Entra ID token renewed for {}", config.name);
                return Ok(keep(deps, &key, tokens, Some(refresh)).await);
            }
            Err(e) if needs_interaction(&e) => {
                tracing::info!(
                    "Entra ID refresh token of {} refused, signing in again",
                    config.name
                );
                deps.session.tokens().remove(&key);
                delete_secret(deps.store, &key).await;
            }
            Err(e) => return Err(e.into()),
        },
        Next::Interactive => {}
    }
    let tokens = sign_in(&config.name, &target, deps).await?;
    tracing::info!("Entra ID sign-in completed for {}", config.name);
    Ok(keep(deps, &key, tokens, None).await)
}

/// Whether a refused refresh token calls for a new browser sign-in.
fn needs_interaction(err: &AuthError) -> bool {
    matches!(err, AuthError::Server { code, .. }
        if code == "invalid_grant" || code == "interaction_required")
}

/// Keep `tokens` for the session and their refresh token in the store (or
/// `previous` when none came back); the access token.
async fn keep(deps: &Deps<'_>, key: &str, mut tokens: Tokens, previous: Option<String>) -> String {
    match &tokens.refresh {
        Some(refresh) => save_secret(deps.store, key, refresh.clone()).await,
        None => tokens.refresh = previous,
    }
    let access = tokens.access.clone();
    deps.session.tokens().insert(key.to_string(), tokens);
    access
}

// ----- Secret store (may block: off the executor) -----

async fn load_secret(store: &'static dyn SecretStore, key: &str) -> Option<String> {
    let key = key.to_string();
    match tokio::task::spawn_blocking(move || store.get(&key)).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            tracing::warn!("Could not read the Entra ID refresh token from the keyring: {e}");
            None
        }
        Err(e) => {
            tracing::warn!("Could not read the Entra ID refresh token from the keyring: {e}");
            None
        }
    }
}

async fn save_secret(store: &'static dyn SecretStore, key: &str, value: String) {
    let key = key.to_string();
    let saved = tokio::task::spawn_blocking(move || store.set(&key, &value)).await;
    if let Some(e) = blocking_error(saved) {
        tracing::warn!("Could not keep the Entra ID refresh token in the keyring: {e}");
    }
}

async fn delete_secret(store: &'static dyn SecretStore, key: &str) {
    let key = key.to_string();
    let deleted = tokio::task::spawn_blocking(move || store.delete(&key)).await;
    if let Some(e) = blocking_error(deleted) {
        tracing::warn!("Could not delete the Entra ID refresh token from the keyring: {e}");
    }
}

fn blocking_error(r: Result<Result<(), String>, tokio::task::JoinError>) -> Option<String> {
    match r {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(e) => Some(e.to_string()),
    }
}

// ----- Token endpoint -----

enum Grant<'a> {
    Refresh(&'a str),
    Code {
        code: &'a str,
        redirect_uri: &'a str,
        verifier: &'a str,
    },
}

/// Redeem `grant` at the token endpoint of `t`.
async fn redeem(t: &Target, grant: Grant<'_>, deps: &Deps<'_>) -> Result<Tokens, AuthError> {
    let mut form: Vec<(&'static str, String)> = vec![
        ("client_id", t.client_id.clone()),
        ("scope", t.scope.clone()),
    ];
    match grant {
        Grant::Refresh(refresh) => {
            form.push(("grant_type", "refresh_token".into()));
            form.push(("refresh_token", refresh.into()));
        }
        Grant::Code {
            code,
            redirect_uri,
            verifier,
        } => {
            form.push(("grant_type", "authorization_code".into()));
            form.push(("code", code.into()));
            form.push(("redirect_uri", redirect_uri.into()));
            form.push(("code_verifier", verifier.into()));
        }
    }
    let url = t.endpoint("token").to_string();
    let (status, body) = tokio::task::spawn_blocking(move || post_form(&url, &form))
        .await
        .map_err(|e| AuthError::Network(e.to_string()))??;
    parse_token_response(status, &body, (deps.now)())
}

/// POST `form` to `url`: status and body, whatever the status.
fn post_form(url: &str, form: &[(&str, String)]) -> Result<(u16, String), AuthError> {
    // The system's certificates, like the updater: HTTPS works behind a
    // corporate proxy inspecting it.
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(TOKEN_TIMEOUT))
        .build()
        .new_agent();
    // ureq's errors never include the request body (the secrets).
    let mut response = agent
        .post(url)
        .send_form(form.iter().map(|(k, v)| (*k, v.as_str())))
        .map_err(|e| AuthError::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| AuthError::Network(e.to_string()))?;
    Ok((status, body))
}

// ----- Browser sign-in -----

/// Open `url` in the default browser, off the async executor (some
/// launchers wait). A failure is only logged: the interface shows the link.
fn open_in_browser(url: &str) {
    let url = url.to_string();
    std::thread::spawn(move || {
        if let Err(e) = webbrowser::open(&url) {
            tracing::warn!("Could not open the browser for the Entra ID sign-in: {e}");
        }
    });
}

/// Sign in to `t` through the browser (authorization code + PKCE) for the
/// connection named `connection`.
async fn sign_in(connection: &str, t: &Target, deps: &Deps<'_>) -> anyhow::Result<Tokens> {
    let (v4, v6) = bind_loopback()
        .await
        .map_err(|e| anyhow::anyhow!("Impossible d'attendre le retour du navigateur : {e}"))?;
    let port = v4.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}");
    let verifier = random_urlsafe(32)?;
    let state = random_urlsafe(16)?;
    let url = authorize_url(t, &redirect_uri, &pkce_challenge(&verifier), &state);

    let code = {
        (deps.notify)(Interaction::Waiting {
            connection: connection.to_string(),
            url: url.clone(),
        });
        let _done = DoneOnDrop {
            notify: deps.notify.as_ref(),
            connection,
        };
        (deps.open_browser)(&url);
        tokio::time::timeout(deps.timeout, wait_for_code(&v4, v6.as_ref(), &state))
            .await
            .map_err(|_| AuthError::Timeout)??
    };
    Ok(redeem(
        t,
        Grant::Code {
            code: &code,
            redirect_uri: &redirect_uri,
            verifier: &verifier,
        },
        deps,
    )
    .await?)
}

/// Listen on a free port of 127.0.0.1 and the same port of ::1 when there
/// is an IPv6 loopback: the browser may resolve `localhost` to ::1 first.
async fn bind_loopback() -> std::io::Result<(TcpListener, Option<TcpListener>)> {
    for _ in 0..5 {
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = v4.local_addr()?.port();
        match TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await {
            Ok(v6) => return Ok((v4, Some(v6))),
            // Another program has it on ::1, where the browser could go
            // instead: another port.
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            // No IPv6 loopback.
            Err(_) => return Ok((v4, None)),
        }
    }
    let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    Ok((v4, None))
}

async fn accept(v4: &TcpListener, v6: Option<&TcpListener>) -> std::io::Result<TcpStream> {
    match v6 {
        Some(v6) => tokio::select! {
            r = v4.accept() => r.map(|(s, _)| s),
            r = v6.accept() => r.map(|(s, _)| s),
        },
        None => v4.accept().await.map(|(s, _)| s),
    }
}

/// Serve the redirect listener until the sign-in comes back: the code, or
/// why it failed. Other requests (`/favicon.ico`…) get a 404.
async fn wait_for_code(
    v4: &TcpListener,
    v6: Option<&TcpListener>,
    state: &str,
) -> Result<String, AuthError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, TcpStream)>(4);
    loop {
        tokio::select! {
            accepted = accept(v4, v6) => {
                let mut stream = accepted.map_err(|e| AuthError::Network(e.to_string()))?;
                let tx = tx.clone();
                // Each request on its own: a browser's speculative connection
                // that sends nothing mustn't hold up the redirect.
                tokio::spawn(async move {
                    let read = tokio::time::timeout(REQUEST_TIMEOUT, read_request_target(&mut stream));
                    if let Ok(Ok(Some(target))) = read.await {
                        let _ = tx.send((target, stream)).await;
                    }
                });
            }
            Some((target, mut stream)) = rx.recv() => match parse_callback(&target, state) {
                Err(AuthError::NotACallback) => respond(&mut stream, "404 Not Found", "").await,
                Ok(code) => {
                    respond(&mut stream, "200 OK", &page(SIGNED_IN)).await;
                    return Ok(code);
                }
                Err(e) => {
                    respond(&mut stream, "400 Bad Request", &page(&explain(&e))).await;
                    return Err(e);
                }
            },
        }
    }
}

/// The target (`/?code=…`) of the GET request on `stream`, once its headers
/// are read (so that closing doesn't reset the connection); `None` for
/// anything else.
async fn read_request_target(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_REQUEST {
            return Ok(None);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut request_line = text.lines().next().unwrap_or_default().split_whitespace();
    Ok(match (request_line.next(), request_line.next()) {
        (Some("GET"), Some(target)) => Some(target.to_string()),
        _ => None,
    })
}

async fn respond(stream: &mut TcpStream, status: &str, html: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        html.len()
    );
    // The browser may be gone: nothing to do about it.
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(html.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The page shown in the browser at the end of the sign-in.
fn page(message: &str) -> String {
    let message = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    format!(
        "<!doctype html><html lang=\"fr\"><head><meta charset=\"utf-8\">\
         <title>storingUnicorns</title></head>\
         <body style=\"font-family: sans-serif; margin: 3em\">\
         <h1>storingUnicorns</h1><p>{message}</p></body></html>"
    )
}

// ========== Forgetting ==========

/// Key of what an interactive browser connection keeps (session token and
/// refresh token); `None` for other connections, which keep nothing.
fn kept_key(config: &ConnectionConfig) -> Option<String> {
    let interactive = config.db_type == DatabaseType::Azure
        && config.azure_auth_method == Some(AzureAuthMethod::InteractiveBrowser);
    interactive
        .then(|| Target::for_config(config).ok())
        .flatten()
        .map(|t| secret_key(&t))
}

/// Whether saving `new` over `old` must forget `old`'s sign-in: another
/// tenant, application, account or method (or no longer Azure).
pub fn needs_forget(old: &ConnectionConfig, new: &ConnectionConfig) -> bool {
    let kept = kept_key(old);
    kept.is_some() && kept != kept_key(new)
}

/// Whether `a` and `b` share one sign-in: interactive browser connections
/// with the same tenant, application and account (as in `secret_key`).
fn same_sign_in(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    let key = kept_key(a);
    key.is_some() && key == kept_key(b)
}

/// Whether one of `others` shares `config`'s sign-in.
fn sign_in_used_by(config: &ConnectionConfig, others: &[ConnectionConfig]) -> bool {
    others.iter().any(|other| same_sign_in(config, other))
}

/// `forget` `config`'s sign-in unless one of the `remaining` connections
/// (those kept after a delete, or all of them, the edited one in its new
/// form, after an edit) uses the same one: other databases of the same
/// server keep it. The explicit "Oublier la connexion Azure AD" calls
/// `forget`.
pub fn forget_unless_used(config: &ConnectionConfig, remaining: &[ConnectionConfig]) {
    if !sign_in_used_by(config, remaining) {
        forget(config);
    }
}

/// Forget the Entra ID sign-in of `config` (session token and refresh token
/// in the OS keyring): when the connection is deleted, changes target
/// (`needs_forget`) or on request. Nothing for other methods. Doesn't block:
/// the keyring is updated in the background.
pub fn forget(config: &ConnectionConfig) {
    let Some(key) = kept_key(config) else {
        return;
    };
    let session = session();
    // At once, so that the next connection doesn't reuse it…
    session.tokens().remove(&key);
    // …and for good once an acquisition in progress is over. Waiting for it
    // and the keyring (Secret Service, an unlock prompt) may block: not on
    // the interface's thread.
    std::thread::spawn(move || forget_key(session, secrets::store(), &key));
}

/// Drop `key`'s session token and refresh token once any acquisition in
/// progress for it is over: it would keep its tokens after us otherwise.
/// Blocks: never call it from async code.
fn forget_key(session: &Session, store: &dyn SecretStore, key: &str) {
    let lock = session.lock_for(key);
    let _turn = lock.blocking_lock();
    session.tokens().remove(key);
    if let Err(e) = store.delete(key) {
        tracing::warn!("Could not delete the Entra ID refresh token from the keyring: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: &str = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";

    #[test]
    fn tenant_is_required() {
        assert_eq!(
            validate_interactive("", CLIENT).unwrap_err(),
            "Le tenant est obligatoire"
        );
        assert_eq!(
            validate_interactive("   ", CLIENT).unwrap_err(),
            "Le tenant est obligatoire"
        );
    }

    #[test]
    fn multi_tenant_endpoints_are_refused() {
        assert_eq!(
            validate_interactive("common", CLIENT).unwrap_err(),
            "Indique le tenant de ton organisation (GUID ou domaine), pas « common »"
        );
        assert_eq!(
            validate_interactive(" Organizations ", CLIENT).unwrap_err(),
            "Indique le tenant de ton organisation (GUID ou domaine), pas « organizations »"
        );
        assert!(validate_interactive("COMMON", CLIENT).is_err());
    }

    #[test]
    fn client_id_must_be_a_guid() {
        for bad in ["", "  ", "my-app", "8f1c2d3e4a5b6c7d8e9f0a1b2c3d4e5f"] {
            assert_eq!(
                validate_interactive("contoso.com", bad).unwrap_err(),
                "L'ID client de l'application doit être un GUID",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn guid_or_domain_tenant_with_guid_client_is_valid() {
        assert!(validate_interactive("contoso.com", CLIENT).is_ok());
        assert!(validate_interactive(
            " contoso.onmicrosoft.com ",
            " 8F1C2D3E-4A5B-6C7D-8E9F-0A1B2C3D4E5F "
        )
        .is_ok());
        assert!(validate_interactive("72f988bf-86f1-41af-91ab-2d7cd011db47", CLIENT).is_ok());
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn random_values_are_urlsafe_and_differ() {
        let a = random_urlsafe(32).unwrap();
        let b = random_urlsafe(32).unwrap();
        assert_eq!(a.len(), 43); // 32 bytes, unpadded base64
        assert_ne!(a, b);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    fn azure(host: &str) -> ConnectionConfig {
        ConnectionConfig {
            db_type: DatabaseType::Azure,
            host: Some(host.into()),
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: Some(" Contoso.com ".into()),
            azure_client_id: Some(" 8F1C2D3E-4A5B-6C7D-8E9F-0A1B2C3D4E5F ".into()),
            username: None,
            ..Default::default()
        }
    }

    fn target() -> Target {
        Target::for_config(&azure("srv.database.windows.net")).unwrap()
    }

    #[test]
    fn target_of_a_public_cloud_connection() {
        let t = target();
        assert_eq!(t.authority, "https://login.microsoftonline.com");
        assert_eq!(t.tenant, "Contoso.com");
        assert_eq!(t.client_id, "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f");
        assert_eq!(
            t.scope,
            "https://database.windows.net/.default offline_access openid profile"
        );
        assert_eq!(t.login_hint, None);
    }

    #[test]
    fn target_follows_the_cloud_and_the_account() {
        let mut c = azure("srv.database.usgovcloudapi.net");
        c.username = Some(" jane@contoso.com ".into());
        let t = Target::for_config(&c).unwrap();
        assert_eq!(t.authority, "https://login.microsoftonline.us");
        assert!(t
            .scope
            .starts_with("https://database.usgovcloudapi.net/.default "));
        assert_eq!(t.login_hint.as_deref(), Some("jane@contoso.com"));

        let t = Target::for_config(&azure("srv.database.chinacloudapi.cn")).unwrap();
        assert_eq!(t.authority, "https://login.chinacloudapi.cn");

        c.username = Some("  ".into());
        assert_eq!(Target::for_config(&c).unwrap().login_hint, None);
    }

    #[test]
    fn target_is_validated() {
        let mut c = azure("srv.database.windows.net");
        c.tenant_id = Some("common".into());
        assert!(Target::for_config(&c).unwrap_err().contains("« common »"));
        c.tenant_id = None;
        assert_eq!(
            Target::for_config(&c).unwrap_err(),
            "Le tenant est obligatoire"
        );
        let mut c = azure("srv.database.windows.net");
        c.azure_client_id = None;
        assert!(Target::for_config(&c).unwrap_err().contains("GUID"));
    }

    #[test]
    fn secret_key_is_lowercase_with_a_dash_for_no_account() {
        let mut t = target();
        assert_eq!(
            secret_key(&t),
            "entra:contoso.com:8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f:-"
        );
        t.login_hint = Some("Jane@Contoso.com".into());
        assert_eq!(
            secret_key(&t),
            "entra:contoso.com:8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f:jane@contoso.com"
        );
    }

    fn query(url: &str) -> std::collections::HashMap<String, String> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }

    #[test]
    fn authorize_url_has_every_parameter() {
        let url = authorize_url(&target(), "http://localhost:51234", "chal", "st");
        assert!(
            url.starts_with("https://login.microsoftonline.com/Contoso.com/oauth2/v2.0/authorize?"),
            "{url}"
        );
        let q = query(&url);
        assert_eq!(q["client_id"], "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], "http://localhost:51234");
        assert_eq!(
            q["scope"],
            "https://database.windows.net/.default offline_access openid profile"
        );
        assert_eq!(q["code_challenge"], "chal");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st");
        assert_eq!(q["prompt"], "select_account");
        assert!(!q.contains_key("login_hint"));
        // Encoded, not raw.
        assert!(
            url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A51234"),
            "{url}"
        );
        assert!(!url.contains(' '), "{url}");
    }

    #[test]
    fn authorize_url_suggests_the_account() {
        let mut t = target();
        t.login_hint = Some("jane+sql@contoso.com".into());
        let url = authorize_url(&t, "http://localhost:1", "c", "s");
        assert_eq!(query(&url)["login_hint"], "jane+sql@contoso.com");
        assert!(url.contains("login_hint=jane%2Bsql%40contoso.com"), "{url}");
    }

    #[test]
    fn authorize_url_uses_the_cloud_authority() {
        let mut c = azure("srv.database.usgovcloudapi.net");
        let t = Target::for_config(&c).unwrap();
        let url = authorize_url(&t, "http://localhost:1", "c", "s");
        assert!(
            url.starts_with("https://login.microsoftonline.us/Contoso.com/oauth2/v2.0/authorize?")
        );
        assert!(query(&url)["scope"].starts_with("https://database.usgovcloudapi.net/.default"));
        c.host = Some("srv.database.chinacloudapi.cn".into());
        let t = Target::for_config(&c).unwrap();
        let url = authorize_url(&t, "http://localhost:1", "c", "s");
        assert!(
            url.starts_with("https://login.chinacloudapi.cn/Contoso.com/oauth2/v2.0/authorize?")
        );
    }

    #[test]
    fn authorize_url_keeps_the_tenant_in_one_path_segment() {
        let mut t = target();
        t.tenant = "contoso.com/evil?x=1".into();
        let url = Url::parse(&authorize_url(&t, "http://localhost:1", "c", "s")).unwrap();
        assert_eq!(
            url.path(),
            "/contoso.com%2Fevil%3Fx=1/oauth2/v2.0/authorize"
        );
        assert!(!url.query_pairs().any(|(k, _)| k == "x"));
    }

    #[test]
    fn callback_gives_the_code() {
        assert_eq!(parse_callback("/?code=abc&state=st", "st").unwrap(), "abc");
        assert_eq!(
            parse_callback("/?state=st&code=a%2Fb&session_state=x", "st").unwrap(),
            "a/b"
        );
    }

    #[test]
    fn callback_with_another_state_is_rejected() {
        assert_eq!(
            parse_callback("/?code=abc&state=other", "st").unwrap_err(),
            AuthError::StateMismatch
        );
        assert_eq!(
            parse_callback("/?code=abc", "st").unwrap_err(),
            AuthError::StateMismatch
        );
    }

    #[test]
    fn callback_errors() {
        assert_eq!(
            parse_callback(
                "/?error=access_denied&error_description=AADSTS65004%3A+User+declined&state=st",
                "st"
            )
            .unwrap_err(),
            AuthError::Denied {
                description: "AADSTS65004: User declined".into()
            }
        );
        assert_eq!(
            parse_callback(
                "/?error=consent_required&error_description=AADSTS65001%3A+no+consent&state=st",
                "st"
            )
            .unwrap_err(),
            AuthError::Server {
                code: "consent_required".into(),
                description: "AADSTS65001: no consent".into(),
                codes: vec![],
            }
        );
    }

    #[test]
    fn other_requests_are_not_callbacks() {
        for target in ["/favicon.ico", "/", "/?foo=bar", "/other?code=abc&state=st"] {
            assert_eq!(
                parse_callback(target, "st").unwrap_err(),
                AuthError::NotACallback,
                "{target}"
            );
        }
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn token_response_gives_tokens_and_expiry() {
        let body = r#"{"token_type":"Bearer","scope":"https://database.windows.net/user_impersonation",
            "expires_in":3599,"ext_expires_in":3599,"access_token":"AT","refresh_token":"RT","id_token":"x"}"#;
        let t = parse_token_response(200, body, at(1000)).unwrap();
        assert_eq!(t.access, "AT");
        assert_eq!(t.refresh.as_deref(), Some("RT"));
        assert_eq!(t.expires_at, at(4599));
        // Older endpoints send the lifetime as a string; the refresh token
        // may be missing.
        let t =
            parse_token_response(200, r#"{"access_token":"AT","expires_in":"60"}"#, at(0)).unwrap();
        assert_eq!(t.refresh, None);
        assert_eq!(t.expires_at, at(60));
    }

    #[test]
    fn token_error_response() {
        let body = r#"{"error":"invalid_grant","error_description":"AADSTS70008: The refresh token has expired.\r\nTrace ID: 1",
            "error_codes":[70008],"timestamp":"2026-10-15 10:00:00Z"}"#;
        assert_eq!(
            parse_token_response(400, body, at(0)).unwrap_err(),
            AuthError::Server {
                code: "invalid_grant".into(),
                description: "AADSTS70008: The refresh token has expired.\r\nTrace ID: 1".into(),
                codes: vec![70008],
            }
        );
    }

    #[test]
    fn unreadable_token_response_does_not_echo_the_body() {
        let e = parse_token_response(502, "<html>secret-ish</html>", at(0)).unwrap_err();
        assert!(
            matches!(&e, AuthError::InvalidResponse(m) if !m.contains("secret")),
            "{e:?}"
        );
        // A 200 without an access token isn't a success.
        assert!(matches!(
            parse_token_response(200, r#"{"refresh_token":"RT"}"#, at(0)).unwrap_err(),
            AuthError::InvalidResponse(_)
        ));
    }

    #[test]
    fn tokens_debug_is_redacted() {
        let t = Tokens {
            access: "AT-secret".into(),
            refresh: Some("RT-secret".into()),
            expires_at: at(0),
        };
        let debug = format!("{t:?}");
        assert!(!debug.contains("secret"), "{debug}");
    }

    fn server(code: &str, description: &str, codes: &[u64]) -> AuthError {
        AuthError::Server {
            code: code.into(),
            description: description.into(),
            codes: codes.to_vec(),
        }
    }

    #[test]
    fn known_entra_errors_are_explained() {
        let consent = explain(&server(
            "invalid_grant",
            "AADSTS65001: The user or administrator has not consented.\r\nTrace ID: 1",
            &[65001],
        ));
        assert!(
            consent.starts_with(
                "L'application n'a pas l'autorisation d'accéder à Azure SQL : un administrateur \
                 doit accorder la permission déléguée « Azure SQL Database / user_impersonation »."
            ),
            "{consent}"
        );
        assert!(
            consent.ends_with("(AADSTS65001: The user or administrator has not consented.)"),
            "{consent}"
        );
        assert!(explain(&server("consent_required", "", &[]))
            .starts_with("L'application n'a pas l'autorisation"));
        assert!(explain(&server(
            "invalid_request",
            "AADSTS50011: The redirect URI does not match",
            &[]
        ))
        .starts_with(
            "L'URI de redirection `http://localhost` n'est pas déclarée : ajoute-la dans la \
             plateforme « Applications mobiles et de bureau » de l'application."
        ));
        assert!(explain(&server(
            "invalid_client",
            "AADSTS7000218: client_assertion or client_secret",
            &[7000218]
        ))
        .starts_with("Active « Autoriser les flux clients publics » dans l'application."));
        assert!(explain(&server(
            "unauthorized_client",
            "AADSTS700016: Application not found",
            &[700016]
        ))
        .starts_with("Application introuvable dans ce tenant : vérifie l'ID client et le tenant."));
        assert!(explain(&server(
            "invalid_request",
            "AADSTS90002: Tenant not found",
            &[90002]
        ))
        .starts_with("Tenant introuvable : vérifie le tenant."));
    }

    #[test]
    fn codes_are_matched_exactly() {
        // 70001 is none of the known codes (700016, 7000218…).
        let e = explain(&server("x", "AADSTS70001: other", &[70001]));
        assert!(e.starts_with("Échec de la connexion Azure AD"), "{e}");
        assert!(e.ends_with("(AADSTS70001: other)"), "{e}");
        // Without a description, the OAuth error code.
        assert_eq!(
            explain(&server("invalid_scope", "", &[])),
            "Échec de la connexion Azure AD. (invalid_scope)"
        );
        // A code only in error_codes is enough.
        assert!(
            explain(&server("invalid_client", "", &[7000218])).starts_with("Active « Autoriser")
        );
    }

    #[test]
    fn cancel_timeout_and_other_failures_are_explained() {
        assert_eq!(
            explain(&AuthError::Denied {
                description: "AADSTS65004: User declined".into()
            }),
            "Connexion annulée dans le navigateur. (AADSTS65004: User declined)"
        );
        assert_eq!(
            explain(&AuthError::Denied {
                description: String::new()
            }),
            "Connexion annulée dans le navigateur."
        );
        assert_eq!(
            explain(&AuthError::Timeout),
            "Connexion non terminée dans le navigateur après 5 minutes."
        );
        assert!(explain(&AuthError::StateMismatch).contains("state"));
        assert!(explain(&AuthError::Network("refused".into())).contains("(refused)"));
        assert_eq!(AuthError::Timeout.to_string(), explain(&AuthError::Timeout));
    }

    fn tokens(expires_at: SystemTime, refresh: Option<&str>) -> Tokens {
        Tokens {
            access: "AT".into(),
            refresh: refresh.map(Into::into),
            expires_at,
        }
    }

    #[test]
    fn valid_cached_token_is_reused() {
        let now = at(10_000);
        let t = tokens(now + Duration::from_secs(301), Some("RT"));
        assert!(matches!(
            next_step(Some(&t), Some("RT"), now),
            Next::UseCached
        ));
        let t = tokens(now + MIN_VALIDITY, None);
        assert!(matches!(next_step(Some(&t), None, now), Next::UseCached));
    }

    #[test]
    fn expiring_token_is_refreshed() {
        let now = at(10_000);
        let t = tokens(now + Duration::from_secs(299), Some("RT"));
        assert!(matches!(next_step(Some(&t), Some("RT"), now), Next::Refresh(r) if r == "RT"));
        assert!(matches!(next_step(None, Some("RT2"), now), Next::Refresh(r) if r == "RT2"));
    }

    #[test]
    fn without_refresh_token_the_browser_opens() {
        let now = at(10_000);
        let t = tokens(at(0), None);
        assert!(matches!(next_step(Some(&t), None, now), Next::Interactive));
        assert!(matches!(next_step(None, None, now), Next::Interactive));
        assert!(matches!(next_step(None, Some(""), now), Next::Interactive));
    }

    fn signed_in() -> ConnectionConfig {
        ConnectionConfig {
            name: "Prod".into(),
            username: Some("jane@contoso.com".into()),
            database: "app".into(),
            ..azure("srv.database.windows.net")
        }
    }

    #[test]
    fn same_account_target_needs_no_forgetting() {
        let old = signed_in();
        assert!(!needs_forget(&old, &old.clone()));
        let renamed = ConnectionConfig {
            name: "Prod 2".into(),
            database: "other".into(),
            color: Some([1, 2, 3]),
            ..old.clone()
        };
        assert!(!needs_forget(&old, &renamed));
        // Same tenant / application / account, written differently.
        let respelled = ConnectionConfig {
            tenant_id: Some("CONTOSO.COM".into()),
            azure_client_id: Some(CLIENT.to_uppercase()),
            username: Some(" Jane@Contoso.com ".into()),
            ..old.clone()
        };
        assert!(!needs_forget(&old, &respelled));
    }

    #[test]
    fn another_account_target_needs_forgetting() {
        let old = signed_in();
        let changes = [
            ConnectionConfig {
                tenant_id: Some("fabrikam.com".into()),
                ..old.clone()
            },
            ConnectionConfig {
                azure_client_id: Some("11111111-2222-3333-4444-555555555555".into()),
                ..old.clone()
            },
            ConnectionConfig {
                username: Some("john@contoso.com".into()),
                ..old.clone()
            },
            ConnectionConfig {
                username: None,
                ..old.clone()
            },
            ConnectionConfig {
                azure_auth_method: Some(AzureAuthMethod::Interactive),
                ..old.clone()
            },
            ConnectionConfig {
                db_type: DatabaseType::SQLServer,
                ..old.clone()
            },
        ];
        for new in &changes {
            assert!(needs_forget(&old, new), "{new:?}");
        }
        // Nothing was kept for other methods.
        let cli = ConnectionConfig {
            azure_auth_method: Some(AzureAuthMethod::Interactive),
            ..old.clone()
        };
        assert!(!needs_forget(&cli, &old));
    }

    #[test]
    fn connections_of_one_account_share_their_sign_in() {
        let prod = signed_in();
        // Another database of the same server, the account written
        // differently.
        let reporting = ConnectionConfig {
            name: "Reporting".into(),
            database: "reports".into(),
            tenant_id: Some(" CONTOSO.COM ".into()),
            azure_client_id: Some(CLIENT.to_uppercase()),
            username: Some(" Jane@Contoso.com ".into()),
            ..prod.clone()
        };
        assert!(same_sign_in(&prod, &reporting));
        let john = ConnectionConfig {
            username: Some("john@contoso.com".into()),
            ..prod.clone()
        };
        assert!(!same_sign_in(&prod, &john));
        let other_app = ConnectionConfig {
            azure_client_id: Some("11111111-2222-3333-4444-555555555555".into()),
            ..prod.clone()
        };
        assert!(!same_sign_in(&prod, &other_app));
        // Other methods keep nothing, so share nothing.
        let cli = ConnectionConfig {
            azure_auth_method: Some(AzureAuthMethod::Interactive),
            ..prod.clone()
        };
        assert!(!same_sign_in(&prod, &cli));
        assert!(!same_sign_in(&cli, &cli.clone()));

        // Deleting Prod keeps the sign-in Reporting still uses.
        assert!(sign_in_used_by(&prod, &[john.clone(), reporting]));
        assert!(!sign_in_used_by(&prod, &[john, other_app, cli]));
        assert!(!sign_in_used_by(&prod, &[]));
    }

    #[test]
    fn interactions_reach_the_handler() {
        // The only test using the global handler (`token` isn't called in
        // tests).
        static SEEN: Mutex<Vec<Interaction>> = Mutex::new(Vec::new());
        emit(Interaction::Done {
            connection: "before".into(),
        });
        set_interaction_handler(|i| SEEN.lock().unwrap().push(i));
        let waiting = Interaction::Waiting {
            connection: "Prod".into(),
            url: "https://login.microsoftonline.com/x".into(),
        };
        emit(waiting.clone());
        assert_eq!(*SEEN.lock().unwrap(), [waiting]);
    }

    #[test]
    fn forget_clears_the_session_token_and_the_secret() {
        let session = Session::default();
        let store = crate::engine::secrets::MemoryStore::default();
        let config = signed_in();
        let key = secret_key(&Target::for_config(&config).unwrap());
        session
            .tokens()
            .insert(key.clone(), tokens(at(0), Some("RT")));
        store.set(&key, "RT").unwrap();
        store.set("entra:other", "RT2").unwrap();

        forget_key(&session, &store, &key);
        assert!(session.tokens().is_empty());
        assert_eq!(store.get(&key).unwrap(), None);
        assert_eq!(store.get("entra:other").unwrap().as_deref(), Some("RT2"));

        // Other methods keep nothing: nothing to forget.
        let cli = ConnectionConfig {
            azure_auth_method: Some(AzureAuthMethod::Interactive),
            ..config
        };
        assert_eq!(kept_key(&cli), None);
    }
}

/// The whole flow against a fake Entra ID: a local token endpoint and a
/// simulated browser calling the redirect URI.
#[cfg(test)]
mod flow_tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::engine::secrets::MemoryStore;

    const CLIENT: &str = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";

    fn config() -> ConnectionConfig {
        ConnectionConfig {
            name: "Prod Azure".into(),
            db_type: DatabaseType::Azure,
            host: Some("srv.database.windows.net".into()),
            azure_auth_method: Some(AzureAuthMethod::InteractiveBrowser),
            tenant_id: Some("contoso.com".into()),
            azure_client_id: Some(CLIENT.into()),
            database: "app".into(),
            ..Default::default()
        }
    }

    fn key() -> String {
        secret_key(&Target::for_config(&config()).unwrap())
    }

    // ----- Fake token endpoint -----

    /// A request received by the fake token endpoint.
    struct TokenRequest {
        path: String,
        form: HashMap<String, String>,
    }

    type Answer = dyn Fn(&HashMap<String, String>) -> (u16, String) + Send + Sync;

    /// Serve `answer` on a local port; its URL and the requests it got.
    async fn fake_authority(answer: Box<Answer>) -> (String, Arc<Mutex<Vec<TokenRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let requests = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (path, body) = read_http_request(&mut stream).await;
                let form: HashMap<String, String> = url::form_urlencoded::parse(body.as_bytes())
                    .into_owned()
                    .collect();
                let (status, json) = answer(&form);
                requests.lock().unwrap().push(TokenRequest { path, form });
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (url, seen)
    }

    /// Path and body of the HTTP request on `stream`.
    async fn read_http_request(stream: &mut TcpStream) -> (String, String) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while buf.len() < end + 4 + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let path = head.split_whitespace().nth(1).unwrap().to_string();
                return (path, String::from_utf8_lossy(&buf[end + 4..]).to_string());
            }
            assert!(n > 0, "connection closed before the headers");
        }
    }

    fn tokens_json(access: &str, refresh: &str) -> String {
        format!(
            r#"{{"token_type":"Bearer","expires_in":3599,"access_token":"{access}","refresh_token":"{refresh}"}}"#
        )
    }

    /// Entra ID as it should behave: the code gives `AT-code` / `RT-1`, each
    /// refresh `AT-<n>` / `RT-<n>` (rotation); `refused` refresh tokens get
    /// `invalid_grant`.
    fn entra(refused: &'static [&'static str]) -> Box<Answer> {
        let rotation = AtomicUsize::new(1);
        Box::new(move |form| {
            match form["grant_type"].as_str() {
            "authorization_code" if form["code"] == "the-code" => {
                (200, tokens_json("AT-code", "RT-1"))
            }
            "refresh_token" if !refused.contains(&form["refresh_token"].as_str()) => {
                let n = rotation.fetch_add(1, Ordering::SeqCst) + 1;
                (200, tokens_json(&format!("AT-{n}"), &format!("RT-{n}")))
            }
            _ => (
                400,
                r#"{"error":"invalid_grant","error_description":"AADSTS70008: expired","error_codes":[70008]}"#
                    .into(),
            ),
        }
        })
    }

    // ----- Simulated browser -----

    enum Behaviour {
        /// Signs in: favicon, then the redirect with the code and the state.
        SignIn,
        /// Calls the redirect URI with this query (`{state}` = the real one).
        Redirect(&'static str),
        /// Never comes back.
        Idle,
    }

    struct Browser {
        behaviour: Behaviour,
        opened: AtomicUsize,
        /// `code_challenge` of the last authorization URL.
        challenge: Mutex<Option<String>>,
        redirect_uri: Mutex<Option<String>>,
        favicon: Mutex<Option<u16>>,
        /// Status and HTML of the page shown after the redirect.
        page: Mutex<Option<(u16, String)>>,
        /// Run in the browser just before it follows the redirect.
        before_redirect: Mutex<Option<Box<dyn FnOnce() + Send>>>,
        thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    }

    impl Browser {
        fn new(behaviour: Behaviour) -> Arc<Self> {
            Arc::new(Self {
                behaviour,
                opened: AtomicUsize::new(0),
                challenge: Mutex::new(None),
                redirect_uri: Mutex::new(None),
                favicon: Mutex::new(None),
                page: Mutex::new(None),
                before_redirect: Mutex::new(None),
                thread: Mutex::new(None),
            })
        }

        fn open(self: &Arc<Self>, url: &str) {
            self.opened.fetch_add(1, Ordering::SeqCst);
            let q: HashMap<String, String> = Url::parse(url)
                .unwrap()
                .query_pairs()
                .into_owned()
                .collect();
            *self.challenge.lock().unwrap() = Some(q["code_challenge"].clone());
            *self.redirect_uri.lock().unwrap() = Some(q["redirect_uri"].clone());
            let query = match self.behaviour {
                Behaviour::SignIn => "code=the-code&state={state}",
                Behaviour::Redirect(query) => query,
                Behaviour::Idle => return,
            };
            let query = query.replace("{state}", &q["state"]);
            let redirect_uri = q["redirect_uri"].clone();
            let signs_in = matches!(self.behaviour, Behaviour::SignIn);
            let me = self.clone();
            let before_redirect = self.before_redirect.lock().unwrap().take();
            let thread = std::thread::spawn(move || {
                if let Some(f) = before_redirect {
                    f();
                }
                let agent = ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .build()
                    .new_agent();
                let get = |url: String| {
                    let mut r = agent.get(&url).call().unwrap();
                    (r.status().as_u16(), r.body_mut().read_to_string().unwrap())
                };
                if signs_in {
                    *me.favicon.lock().unwrap() =
                        Some(get(format!("{redirect_uri}/favicon.ico")).0);
                }
                *me.page.lock().unwrap() = Some(get(format!("{redirect_uri}/?{query}")));
            });
            *self.thread.lock().unwrap() = Some(thread);
        }

        /// Wait for the simulated browser to have read its page.
        fn finish(&self) {
            if let Some(t) = self.thread.lock().unwrap().take() {
                t.join().unwrap();
            }
        }
    }

    // ----- Harness -----

    struct Harness {
        authority: String,
        requests: Arc<Mutex<Vec<TokenRequest>>>,
        browser: Arc<Browser>,
        events: Arc<Mutex<Vec<Interaction>>>,
        /// Seconds added to the real clock.
        clock: Arc<AtomicU64>,
        store: &'static MemoryStore,
        session: &'static Session,
        timeout: Duration,
    }

    impl Harness {
        async fn new(answer: Box<Answer>, behaviour: Behaviour) -> Self {
            let (authority, requests) = fake_authority(answer).await;
            Self {
                authority,
                requests,
                browser: Browser::new(behaviour),
                events: Arc::default(),
                clock: Arc::default(),
                store: Box::leak(Box::default()),
                session: Box::leak(Box::default()),
                timeout: Duration::from_secs(20),
            }
        }

        fn deps(&self) -> Deps<'_> {
            let browser = self.browser.clone();
            let events = self.events.clone();
            let clock = self.clock.clone();
            Deps {
                authority: Some(self.authority.clone()),
                open_browser: Box::new(move |url| browser.open(url)),
                notify: Box::new(move |i| events.lock().unwrap().push(i)),
                store: self.store,
                session: self.session,
                now: Box::new(move || {
                    SystemTime::now() + Duration::from_secs(clock.load(Ordering::SeqCst))
                }),
                timeout: self.timeout,
            }
        }

        async fn token(&self) -> anyhow::Result<String> {
            token_with(&config(), &self.deps()).await
        }

        fn grants(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.form["grant_type"].clone())
                .collect()
        }

        fn stored(&self) -> Option<String> {
            self.store.get(&key()).unwrap()
        }

        fn events(&self) -> Vec<Interaction> {
            self.events.lock().unwrap().clone()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn browser_sign_in_then_cached_then_silent_refresh() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        assert_eq!(h.token().await.unwrap(), "AT-code");
        h.browser.finish();

        // The code was exchanged with the verifier of the challenge sent.
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        {
            let requests = h.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let r = &requests[0];
            assert_eq!(r.path, "/contoso.com/oauth2/v2.0/token");
            assert_eq!(r.form["grant_type"], "authorization_code");
            assert_eq!(r.form["code"], "the-code");
            assert_eq!(r.form["client_id"], CLIENT);
            assert_eq!(
                r.form["scope"],
                "https://database.windows.net/.default offline_access openid profile"
            );
            let redirect_uri = h.browser.redirect_uri.lock().unwrap().clone().unwrap();
            assert!(
                redirect_uri.starts_with("http://localhost:"),
                "{redirect_uri}"
            );
            assert_eq!(r.form["redirect_uri"], redirect_uri);
            let challenge = h.browser.challenge.lock().unwrap().clone().unwrap();
            assert_eq!(pkce_challenge(&r.form["code_verifier"]), challenge);
        }
        // The refresh token is kept; the browser got its pages.
        assert_eq!(h.stored().as_deref(), Some("RT-1"));
        assert_eq!(*h.browser.favicon.lock().unwrap(), Some(404));
        let (status, html) = h.browser.page.lock().unwrap().clone().unwrap();
        assert_eq!(status, 200);
        assert!(
            html.contains("Connexion réussie, vous pouvez fermer cet onglet."),
            "{html}"
        );
        // The interface was told when the wait started and ended.
        let events = h.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(
            matches!(&events[0], Interaction::Waiting { connection, url }
            if connection == "Prod Azure" && url.starts_with(&format!("{}/contoso.com/oauth2/v2.0/authorize?", h.authority)))
        );
        assert_eq!(
            events[1],
            Interaction::Done {
                connection: "Prod Azure".into()
            }
        );

        // Still valid: reused, no request.
        assert_eq!(h.token().await.unwrap(), "AT-code");
        assert_eq!(h.grants().len(), 1);

        // Expired: renewed silently, the refresh token rotated.
        h.clock.store(2 * 3600, Ordering::SeqCst);
        assert_eq!(h.token().await.unwrap(), "AT-2");
        assert_eq!(h.grants(), ["authorization_code", "refresh_token"]);
        assert_eq!(h.requests.lock().unwrap()[1].form["refresh_token"], "RT-1");
        assert_eq!(h.stored().as_deref(), Some("RT-2"));
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        assert_eq!(h.events().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_token_from_the_keyring_needs_no_browser() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        h.store.set(&key(), "RT-7").unwrap();
        assert_eq!(h.token().await.unwrap(), "AT-2");
        assert_eq!(h.grants(), ["refresh_token"]);
        assert_eq!(h.requests.lock().unwrap()[0].form["refresh_token"], "RT-7");
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 0);
        assert!(h.events().is_empty());
        assert_eq!(h.stored().as_deref(), Some("RT-2"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_refresh_token_goes_back_to_the_browser() {
        let h = Harness::new(entra(&["RT-old"]), Behaviour::SignIn).await;
        h.store.set(&key(), "RT-old").unwrap();
        assert_eq!(h.token().await.unwrap(), "AT-code");
        h.browser.finish();
        assert_eq!(h.grants(), ["refresh_token", "authorization_code"]);
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        assert_eq!(h.stored().as_deref(), Some("RT-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_state_is_rejected() {
        let h = Harness::new(
            entra(&[]),
            Behaviour::Redirect("code=the-code&state=forged"),
        )
        .await;
        let e = h.token().await.unwrap_err().to_string();
        h.browser.finish();
        assert_eq!(e, explain(&AuthError::StateMismatch));
        assert!(h.grants().is_empty(), "the code must not be exchanged");
        let (status, html) = h.browser.page.lock().unwrap().clone().unwrap();
        assert_eq!(status, 400);
        assert!(html.contains("state"), "{html}");
        assert!(matches!(h.events().last(), Some(Interaction::Done { .. })));
        assert_eq!(h.stored(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_in_the_browser() {
        let h = Harness::new(
            entra(&[]),
            Behaviour::Redirect(
                "error=access_denied&error_description=AADSTS65004%3A+User+declined&state={state}",
            ),
        )
        .await;
        let e = h.token().await.unwrap_err().to_string();
        h.browser.finish();
        assert_eq!(
            e,
            "Connexion annulée dans le navigateur. (AADSTS65004: User declined)"
        );
        let (_, html) = h.browser.page.lock().unwrap().clone().unwrap();
        assert!(
            html.contains("Connexion annulée dans le navigateur."),
            "{html}"
        );
        assert!(h.grants().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unfinished_sign_in_times_out() {
        let mut h = Harness::new(entra(&[]), Behaviour::Idle).await;
        h.timeout = Duration::from_millis(300);
        let e = h.token().await.unwrap_err().to_string();
        assert_eq!(
            e,
            "Connexion non terminée dans le navigateur après 5 minutes."
        );
        let events = h.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], Interaction::Waiting { .. }));
        assert!(matches!(events[1], Interaction::Done { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn token_endpoint_errors_are_explained() {
        let h = Harness::new(
            Box::new(|_| {
                (
                    400,
                    r#"{"error":"invalid_client","error_description":"AADSTS7000218: The request body must contain client_assertion or client_secret.","error_codes":[7000218]}"#
                        .into(),
                )
            }),
            Behaviour::SignIn,
        )
        .await;
        let e = h.token().await.unwrap_err().to_string();
        h.browser.finish();
        assert!(
            e.starts_with("Active « Autoriser les flux clients publics » dans l'application."),
            "{e}"
        );
        assert_eq!(h.stored(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unreachable_entra_id_does_not_open_the_browser() {
        let mut h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        // Nothing listens there.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        h.authority = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        h.store.set(&key(), "RT-1").unwrap();
        let e = h.token().await.unwrap_err().to_string();
        assert!(e.starts_with("Impossible de joindre Entra ID"), "{e}");
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 0);
        // A network failure isn't a refused token: it is kept.
        assert_eq!(h.stored().as_deref(), Some("RT-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn simultaneous_connections_share_one_sign_in() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        let (a, b) = tokio::join!(h.token(), h.token());
        h.browser.finish();
        assert_eq!(a.unwrap(), "AT-code");
        assert_eq!(b.unwrap(), "AT-code");
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        assert_eq!(h.grants(), ["authorization_code"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forget_during_a_sign_in_wins() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        let (session, store) = (h.session, h.store);
        let forgetting = Arc::new(Mutex::new(None));
        let handle = forgetting.clone();
        // The user forgets the connection while its sign-in is waiting for
        // the browser.
        *h.browser.before_redirect.lock().unwrap() = Some(Box::new(move || {
            let (started, has_started) = std::sync::mpsc::channel();
            let t = std::thread::spawn(move || {
                started.send(()).unwrap();
                forget_key(session, store, &key());
            });
            has_started.recv().unwrap();
            // Time for a forget that wouldn't wait to be over.
            std::thread::sleep(Duration::from_millis(50));
            *handle.lock().unwrap() = Some(t);
        }));
        assert_eq!(h.token().await.unwrap(), "AT-code");
        h.browser.finish();
        let forget = forgetting.lock().unwrap().take().unwrap();
        forget.join().unwrap();
        // The forget ran after the sign-in kept its tokens.
        assert!(session.tokens().is_empty());
        assert_eq!(h.stored(), None);
    }

    #[tokio::test]
    async fn redirect_listeners_share_one_loopback_port() {
        let (v4, v6) = bind_loopback().await.unwrap();
        let a = v4.local_addr().unwrap();
        assert!(a.is_ipv4() && a.ip().is_loopback(), "{a}");
        if let Some(v6) = v6 {
            let b = v6.local_addr().unwrap();
            assert!(b.is_ipv6() && b.ip().is_loopback(), "{b}");
            assert_eq!(a.port(), b.port());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_the_connection_ends_the_wait() {
        let h = Harness::new(entra(&[]), Behaviour::Idle).await;
        let deps = h.deps();
        let config = config();
        let sign_in = token_with(&config, &deps);
        // The app gives up after a moment (the future is dropped).
        let r = tokio::time::timeout(Duration::from_millis(200), sign_in).await;
        assert!(r.is_err());
        let events = h.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[1], Interaction::Done { .. }));
        // The listener is closed.
        let redirect_uri = h.browser.redirect_uri.lock().unwrap().clone().unwrap();
        let port = redirect_uri.rsplit(':').next().unwrap();
        assert!(std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_err());
    }
}
