//! Azure SQL sign-in with the user's own Entra ID account through the
//! browser (OAuth 2.0 authorization code + PKCE), like SSMS's "Active
//! Directory Interactive". Nothing to set up: without a typed tenant the
//! server tells its own (`fedauth_probe`), without an application
//! Microsoft's drivers' is used, as DataGrip does.

use std::collections::HashMap;
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
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
use super::fedauth_probe::{self, FedAuthInfo};

/// Application (client) ID of Microsoft's SQL Server drivers: the one the
/// JDBC driver (so DataGrip) signs in with for `ActiveDirectoryInteractive`,
/// public and preauthorized for Azure SQL. Used when a connection names no
/// application.
pub const DEFAULT_CLIENT_ID: &str = "7f98cb04-cd1e-40df-9140-3bf7e2cea4db";

/// Tenant of a connection that typed none and whose server didn't tell: the
/// account's own organization.
const ORGANIZATIONS: &str = "organizations";

/// Check the tenant and application (client) ID of an interactive
/// connection; a French message for the form otherwise.
///
/// Both are optional: without a tenant, the server's (FEDAUTHINFO) is used,
/// else `organizations`; without an application, `DEFAULT_CLIENT_ID`. A
/// typed tenant is a GUID, a domain or `organizations`, as a server's
/// (`is_tenant_name`); `common` and `consumers` are refused, as they let
/// personal Microsoft accounts in, which Azure SQL doesn't take.
pub fn validate_interactive(tenant: &str, client_id: &str) -> Result<(), String> {
    let tenant = tenant.trim();
    if is_personal_tenant(tenant) {
        return Err(format!(
            "Indique le tenant de ton organisation (GUID ou domaine) ou laisse-le vide, \
             pas « {} »",
            tenant.to_ascii_lowercase()
        ));
    }
    if !tenant.is_empty() && !is_tenant_name(tenant) {
        return Err("Le tenant doit être un GUID ou un nom de domaine \
                    (lettres, chiffres, « . » et « - »)"
            .into());
    }
    let client_id = client_id.trim();
    if !client_id.is_empty() && !is_guid(client_id) {
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
    /// The target of an interactive connection (checked by
    /// `validate_interactive`): cloud from its host, account from its
    /// username, application typed or `DEFAULT_CLIENT_ID`, and tenant typed,
    /// else its server's (`probed`, its FEDAUTHINFO answer: ignored unless
    /// it names a known Entra ID authority), else `organizations`.
    pub fn for_config(
        config: &ConnectionConfig,
        probed: Option<&FedAuthInfo>,
    ) -> Result<Target, String> {
        let tenant = config.tenant_id.as_deref().unwrap_or_default().trim();
        let client_id = config.azure_client_id.as_deref().unwrap_or_default().trim();
        validate_interactive(tenant, client_id)?;
        let cloud = AzureCloud::from_host(config.host.as_deref().unwrap_or_default());
        let from_server = probed.filter(|_| tenant.is_empty()).and_then(|info| {
            let (authority, tenant) = sts_authority(&info.sts_url)?;
            // The SPN only switches to another cloud's Azure SQL (a custom
            // DNS name of a sovereign-cloud server): a token for any other
            // resource would still be handed to the server.
            let resource = AzureCloud::from_sql_resource(&info.spn).unwrap_or(cloud);
            Some((authority, tenant, resource))
        });
        let (authority, tenant, resource) = from_server.unwrap_or_else(|| {
            let tenant = if tenant.is_empty() {
                ORGANIZATIONS
            } else {
                tenant
            };
            (
                cloud.authority_host().to_string(),
                tenant.to_string(),
                cloud,
            )
        });
        let client_id = if client_id.is_empty() {
            DEFAULT_CLIENT_ID.to_string()
        } else {
            client_id.to_ascii_lowercase()
        };
        let login_hint = config
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        Ok(Target {
            authority: format!("https://{authority}"),
            tenant,
            client_id,
            scope: format!("{} offline_access openid profile", resource.sql_scope()),
            login_hint,
        })
    }

    /// Whose application the sign-in uses.
    pub fn app(&self) -> SignInApp {
        if self.client_id == DEFAULT_CLIENT_ID {
            SignInApp::Microsoft
        } else {
            SignInApp::Own
        }
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

/// The tenant typed in `config`, if any.
fn typed_tenant(config: &ConnectionConfig) -> Option<&str> {
    config
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// Server of `config` (host as compared and asked: trimmed, lowercase,
/// without a trailing dot) and port.
fn server_of(config: &ConnectionConfig) -> (String, u16) {
    let host = config.host.as_deref().unwrap_or_default().trim();
    let host = host.strip_suffix('.').unwrap_or(host);
    (host.to_ascii_lowercase(), config.port.unwrap_or(1433))
}

/// Entra ID authorities a server's STS URL may name, and the host to use
/// for each (`login.windows.net` is the public cloud's old name).
const KNOWN_AUTHORITIES: [(&str, &str); 5] = [
    ("login.windows.net", "login.microsoftonline.com"),
    ("login.microsoftonline.com", "login.microsoftonline.com"),
    ("login.microsoftonline.us", "login.microsoftonline.us"),
    ("login.chinacloudapi.cn", "login.chinacloudapi.cn"),
    (
        "login.partner.microsoftonline.cn",
        "login.partner.microsoftonline.cn",
    ),
];

/// Authority host and tenant of a server's STS URL
/// (`https://login.windows.net/<tenant>`): only a known Entra ID authority
/// over HTTPS, and an organization's tenant (a GUID or a domain).
fn sts_authority(sts_url: &str) -> Option<(String, String)> {
    let url = Url::parse(sts_url.trim()).ok()?;
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let (_, authority) = KNOWN_AUTHORITIES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(host))?;
    let tenant = url.path_segments()?.next()?;
    (is_tenant_name(tenant) && !is_personal_tenant(tenant))
        .then(|| (authority.to_string(), tenant.to_string()))
}

/// Whether `tenant` reads as a GUID or a domain: letters, digits, `.` and
/// `-`, at least one letter or digit, at most 253 characters. Never `.` or
/// `..`: `url`'s `PathSegmentsMut::extend` drops such segments, so the
/// tenant would vanish from `Target::endpoint`'s path, and `oauth2` would
/// be read as the tenant.
fn is_tenant_name(tenant: &str) -> bool {
    tenant.len() <= 253
        && tenant.bytes().any(|b| b.is_ascii_alphanumeric())
        && tenant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

/// The host of `authority` when it is a known Entra ID authority other than
/// the public cloud's.
fn sovereign_authority(authority: &str) -> Option<&'static str> {
    let host = authority.strip_prefix("https://")?;
    KNOWN_AUTHORITIES
        .iter()
        .map(|(_, authority)| *authority)
        .find(|a| *a != AzureCloud::Public.authority_host() && a.eq_ignore_ascii_case(host))
}

/// Whether `tenant` names Entra ID's multi-tenant endpoints that let
/// personal Microsoft accounts in.
fn is_personal_tenant(tenant: &str) -> bool {
    ["common", "consumers"]
        .iter()
        .any(|t| t.eq_ignore_ascii_case(tenant))
}

/// Key of `t`'s refresh token in the memory cache and the OS keyring.
///
/// `organizations` names no tenant: its sign-ins differ per cloud, so a
/// sovereign cloud's carries its authority, `organizations@<host>`. The
/// public cloud's keeps its form, and the sign-ins kept under it.
pub fn secret_key(t: &Target) -> String {
    let mut tenant = t.tenant.to_lowercase();
    if tenant == ORGANIZATIONS {
        if let Some(host) = sovereign_authority(&t.authority) {
            tenant = format!("{ORGANIZATIONS}@{host}");
        }
    }
    format!(
        "entra:{}:{}:{}",
        tenant,
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

/// Whose application a sign-in uses (`Target::app`): Entra ID's errors
/// don't call for the same fix with Microsoft's drivers' application, which
/// nobody here registered, as with the organization's own.
///
/// `AuthError` has no `Display`: every message shown is worded for the
/// sign-in's application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInApp {
    /// `DEFAULT_CLIENT_ID`.
    Microsoft,
    /// The application typed in the connection.
    Own,
}

/// `err` as the user's message (`explain`).
fn failure(err: &AuthError, app: SignInApp) -> anyhow::Error {
    anyhow::Error::msg(explain(err, app))
}

/// French message for `err` in a sign-in with `app`, Entra ID's own message
/// in parentheses.
pub fn explain(err: &AuthError, app: SignInApp) -> String {
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
            let own = app == SignInApp::Own;
            // The redirect URI and the public client flows are settings of
            // the organization's application: Microsoft's has them right.
            let message = if (has(65001) || code == "consent_required") && own {
                "L'application n'a pas l'autorisation d'accéder à Azure SQL : un administrateur \
                 doit accorder la permission déléguée « Azure SQL Database / user_impersonation »."
            } else if has(65001) || code == "consent_required" {
                "Ton organisation n'autorise pas l'application des drivers Microsoft pour Azure \
                 SQL : demande à un administrateur d'accorder son consentement, ou indique \
                 l'ID client de l'application de ton organisation dans les options avancées \
                 de la connexion (champ ID app dans le terminal)."
            } else if has(50011) && own {
                "L'URI de redirection `http://localhost` n'est pas déclarée : ajoute-la dans la \
                 plateforme « Applications mobiles et de bureau » de l'application."
            } else if has(7000218) && own {
                "Active « Autoriser les flux clients publics » dans l'application."
            } else if has(700016) && own {
                "Application introuvable dans ce tenant : vérifie l'ID client et le tenant."
            } else if has(700016) {
                "Ce tenant ne connaît pas l'application des drivers Microsoft : vérifie le \
                 tenant dans les options avancées de la connexion (champ Tenant dans le \
                 terminal), ou laisse-le vide pour utiliser celui du serveur."
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
            "Réponse de connexion inattendue (state différent), ignorée : termine la connexion \
             dans l'onglet ouvert par storingUnicorns."
                .into()
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
/// Pause after the redirect listener failed to accept a connection.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);
/// Largest request accepted on the redirect listener.
const MAX_REQUEST: usize = 32 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole token request.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);

const SIGNED_IN: &str = "Connexion réussie, vous pouvez fermer cet onglet.";

/// How long a server isn't asked again after its FEDAUTHINFO probe failed
/// or gave nothing (meanwhile: `organizations`). An answer is kept for the
/// session.
const PROBE_RETRY: Duration = Duration::from_secs(10 * 60);

/// A server's answer to the FEDAUTHINFO probe (`None`: it failed or gave
/// none) and when it came.
#[derive(Clone)]
struct Probed {
    at: SystemTime,
    info: Option<FedAuthInfo>,
}

/// Key of a server in `Session::servers`.
fn server_key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.to_ascii_lowercase())
}

/// Access tokens of the session, one lock per target (or server probed),
/// the servers' answers to the FEDAUTHINFO probe, and the tenants noted in
/// the keyring (`note_tenant`).
#[derive(Default)]
struct Session {
    tokens: Mutex<HashMap<String, Tokens>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    servers: Mutex<HashMap<String, Probed>>,
    /// `kept_key` → tenant written under it.
    noted: Mutex<HashMap<String, String>>,
    /// `organizations` key → tenant its sign-in landed in (`note_tid`).
    tids: Mutex<HashMap<String, String>>,
}

impl Session {
    fn tokens(&self) -> MutexGuard<'_, HashMap<String, Tokens>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn servers(&self) -> MutexGuard<'_, HashMap<String, Probed>> {
        self.servers.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn noted(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.noted.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tids(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.tids.lock().unwrap_or_else(|e| e.into_inner())
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

/// The FEDAUTHINFO probe of a server (`fedauth_probe::probe`).
type Probe = Box<
    dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<FedAuthInfo>>> + Send>>
        + Send
        + Sync,
>;

/// What the flow talks to, replaced in tests.
struct Deps<'a> {
    /// Replaces the target's authority (its tenant stays) in the URLs
    /// requested (`at_authority`), never in its keys.
    authority: Option<String>,
    /// Asks a server for its tenant.
    probe: Probe,
    open_browser: Box<dyn Fn(&str) + Send + Sync>,
    notify: Box<dyn Fn(Interaction) + Send + Sync>,
    store: &'static dyn SecretStore,
    session: &'a Session,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    timeout: Duration,
    /// Whether the browser may be opened (see `token`).
    interactive: bool,
}

impl Deps<'_> {
    /// `t` with the authority its URLs are built with: its own, unless
    /// replaced (`authority`). Only for URLs: its keyring keys
    /// (`secret_key`) stay those of its own authority.
    fn at_authority(&self, t: &Target) -> Target {
        let mut t = t.clone();
        if let Some(authority) = &self.authority {
            t.authority = authority.clone();
        }
        t
    }
}

/// The error of a reconnection that would need the browser.
pub fn session_expired(connection: &str) -> String {
    format!("Session Azure AD expirée : reconnectez-vous à {connection}")
}

/// An access token for Azure SQL with the Entra ID account of `config`
/// (`AzureAuthMethod::InteractiveBrowser`): the session's while it is valid,
/// else renewed with the refresh token (memory, then OS keyring), else
/// through the browser. The error is a French message for the user.
///
/// Only a connection the user opens (or tests) is `interactive`: a
/// reconnection (a shared client replaced after an interrupted operation, a
/// transaction's dedicated session) runs under other operations, where a
/// browser wait could neither be shown nor cancelled; it fails with
/// `session_expired` instead.
pub async fn token(config: &ConnectionConfig, interactive: bool) -> anyhow::Result<String> {
    // The first use probes the OS keyring, which may block.
    let store = tokio::task::spawn_blocking(secrets::store).await?;
    let deps = Deps {
        authority: None,
        probe: Box::new(|host, port| {
            Box::pin(async move { fedauth_probe::probe(&host, port).await })
        }),
        open_browser: Box::new(open_in_browser),
        notify: Box::new(emit),
        store,
        session: session(),
        now: Box::new(SystemTime::now),
        timeout: SIGN_IN_TIMEOUT,
        interactive,
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
    let resolved = resolve_target(config, deps)
        .await
        .map_err(anyhow::Error::msg)?;
    let mut target = resolved.clone();
    if typed_tenant(config).is_none() {
        target = with_kept_sign_in(config, target, deps).await;
    }
    // A borrowed organizations sign-in never goes back to the browser…
    let borrowed = !target.tenant.eq_ignore_ascii_case(&resolved.tenant);
    let access = match acquire(config, &target, deps, !borrowed).await? {
        Some(access) => access,
        // …the server's tenant does.
        None => {
            tracing::info!(
                "Entra ID: the borrowed sign-in of {} is gone, signing in to its server's tenant",
                config.name
            );
            target = resolved.clone();
            acquire(config, &target, deps, true)
                .await?
                .ok_or_else(|| anyhow::anyhow!(session_expired(&config.name)))?
        }
    };
    // The server's tenant, even when the organizations sign-in that landed
    // there serves it.
    note_tenant(config, &resolved, deps).await;
    note_tid(&target, &access, deps).await;
    Ok(access)
}

/// An access token for `target`: the session's, renewed, or through the
/// browser (see `token`); `None` when the browser is needed but not
/// `browser`.
async fn acquire(
    config: &ConnectionConfig,
    target: &Target,
    deps: &Deps<'_>,
    browser: bool,
) -> anyhow::Result<Option<String>> {
    let key = secret_key(target);
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
                return Ok(Some(t.access));
            }
        }
        Next::Refresh(refresh) => match redeem(target, Grant::Refresh(&refresh), deps).await {
            Ok(tokens) => {
                tracing::info!("Entra ID token renewed for {}", config.name);
                return Ok(Some(keep(deps, &key, tokens, Some(refresh)).await));
            }
            Err(e) if needs_interaction(&e) => {
                tracing::info!(
                    "Entra ID refresh token of {} refused, signing in again",
                    config.name
                );
                deps.session.tokens().remove(&key);
                delete_secret(deps.store, &key).await;
                // With the tenant an organizations sign-in landed in: a
                // borrowed one isn't signed in again, which would replace it.
                if is_organizations_key(&key) {
                    deps.session.tids().remove(&key);
                    delete_secret(deps.store, &tid_note(&key)).await;
                }
            }
            Err(e) => return Err(failure(&e, target.app())),
        },
        Next::Interactive => {}
    }
    if !deps.interactive {
        tracing::info!("Entra ID sign-in of {} needs the browser", config.name);
        anyhow::bail!(session_expired(&config.name));
    }
    if !browser {
        return Ok(None);
    }
    let tokens = sign_in(&config.name, target, deps).await?;
    tracing::info!("Entra ID sign-in completed for {}", config.name);
    Ok(Some(keep(deps, &key, tokens, None).await))
}

/// For a connection without a typed tenant, note in the keyring, under
/// `kept_key`, its server's tenant (`organizations` while the server didn't
/// answer; not a secret, see `note_value`): `forget` finds its refresh token
/// without asking the server, and a sign-in goes on when the server stops
/// answering (`noted_server_answer`).
async fn note_tenant(config: &ConnectionConfig, target: &Target, deps: &Deps<'_>) {
    if typed_tenant(config).is_some() {
        return;
    }
    let Some(note) = kept_key(config) else {
        return;
    };
    let value = note_value(config, target);
    if deps.session.noted().get(&note) == Some(&value) {
        return;
    }
    save_secret(deps.store, &note, value.clone()).await;
    deps.session.noted().insert(note, value);
}

/// What a connection's note says of `target`: its tenant, with
/// `@<authority host>` when the authority isn't the one of the cloud the
/// connection's host names (a custom DNS name of a sovereign-cloud server).
fn note_value(config: &ConnectionConfig, target: &Target) -> String {
    let tenant = target.tenant.to_lowercase();
    let cloud = AzureCloud::from_host(config.host.as_deref().unwrap_or_default());
    let host = target
        .authority
        .strip_prefix("https://")
        .unwrap_or(&target.authority);
    if tenant == ORGANIZATIONS || host.eq_ignore_ascii_case(cloud.authority_host()) {
        tenant
    } else {
        format!("{tenant}@{}", host.to_ascii_lowercase())
    }
}

/// The server's answer a note (`note_value`) stands for; `None` for
/// `organizations`, which names no tenant, and for a note naming no known
/// authority or no tenant name (`../x` would read as `x` in a URL).
fn noted_answer(config: &ConnectionConfig, value: &str) -> Option<FedAuthInfo> {
    let cloud = AzureCloud::from_host(config.host.as_deref().unwrap_or_default());
    let (tenant, host) = value
        .split_once('@')
        .unwrap_or((value, cloud.authority_host()));
    // The cloud is read from the note's text, the authority from the URL
    // built with it: both must name the same host, so only a known one,
    // exactly (`%6Cogin.microsoftonline.us` would only read as one in a URL).
    let (host, _) = KNOWN_AUTHORITIES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(host))?;
    if !is_tenant_name(tenant) || tenant.eq_ignore_ascii_case(ORGANIZATIONS) {
        return None;
    }
    Some(FedAuthInfo {
        sts_url: format!("https://{host}/{tenant}"),
        spn: format!("{}/", cloud_of_authority(host).sql_resource()),
    })
}

/// The target a note (`note_value`) stands for, checked as a server's
/// answer: `organizations` for one that names no tenant or doesn't read.
fn noted_target(config: &ConnectionConfig, value: &str) -> Option<Target> {
    Target::for_config(config, noted_answer(config, value).as_ref()).ok()
}

/// The cloud of an Entra ID authority host (`KNOWN_AUTHORITIES`).
fn cloud_of_authority(host: &str) -> AzureCloud {
    let is = |name: &str| host.eq_ignore_ascii_case(name);
    if is(AzureCloud::UsGovernment.authority_host()) {
        AzureCloud::UsGovernment
    } else if is(AzureCloud::China.authority_host()) || is("login.partner.microsoftonline.cn") {
        AzureCloud::China
    } else {
        AzureCloud::Public
    }
}

/// The answer `config`'s server gave before, as noted, for when it doesn't
/// answer now: its tenant rather than `organizations`, so that its refresh
/// token keeps working.
async fn noted_server_answer(config: &ConnectionConfig, deps: &Deps<'_>) -> Option<FedAuthInfo> {
    let note = kept_key(config)?;
    let in_session = deps.session.noted().get(&note).cloned();
    let value = match in_session {
        Some(value) => value,
        None => load_secret(deps.store, &note).await?,
    };
    let answer = noted_answer(config, &value)?;
    tracing::info!(
        "Entra ID: the server of {} didn't answer, signing in to its noted tenant",
        config.name
    );
    Some(answer)
}

/// The target of `config`, its server asked for its tenant (FEDAUTHINFO)
/// when none is typed.
async fn resolve_target(config: &ConnectionConfig, deps: &Deps<'_>) -> Result<Target, String> {
    // Checked first: an invalid connection asks nothing.
    let without_server = Target::for_config(config, None)?;
    if typed_tenant(config).is_some() {
        return Ok(without_server);
    }
    let (host, port) = server_of(config);
    let answer = match server_answer(&host, port, deps).await {
        Some(answer) => Some(answer),
        None => noted_server_answer(config, deps).await,
    };
    Target::for_config(config, answer.as_ref())
}

/// `target`, unless it holds no sign-in while the `organizations` one of the
/// same application and account does and landed in the very tenant the
/// server names (`note_tid`): a connection that signed in while its server
/// didn't answer keeps that sign-in once the server answers, but a guest
/// tenant never borrows the home one.
async fn with_kept_sign_in(config: &ConnectionConfig, target: Target, deps: &Deps<'_>) -> Target {
    let Ok(fallback) = Target::for_config(config, None) else {
        return target;
    };
    let fallback_key = secret_key(&fallback);
    if target.tenant.eq_ignore_ascii_case(&fallback.tenant)
        || !is_guid(&target.tenant)
        || holds_sign_in(&secret_key(&target), deps).await
        || !same_tenant(
            &target.tenant,
            noted_tid(&fallback_key, deps).await.as_deref(),
        )
        || !holds_sign_in(&fallback_key, deps).await
    {
        return target;
    }
    tracing::info!(
        "Entra ID: {} keeps the sign-in made while its server didn't answer",
        config.name
    );
    fallback
}

/// The tenant (`tid` claim) of an access token, read without checking its
/// signature: it only tells where a sign-in under `organizations` landed,
/// and the token came straight from the token endpoint over TLS. `None`
/// when missing or malformed.
fn token_tid(access: &str) -> Option<String> {
    let payload = access.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let tid = claims.get("tid")?.as_str()?;
    is_guid(tid).then(|| tid.to_ascii_lowercase())
}

/// Keyring note of the tenant the `organizations` sign-in under `key`
/// landed in (not a secret).
fn tid_note(key: &str) -> String {
    format!("entra:tid:{}", key.strip_prefix("entra:").unwrap_or(key))
}

/// Whether `key` is an `organizations` sign-in's (`secret_key`).
fn is_organizations_key(key: &str) -> bool {
    key.starts_with(&format!("entra:{ORGANIZATIONS}:"))
        || key.starts_with(&format!("entra:{ORGANIZATIONS}@"))
}

/// Whether a sign-in that landed in tenant `tid` serves a server naming
/// `tenant`: the same tenant, both GUIDs (a domain can't be compared without
/// the network).
fn same_tenant(tenant: &str, tid: Option<&str>) -> bool {
    is_guid(tenant) && tid.is_some_and(|tid| tid.eq_ignore_ascii_case(tenant))
}

/// For a sign-in kept under `organizations`, note the tenant it landed in
/// (`tid` of `access`) next to it in the keyring, and in the session.
async fn note_tid(target: &Target, access: &str, deps: &Deps<'_>) {
    if !target.tenant.eq_ignore_ascii_case(ORGANIZATIONS) {
        return;
    }
    let Some(tid) = token_tid(access) else {
        return;
    };
    let key = secret_key(target);
    if deps.session.tids().get(&key) == Some(&tid) {
        return;
    }
    save_secret(deps.store, &tid_note(&key), tid.clone()).await;
    deps.session.tids().insert(key, tid);
}

/// The tenant the `organizations` sign-in under `key` landed in, as noted.
async fn noted_tid(key: &str, deps: &Deps<'_>) -> Option<String> {
    let known = deps.session.tids().get(key).cloned();
    if known.is_some() {
        return known;
    }
    load_secret(deps.store, &tid_note(key)).await
}

/// Whether `key` holds a session token or a refresh token.
async fn holds_sign_in(key: &str, deps: &Deps<'_>) -> bool {
    if deps.session.tokens().contains_key(key) {
        return true;
    }
    load_secret(deps.store, key)
        .await
        .is_some_and(|refresh| !refresh.is_empty())
}

/// The FEDAUTHINFO of `host:port`: remembered (an answer for the session, a
/// failure for `PROBE_RETRY`), else asked, one probe at a time per server.
/// A reconnection (`!deps.interactive`) never asks again: what the server
/// said when the user connected decided where the sign-in is.
async fn server_answer(host: &str, port: u16, deps: &Deps<'_>) -> Option<FedAuthInfo> {
    if host.is_empty() {
        return None;
    }
    let key = server_key(host, port);
    let lock = deps.session.lock_for(&format!("probe:{key}"));
    let _turn = lock.lock().await;
    let now = (deps.now)();
    if let Some(known) = deps.session.servers().get(&key) {
        let stale = matches!(now.duration_since(known.at), Ok(age) if age >= PROBE_RETRY);
        if known.info.is_some() || !stale || !deps.interactive {
            return known.info.clone();
        }
    }
    let info = match (deps.probe)(host.to_string(), port).await {
        Ok(Some(info)) => {
            tracing::info!("Entra ID authority of {host}: {}", info.sts_url);
            Some(info)
        }
        Ok(None) => {
            tracing::info!("{host} named no Entra ID authority: signing in with organizations");
            None
        }
        Err(e) => {
            tracing::warn!(
                "Could not ask {host}:{port} for its Entra ID authority ({e:#}): \
                 signing in with organizations"
            );
            None
        }
    };
    deps.session.servers().insert(
        key,
        Probed {
            at: now,
            info: info.clone(),
        },
    );
    info
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
            tracing::warn!("Could not read the Entra ID sign-in from the keyring: {e}");
            None
        }
        Err(e) => {
            tracing::warn!("Could not read the Entra ID sign-in from the keyring: {e}");
            None
        }
    }
}

async fn save_secret(store: &'static dyn SecretStore, key: &str, value: String) {
    let key = key.to_string();
    let saved = tokio::task::spawn_blocking(move || store.set(&key, &value)).await;
    if let Some(e) = blocking_error(saved) {
        tracing::warn!("Could not keep the Entra ID sign-in in the keyring: {e}");
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
    let url = deps.at_authority(t).endpoint("token").to_string();
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
    let url = authorize_url(
        &deps.at_authority(t),
        &redirect_uri,
        &pkce_challenge(&verifier),
        &state,
    );
    let app = t.app();

    let (code, mut browser) = {
        (deps.notify)(Interaction::Waiting {
            connection: connection.to_string(),
            url: url.clone(),
        });
        let _done = DoneOnDrop {
            notify: deps.notify.as_ref(),
            connection,
        };
        (deps.open_browser)(&url);
        let (v4, v6) = (&v4, v6.as_ref());
        let wait = wait_for_code(move || accept(v4, v6), &state, app);
        tokio::time::timeout(deps.timeout, wait)
            .await
            .unwrap_or(Err(AuthError::Timeout))
            .map_err(|e| failure(&e, app))?
    };
    let grant = Grant::Code {
        code: &code,
        redirect_uri: &redirect_uri,
        verifier: &verifier,
    };
    // The browser's page waits for the exchange (bounded by the token
    // request's timeout), so that it tells how the sign-in really ended.
    let tokens = redeem(t, grant, deps).await;
    match &tokens {
        Ok(_) => respond(&mut browser, "200 OK", &page(SIGNED_IN)).await,
        Err(e) => respond(&mut browser, "400 Bad Request", &page(&explain(e, app))).await,
    }
    tokens.map_err(|e| failure(&e, app))
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

/// Serve the connections `accept` gives (the redirect listener) until the
/// sign-in comes back: the code with the browser's connection, still to be
/// answered, or why it failed. Other requests (`/favicon.ico`…) get a 404,
/// redirects with another `state` (another tab, a forged request) a 400
/// while the wait goes on. Errors are worded for `app` in the browser.
async fn wait_for_code<A, F>(
    mut accept: A,
    state: &str,
    app: SignInApp,
) -> Result<(String, TcpStream), AuthError>
where
    A: FnMut() -> F,
    F: std::future::Future<Output = std::io::Result<TcpStream>>,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, TcpStream)>(4);
    loop {
        tokio::select! {
            accepted = accept() => match accepted {
                Ok(mut stream) => {
                    let tx = tx.clone();
                    // Each request on its own: a browser's speculative
                    // connection that sends nothing mustn't hold up the
                    // redirect.
                    tokio::spawn(async move {
                        let read =
                            tokio::time::timeout(REQUEST_TIMEOUT, read_request_target(&mut stream));
                        if let Ok(Ok(Some(target))) = read.await {
                            let _ = tx.send((target, stream)).await;
                        }
                    });
                }
                // E.g. out of file descriptors for a moment: the redirect
                // may still come.
                Err(e) => {
                    tracing::warn!("Sign-in redirect listener could not accept a connection: {e}");
                    tokio::time::sleep(ACCEPT_RETRY).await;
                }
            },
            Some((target, mut stream)) = rx.recv() => match parse_callback(&target, state) {
                Err(AuthError::NotACallback) => respond(&mut stream, "404 Not Found", "").await,
                Err(AuthError::StateMismatch) => {
                    let message = explain(&AuthError::StateMismatch, app);
                    respond(&mut stream, "400 Bad Request", &page(&message)).await;
                }
                // Answered once the code is exchanged.
                Ok(code) => return Ok((code, stream)),
                Err(e) => {
                    respond(&mut stream, "400 Bad Request", &page(&explain(&e, app))).await;
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

/// The target of an interactive browser connection, its server not asked;
/// `None` for other connections, which keep nothing.
fn interactive_target(config: &ConnectionConfig) -> Option<Target> {
    let interactive = config.db_type == DatabaseType::Azure
        && config.azure_auth_method == Some(AzureAuthMethod::InteractiveBrowser);
    interactive
        .then(|| Target::for_config(config, None).ok())
        .flatten()
}

/// Which sign-in an interactive browser connection keeps (session token and
/// refresh token), known without the network; `None` for other
/// connections, which keep nothing.
///
/// With a typed tenant it is the keyring key, as in 0.14.0. Without, the
/// tenant comes from the server (FEDAUTHINFO), which isn't asked here: the
/// server stands for it, `entra:auto:<host>:<application>:<account>`. So
/// connections share a sign-in only when they surely do; two servers of one
/// tenant look distinct, and deleting one may forget the refresh token the
/// other also uses: one more browser sign-in, never a token left behind.
fn kept_key(config: &ConnectionConfig) -> Option<String> {
    let target = interactive_target(config)?;
    Some(if typed_tenant(config).is_some() {
        secret_key(&target)
    } else {
        let tenant = format!("auto:{}", server_of(config).0);
        secret_key(&Target { tenant, ..target })
    })
}

/// The keyring keys `config`'s sign-in may be under, known without the
/// network nor the keyring: its typed tenant's; else `organizations`, the
/// server's tenant if it answered in this session, and the tenant noted in
/// this session. Empty for connections that keep nothing.
fn known_keys(config: &ConnectionConfig, session: &Session) -> Vec<String> {
    let Some(target) = interactive_target(config) else {
        return Vec::new();
    };
    let mut keys = vec![secret_key(&target)];
    if typed_tenant(config).is_some() {
        return keys;
    }
    let (host, port) = server_of(config);
    let answer = session
        .servers()
        .get(&server_key(&host, port))
        .and_then(|p| p.info.clone());
    if let Some(t) = answer.and_then(|info| Target::for_config(config, Some(&info)).ok()) {
        keys.push(secret_key(&t));
    }
    let noted = kept_key(config).and_then(|note| session.noted().get(&note).cloned());
    if let Some(t) = noted.and_then(|value| noted_target(config, &value)) {
        keys.push(secret_key(&t));
    }
    let mut unique = Vec::new();
    for key in keys {
        if !unique.contains(&key) {
            unique.push(key);
        }
    }
    unique
}

/// The keyring keys `config` is known to sign in with, without the network:
/// its typed tenant's; else the one its server's answer in this session
/// leads to (`organizations` when it didn't answer), and the tenant noted in
/// this session or, given the `store`, in the keyring. Empty for
/// connections that keep nothing.
fn used_keys(
    config: &ConnectionConfig,
    session: &Session,
    store: Option<&dyn SecretStore>,
) -> Vec<String> {
    let Some(target) = interactive_target(config) else {
        return Vec::new();
    };
    if typed_tenant(config).is_some() {
        return vec![secret_key(&target)];
    }
    let mut targets = Vec::new();
    let (host, port) = server_of(config);
    let probed = session.servers().get(&server_key(&host, port)).cloned();
    if let Some(probed) = probed {
        targets.extend(Target::for_config(config, probed.info.as_ref()).ok());
    }
    let note = kept_key(config);
    let mut values: Vec<String> = note
        .as_ref()
        .and_then(|note| session.noted().get(note).cloned())
        .into_iter()
        .collect();
    if let (Some(note), Some(store)) = (&note, store) {
        values.extend(store.get(note).ok().flatten());
    }
    targets.extend(values.iter().filter_map(|v| noted_target(config, v)));
    let mut keys: Vec<String> = targets.iter().map(secret_key).collect();
    // The organizations sign-in, when it landed in one of these tenants
    // (`with_kept_sign_in`).
    let organizations = secret_key(&target);
    let mut tid = session.tids().get(&organizations).cloned();
    if let (None, Some(store)) = (&tid, store) {
        tid = store.get(&tid_note(&organizations)).ok().flatten();
    }
    if targets
        .iter()
        .any(|t| same_tenant(&t.tenant, tid.as_deref()))
    {
        keys.push(organizations);
    }
    keys
}

/// Forget `config`'s sign-in for good: every key known offline
/// (`known_keys`, and the tenant noted in the keyring), then the note;
/// `ask` (the server's FEDAUTHINFO) only as a last resort, when its tenant
/// is known neither way. Keys one of the `remaining` connections is known
/// to use stay (`used_keys`). Blocks (keyring, maybe the server): never
/// call it from async code.
fn forget_sign_in(
    config: &ConnectionConfig,
    remaining: &[ConnectionConfig],
    session: &Session,
    store: &dyn SecretStore,
    ask: &dyn Fn(&str, u16) -> Option<FedAuthInfo>,
) {
    if interactive_target(config).is_none() {
        return;
    }
    let used: Vec<String> = remaining
        .iter()
        .flat_map(|other| used_keys(other, session, Some(store)))
        .collect();
    let forget = |key: &str| {
        if used.iter().any(|u| u == key) {
            tracing::info!("Entra ID sign-in kept: another connection uses it");
        } else {
            forget_key(session, store, key);
        }
    };
    let mut keys = known_keys(config, session);
    let note = kept_key(config).filter(|_| typed_tenant(config).is_none());
    let mut tenant_known = note.is_none();
    if let Some(note) = &note {
        let (host, port) = server_of(config);
        tenant_known |= session
            .servers()
            .get(&server_key(&host, port))
            .is_some_and(|p| p.info.is_some());
        match store.get(note) {
            Ok(Some(value)) => {
                if let Some(t) = noted_target(config, &value) {
                    // An organizations note doesn't tell the server's tenant:
                    // a sign-in kept under it before may be left.
                    tenant_known |= !t.tenant.eq_ignore_ascii_case(ORGANIZATIONS);
                    let key = secret_key(&t);
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("Could not read the Entra ID sign-in's tenant: {e}"),
        }
    }
    for key in &keys {
        forget(key);
    }
    let Some(note) = note else {
        return;
    };
    if let Err(e) = store.delete(&note) {
        tracing::warn!("Could not delete the Entra ID sign-in's tenant from the keyring: {e}");
    }
    session.noted().remove(&note);
    if tenant_known {
        return;
    }
    let (host, port) = server_of(config);
    match ask(&host, port).and_then(|info| Target::for_config(config, Some(&info)).ok()) {
        Some(t) => forget(&secret_key(&t)),
        None => tracing::warn!(
            "Entra ID tenant of {host} unknown: a sign-in kept under it may stay in the keyring"
        ),
    }
}

/// The FEDAUTHINFO of `host:port`, waited for on this thread (`forget`'s).
fn ask_blocking(host: &str, port: u16) -> Option<FedAuthInfo> {
    match run_bounded(fedauth_probe::probe(host, port)) {
        Some(Ok(info)) => info,
        Some(Err(e)) => {
            tracing::warn!("Could not ask {host}:{port} for its Entra ID tenant: {e:#}");
            None
        }
        None => None,
    }
}

/// Run `future` on a runtime of this thread, shut down without waiting for
/// blocking work it left (a name resolution stuck in getaddrinfo): the
/// thread stays bounded by the future's own timeouts.
fn run_bounded<F: Future>(future: F) -> Option<F::Output> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let output = runtime.block_on(future);
    runtime.shutdown_background();
    Some(output)
}

/// Whether saving `new` over `old` must forget `old`'s sign-in: another
/// tenant (or server, when none is typed), application, account or method
/// (or no longer Azure).
pub fn needs_forget(old: &ConnectionConfig, new: &ConnectionConfig) -> bool {
    let kept = kept_key(old);
    kept.is_some() && kept != kept_key(new)
}

/// Whether `a` and `b` share one sign-in: interactive browser connections
/// with the same tenant (or server, when none is typed), application and
/// account (`kept_key`).
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
/// server keep it. Keys a remaining connection is known to use stay too
/// (`used_keys`: two servers of one tenant, say). The explicit "Oublier la
/// connexion Azure AD" calls `forget`.
pub fn forget_unless_used(config: &ConnectionConfig, remaining: &[ConnectionConfig]) {
    if orphaned(config, remaining) {
        forget_except(config, remaining.to_vec());
    }
}

/// Whether `config` keeps a sign-in (interactive browser connection) that
/// none of the `remaining` connections uses: nothing would forget it later.
pub fn orphaned(config: &ConnectionConfig, remaining: &[ConnectionConfig]) -> bool {
    kept_key(config).is_some() && !sign_in_used_by(config, remaining)
}

/// Forget the Entra ID sign-in of `config` (session token and refresh token
/// in the OS keyring): when the connection is deleted, changes target
/// (`needs_forget`) or on request. Nothing for other methods. Doesn't block:
/// the keyring is updated in the background.
pub fn forget(config: &ConnectionConfig) {
    forget_except(config, Vec::new());
}

/// `forget`, except what one of the `remaining` connections is known to use.
fn forget_except(config: &ConnectionConfig, remaining: Vec<ConnectionConfig>) {
    let session = session();
    let used: Vec<String> = remaining
        .iter()
        .flat_map(|other| used_keys(other, session, None))
        .collect();
    // At once, so that the next connection doesn't reuse it…
    for key in known_keys(config, session) {
        if !used.contains(&key) {
            session.tokens().remove(&key);
        }
    }
    // …and for good once an acquisition in progress is over. Waiting for it,
    // the keyring (Secret Service, an unlock prompt) and maybe the server
    // may block: not on the interface's thread.
    let config = config.clone();
    std::thread::spawn(move || {
        forget_sign_in(
            &config,
            &remaining,
            session,
            secrets::store(),
            &ask_blocking,
        );
    });
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
    // With the tenant an organizations sign-in landed in.
    if is_organizations_key(key) {
        session.tids().remove(key);
        if let Err(e) = store.delete(&tid_note(key)) {
            tracing::warn!("Could not delete the Entra ID sign-in's tenant from the keyring: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: &str = "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";

    #[test]
    fn tenant_and_client_id_are_optional() {
        for (tenant, client) in [
            ("", ""),
            ("  ", "  "),
            ("", CLIENT),
            ("contoso.com", ""),
            (" Organizations ", ""),
        ] {
            assert!(
                validate_interactive(tenant, client).is_ok(),
                "{tenant:?} {client:?}"
            );
        }
    }

    #[test]
    fn common_tenant_is_refused() {
        assert_eq!(
            validate_interactive("common", CLIENT).unwrap_err(),
            "Indique le tenant de ton organisation (GUID ou domaine) ou laisse-le vide, \
             pas « common »"
        );
        assert!(validate_interactive(" COMMON ", "").is_err());
        // Personal accounts only, as well.
        assert_eq!(
            validate_interactive(" Consumers ", "").unwrap_err(),
            "Indique le tenant de ton organisation (GUID ou domaine) ou laisse-le vide, \
             pas « consumers »"
        );
    }

    #[test]
    fn a_typed_tenant_is_a_guid_or_a_domain() {
        for good in [
            "contoso.com",
            " Contoso-EU.onmicrosoft.com ",
            "72f988bf-86f1-41af-91ab-2d7cd011db47",
            "organizations",
        ] {
            assert!(validate_interactive(good, "").is_ok(), "{good}");
        }
        let long = "a".repeat(254);
        for bad in [
            ".",
            "..",
            "-",
            "-.-",
            "contoso.com/evil",
            "con toso",
            "contoso.com?x=1",
            "a%2Fb",
            "contosö.com",
            long.as_str(),
        ] {
            assert_eq!(
                validate_interactive(bad, "").unwrap_err(),
                "Le tenant doit être un GUID ou un nom de domaine \
                 (lettres, chiffres, « . » et « - »)",
                "{bad}"
            );
        }
    }

    #[test]
    fn a_typed_client_id_must_be_a_guid() {
        for bad in ["my-app", "8f1c2d3e4a5b6c7d8e9f0a1b2c3d4e5f"] {
            assert_eq!(
                validate_interactive("contoso.com", bad).unwrap_err(),
                "L'ID client de l'application doit être un GUID",
                "{bad:?}"
            );
            assert!(validate_interactive("", bad).is_err());
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
        Target::for_config(&azure("srv.database.windows.net"), None).unwrap()
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
        let t = Target::for_config(&c, None).unwrap();
        assert_eq!(t.authority, "https://login.microsoftonline.us");
        assert!(t
            .scope
            .starts_with("https://database.usgovcloudapi.net/.default "));
        assert_eq!(t.login_hint.as_deref(), Some("jane@contoso.com"));

        let t = Target::for_config(&azure("srv.database.chinacloudapi.cn"), None).unwrap();
        assert_eq!(t.authority, "https://login.chinacloudapi.cn");

        c.username = Some("  ".into());
        assert_eq!(Target::for_config(&c, None).unwrap().login_hint, None);
    }

    #[test]
    fn target_is_validated() {
        let mut c = azure("srv.database.windows.net");
        c.tenant_id = Some("common".into());
        assert!(Target::for_config(&c, None)
            .unwrap_err()
            .contains("« common »"));
        let mut c = azure("srv.database.windows.net");
        c.azure_client_id = Some("my-app".into());
        assert!(Target::for_config(&c, None).unwrap_err().contains("GUID"));
    }

    const GUID: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

    /// A connection with neither tenant nor application.
    fn zero_config(host: &str) -> ConnectionConfig {
        ConnectionConfig {
            tenant_id: None,
            azure_client_id: Some("  ".into()),
            ..azure(host)
        }
    }

    fn answer(sts_url: &str, spn: &str) -> FedAuthInfo {
        FedAuthInfo {
            sts_url: sts_url.into(),
            spn: spn.into(),
        }
    }

    fn azure_sql_answer() -> FedAuthInfo {
        answer(
            &format!("https://login.windows.net/{GUID}"),
            "https://database.windows.net/",
        )
    }

    #[test]
    fn zero_config_target_uses_the_servers_tenant_and_microsofts_app() {
        let t = Target::for_config(
            &zero_config("srv.database.windows.net"),
            Some(&azure_sql_answer()),
        )
        .unwrap();
        // login.windows.net is the public cloud's old name.
        assert_eq!(t.authority, "https://login.microsoftonline.com");
        assert_eq!(t.tenant, GUID);
        assert_eq!(t.client_id, DEFAULT_CLIENT_ID);
        assert_eq!(
            t.scope,
            "https://database.windows.net/.default offline_access openid profile"
        );
        // A typed application with the server's tenant.
        let c = ConnectionConfig {
            azure_client_id: Some(CLIENT.to_uppercase()),
            ..zero_config("srv.database.windows.net")
        };
        let t = Target::for_config(&c, Some(&azure_sql_answer())).unwrap();
        assert_eq!((t.tenant.as_str(), t.client_id.as_str()), (GUID, CLIENT));
    }

    #[test]
    fn a_typed_tenant_wins_over_the_server() {
        let t = Target::for_config(
            &azure("srv.database.windows.net"),
            Some(&azure_sql_answer()),
        )
        .unwrap();
        assert_eq!(t, target());
        let c = ConnectionConfig {
            tenant_id: Some("organizations".into()),
            ..zero_config("srv.database.usgovcloudapi.net")
        };
        let t = Target::for_config(&c, Some(&azure_sql_answer())).unwrap();
        assert_eq!(t.tenant, "organizations");
        assert_eq!(t.authority, "https://login.microsoftonline.us");
    }

    #[test]
    fn without_the_servers_answer_the_tenant_is_organizations() {
        let t = Target::for_config(&zero_config("srv.database.usgovcloudapi.net"), None).unwrap();
        assert_eq!(t.authority, "https://login.microsoftonline.us");
        assert_eq!(t.tenant, "organizations");
        assert_eq!(t.client_id, DEFAULT_CLIENT_ID);
        assert!(t
            .scope
            .starts_with("https://database.usgovcloudapi.net/.default "));
    }

    #[test]
    fn known_authorities_of_the_server_are_kept() {
        for (sts, authority, tenant) in [
            (
                format!("https://login.microsoftonline.us/{GUID}"),
                "https://login.microsoftonline.us",
                GUID,
            ),
            (
                format!("https://login.chinacloudapi.cn/{GUID}/"),
                "https://login.chinacloudapi.cn",
                GUID,
            ),
            (
                format!("https://login.partner.microsoftonline.cn/{GUID}"),
                "https://login.partner.microsoftonline.cn",
                GUID,
            ),
            (
                "HTTPS://LOGIN.WINDOWS.NET/contoso.onmicrosoft.com".to_string(),
                "https://login.microsoftonline.com",
                "contoso.onmicrosoft.com",
            ),
            (
                format!("https://login.microsoftonline.com/{GUID}/oauth2/authorize"),
                "https://login.microsoftonline.com",
                GUID,
            ),
        ] {
            let info = answer(&sts, "https://database.windows.net/");
            let t =
                Target::for_config(&zero_config("srv.database.windows.net"), Some(&info)).unwrap();
            assert_eq!(
                (t.authority.as_str(), t.tenant.as_str()),
                (authority, tenant),
                "{sts}"
            );
        }
    }

    #[test]
    fn unknown_authorities_of_the_server_are_ignored() {
        for sts in [
            format!("https://evil.example.com/{GUID}"),
            format!("https://login.microsoftonline.com.evil.com/{GUID}"),
            format!("http://login.microsoftonline.com/{GUID}"),
            format!("https://login.microsoftonline.com:8443/{GUID}"),
            format!("https://user@login.microsoftonline.com/{GUID}"),
            "https://login.microsoftonline.com/".into(),
            "https://login.microsoftonline.com/common".into(),
            "https://login.microsoftonline.com/consumers".into(),
            "https://login.microsoftonline.com/a%2Fb".into(),
            "https://login.microsoftonline.com/con toso".into(),
            "https://login.microsoftonline.com/-".into(),
            "https://login.microsoftonline.com/.-.".into(),
            "not a url".into(),
        ] {
            let info = answer(&sts, "https://database.windows.net/");
            let t =
                Target::for_config(&zero_config("srv.database.windows.net"), Some(&info)).unwrap();
            assert_eq!(
                (t.authority.as_str(), t.tenant.as_str()),
                ("https://login.microsoftonline.com", "organizations"),
                "{sts}"
            );
        }
    }

    #[test]
    fn the_servers_spn_only_names_another_clouds_azure_sql() {
        let sts = format!("https://login.microsoftonline.us/{GUID}");
        // A custom DNS name of a US Government server.
        let gov = answer(&sts, "https://database.usgovcloudapi.net/");
        let t = Target::for_config(&zero_config("sql.contoso.internal"), Some(&gov)).unwrap();
        assert!(
            t.scope
                .starts_with("https://database.usgovcloudapi.net/.default "),
            "{}",
            t.scope
        );
        // Any other resource is ignored: its token would go to the server.
        for spn in [
            "https://graph.microsoft.com/",
            "https://management.azure.com",
            "",
        ] {
            let info = answer(&sts, spn);
            let t =
                Target::for_config(&zero_config("srv.database.windows.net"), Some(&info)).unwrap();
            assert!(
                t.scope
                    .starts_with("https://database.windows.net/.default "),
                "{spn}: {}",
                t.scope
            );
        }
    }

    #[test]
    fn a_note_names_a_known_authority_exactly() {
        let c = zero_config("srv.database.usgovcloudapi.net");
        // As `note_value` writes it, in any case.
        for note in [
            format!("{GUID}@login.microsoftonline.us"),
            format!("{GUID}@LOGIN.MicrosoftOnline.US"),
        ] {
            let info = noted_answer(&c, &note).unwrap();
            assert_eq!(
                info.sts_url,
                format!("https://login.microsoftonline.us/{GUID}"),
                "{note}"
            );
            assert_eq!(info.spn, "https://database.usgovcloudapi.net/", "{note}");
        }
        // A host that only a URL would read as a known one: no note.
        let note = format!("{GUID}@%6Cogin.microsoftonline.us");
        assert!(noted_answer(&c, &note).is_none());
        assert_eq!(noted_target(&c, &note), Target::for_config(&c, None).ok());
    }

    #[test]
    fn a_corrupt_note_stands_for_no_note() {
        for host in [
            "srv.database.windows.net",
            "srv.database.usgovcloudapi.net",
            "srv.database.chinacloudapi.cn",
            "sql.contoso.internal",
        ] {
            let c = zero_config(host);
            let cloud = AzureCloud::from_host(host);
            for note in [
                "t@evil.example",
                "t@login.microsoftonline.com@evil.example",
                "t@login.microsoftonline.us:444",
                "@x",
                "organizations@login.microsoftonline.us",
                "common@login.microsoftonline.us",
                "consumers",
                "../x",
                "x#",
                "x?y",
                "",
            ] {
                let t = noted_target(&c, note).unwrap();
                assert_eq!(
                    t.authority,
                    format!("https://{}", cloud.authority_host()),
                    "{host} {note:?}"
                );
                assert_eq!(t.tenant, ORGANIZATIONS, "{host} {note:?}");
                assert!(
                    t.scope.starts_with(&format!("{} ", cloud.sql_scope())),
                    "{host} {note:?}: {}",
                    t.scope
                );
            }
        }
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

    #[test]
    fn secret_keys_use_the_effective_tenant_and_application() {
        // 0.14.0's connections keep their key.
        assert_eq!(
            secret_key(&target()),
            "entra:contoso.com:8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f:-"
        );
        let mut c = zero_config("srv.database.windows.net");
        c.username = Some(" Jane@Contoso.com ".into());
        assert_eq!(
            secret_key(&Target::for_config(&c, Some(&azure_sql_answer())).unwrap()),
            format!("entra:{GUID}:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:jane@contoso.com")
        );
        assert_eq!(
            secret_key(&Target::for_config(&c, None).unwrap()),
            "entra:organizations:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:jane@contoso.com"
        );
    }

    #[test]
    fn kept_sign_ins_are_known_without_the_network() {
        // Typed tenant: the keyring key, as in 0.14.0.
        assert_eq!(
            kept_key(&azure("srv.database.windows.net")).unwrap(),
            "entra:contoso.com:8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f:-"
        );
        // Else the server stands for the tenant it will give.
        let mut c = zero_config(" SRV.database.windows.net. ");
        c.username = Some("jane@contoso.com".into());
        assert_eq!(
            kept_key(&c).unwrap(),
            "entra:auto:srv.database.windows.net:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:\
             jane@contoso.com"
        );
        // Other databases of the server share it, other servers don't (even
        // of the same tenant: unknown without asking them).
        let other_db = ConnectionConfig {
            name: "Reporting".into(),
            database: "reports".into(),
            host: Some("srv.database.windows.net".into()),
            ..c.clone()
        };
        assert!(same_sign_in(&c, &other_db));
        assert!(!needs_forget(&c, &other_db));
        let other_server = ConnectionConfig {
            host: Some("srv2.database.windows.net".into()),
            ..c.clone()
        };
        assert!(!same_sign_in(&c, &other_server));
        assert!(needs_forget(&c, &other_server));
        // Typing the tenant changes the sign-in.
        let typed = ConnectionConfig {
            tenant_id: Some(GUID.into()),
            ..c.clone()
        };
        assert!(!same_sign_in(&c, &typed));
        assert!(needs_forget(&c, &typed));
        assert!(orphaned(&c, &[other_server, typed]));
        assert!(!orphaned(&c, &[other_db]));
    }

    /// An access token whose payload is `claims` (signature not checked).
    fn token_with_claims(claims: &str) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(claims)
        )
    }

    #[test]
    fn the_tenant_of_an_access_token_is_its_tid() {
        let claims = format!(
            r#"{{"aud":"https://database.windows.net","tid":"{}"}}"#,
            GUID.to_uppercase()
        );
        assert_eq!(
            token_tid(&token_with_claims(&claims)).as_deref(),
            Some(GUID)
        );
        // Padded base64 too.
        let padded = format!(
            "x.{}.sig",
            base64::engine::general_purpose::URL_SAFE.encode(&claims)
        );
        assert_eq!(token_tid(&padded).as_deref(), Some(GUID));
        // Anything else: unknown, never an error.
        for bad in [
            token_with_claims(r#"{"aud":"x"}"#),
            token_with_claims(r#"{"tid":"contoso.com"}"#),
            token_with_claims(r#"{"tid":42}"#),
            token_with_claims("not json"),
            "AT-opaque".to_string(),
            "a.%%%.b".to_string(),
            String::new(),
        ] {
            assert_eq!(token_tid(&bad), None, "{bad}");
        }
    }

    #[test]
    fn a_home_sign_in_serves_only_its_own_tenant() {
        assert!(same_tenant(GUID, Some(&GUID.to_uppercase())));
        assert!(!same_tenant(
            GUID,
            Some("11111111-2222-3333-4444-555555555555")
        ));
        assert!(!same_tenant(GUID, None));
        // A domain can't be compared without the network.
        assert!(!same_tenant("contoso.com", Some("contoso.com")));
        assert_eq!(
            tid_note("entra:organizations:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:jane@contoso.com"),
            "entra:tid:organizations:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:jane@contoso.com"
        );
    }

    #[test]
    fn organizations_sign_ins_differ_per_cloud() {
        let key = |host: &str, tenant: Option<&str>| {
            let c = ConnectionConfig {
                tenant_id: tenant.map(Into::into),
                ..zero_config(host)
            };
            secret_key(&Target::for_config(&c, None).unwrap())
        };
        let app = DEFAULT_CLIENT_ID;
        // The public cloud's keeps its key.
        let public = key("srv.database.windows.net", None);
        assert_eq!(public, format!("entra:organizations:{app}:-"));
        let gov = key("srv.database.usgovcloudapi.net", None);
        assert_eq!(
            gov,
            format!("entra:organizations@login.microsoftonline.us:{app}:-")
        );
        assert_eq!(
            key("srv.database.chinacloudapi.cn", Some("Organizations")),
            format!("entra:organizations@login.chinacloudapi.cn:{app}:-")
        );
        for k in [&public, &gov] {
            assert!(is_organizations_key(k), "{k}");
        }
        assert_eq!(
            tid_note(&gov),
            format!("entra:tid:organizations@login.microsoftonline.us:{app}:-")
        );
        // A tenant's key doesn't change.
        assert_eq!(
            key("srv.database.usgovcloudapi.net", Some(GUID)),
            format!("entra:{GUID}:{app}:-")
        );
        assert!(!is_organizations_key(&format!("entra:{GUID}:{app}:-")));
    }

    #[test]
    fn keys_of_a_zero_config_sign_in_known_offline() {
        let session = Session::default();
        let mut c = zero_config("srv.database.windows.net");
        c.port = Some(1444);
        let organizations = secret_key(&Target::for_config(&c, None).unwrap());
        // The server's tenant is unknown.
        assert_eq!(known_keys(&c, &session), vec![organizations.clone()]);
        // Answered in this session: both keys it may have used.
        session.servers().insert(
            server_key("srv.database.windows.net", 1444),
            Probed {
                at: SystemTime::UNIX_EPOCH,
                info: Some(azure_sql_answer()),
            },
        );
        let tenant = secret_key(&Target::for_config(&c, Some(&azure_sql_answer())).unwrap());
        assert_eq!(
            known_keys(&c, &session),
            [organizations.clone(), tenant.clone()]
        );
        // Or noted when its sign-in was kept.
        let session = Session::default();
        session
            .noted()
            .insert(kept_key(&c).unwrap(), GUID.to_string());
        assert_eq!(known_keys(&c, &session), [organizations, tenant]);
        // Typed tenant: its key only.
        assert_eq!(
            known_keys(&azure("srv.database.windows.net"), &session),
            [secret_key(&target())]
        );
        // Other methods keep nothing.
        let cli = ConnectionConfig {
            azure_auth_method: Some(AzureAuthMethod::Interactive),
            ..c
        };
        assert!(known_keys(&cli, &session).is_empty());
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
        let t = Target::for_config(&c, None).unwrap();
        let url = authorize_url(&t, "http://localhost:1", "c", "s");
        assert!(
            url.starts_with("https://login.microsoftonline.us/Contoso.com/oauth2/v2.0/authorize?")
        );
        assert!(query(&url)["scope"].starts_with("https://database.usgovcloudapi.net/.default"));
        c.host = Some("srv.database.chinacloudapi.cn".into());
        let t = Target::for_config(&c, None).unwrap();
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
    fn a_dot_tenant_would_vanish_from_the_path() {
        // Why `is_tenant_name` refuses them.
        for dots in [".", ".."] {
            let mut t = target();
            t.tenant = dots.into();
            assert_eq!(t.endpoint("token").path(), "/oauth2/v2.0/token");
            assert!(!is_tenant_name(dots));
        }
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

    /// `err` explained for a sign-in with the organization's application.
    fn explain_own(err: &AuthError) -> String {
        explain(err, SignInApp::Own)
    }

    #[test]
    fn the_application_of_a_target_is_known() {
        let mut t = target();
        assert_eq!(t.app(), SignInApp::Own);
        t.client_id = DEFAULT_CLIENT_ID.into();
        assert_eq!(t.app(), SignInApp::Microsoft);
        let zero = Target::for_config(&zero_config("srv.database.windows.net"), None).unwrap();
        assert_eq!(zero.app(), SignInApp::Microsoft);
    }

    #[test]
    fn errors_with_microsofts_application_are_worded_for_it() {
        let microsoft = |e: &AuthError| explain(e, SignInApp::Microsoft);
        // Consent: the organization blocks it, or names its own.
        let consent = microsoft(&server(
            "invalid_grant",
            "AADSTS65001: The user or administrator has not consented.\r\nTrace ID: 1",
            &[65001],
        ));
        assert_eq!(
            consent,
            "Ton organisation n'autorise pas l'application des drivers Microsoft pour Azure SQL \
             : demande à un administrateur d'accorder son consentement, ou indique \
             l'ID client de l'application de ton organisation dans les options avancées de \
             la connexion (champ ID app dans le terminal). \
             (AADSTS65001: The user or administrator has not consented.)"
        );
        assert!(microsoft(&server("consent_required", "", &[]))
            .starts_with("Ton organisation n'autorise pas l'application des drivers Microsoft"));
        // Not found: the tenant is the one to check.
        assert!(microsoft(&server(
            "unauthorized_client",
            "AADSTS700016: Application not found",
            &[700016]
        ))
        .starts_with(
            "Ce tenant ne connaît pas l'application des drivers Microsoft : vérifie le tenant \
             dans les options avancées de la connexion (champ Tenant dans le terminal), ou \
             laisse-le vide pour utiliser celui du serveur."
        ));
        // Settings of one's own application: nothing for the user to fix.
        for (code, n) in [
            ("AADSTS50011: The redirect URI does not match", 50011),
            ("AADSTS7000218: client_assertion or client_secret", 7000218),
        ] {
            let e = microsoft(&server("invalid_client", code, &[n]));
            assert_eq!(e, format!("Échec de la connexion Azure AD. ({code})"));
        }
        // The same for both.
        let tenant = server("invalid_request", "AADSTS90002: Tenant not found", &[90002]);
        assert_eq!(microsoft(&tenant), explain_own(&tenant));
    }

    #[test]
    fn known_entra_errors_are_explained() {
        let consent = explain_own(&server(
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
        assert!(explain_own(&server("consent_required", "", &[]))
            .starts_with("L'application n'a pas l'autorisation"));
        assert!(explain_own(&server(
            "invalid_request",
            "AADSTS50011: The redirect URI does not match",
            &[]
        ))
        .starts_with(
            "L'URI de redirection `http://localhost` n'est pas déclarée : ajoute-la dans la \
             plateforme « Applications mobiles et de bureau » de l'application."
        ));
        assert!(explain_own(&server(
            "invalid_client",
            "AADSTS7000218: client_assertion or client_secret",
            &[7000218]
        ))
        .starts_with("Active « Autoriser les flux clients publics » dans l'application."));
        assert!(explain_own(&server(
            "unauthorized_client",
            "AADSTS700016: Application not found",
            &[700016]
        ))
        .starts_with("Application introuvable dans ce tenant : vérifie l'ID client et le tenant."));
        assert!(explain_own(&server(
            "invalid_request",
            "AADSTS90002: Tenant not found",
            &[90002]
        ))
        .starts_with("Tenant introuvable : vérifie le tenant."));
    }

    #[test]
    fn codes_are_matched_exactly() {
        // 70001 is none of the known codes (700016, 7000218…).
        let e = explain_own(&server("x", "AADSTS70001: other", &[70001]));
        assert!(e.starts_with("Échec de la connexion Azure AD"), "{e}");
        assert!(e.ends_with("(AADSTS70001: other)"), "{e}");
        // Without a description, the OAuth error code.
        assert_eq!(
            explain_own(&server("invalid_scope", "", &[])),
            "Échec de la connexion Azure AD. (invalid_scope)"
        );
        // A code only in error_codes is enough.
        assert!(explain_own(&server("invalid_client", "", &[7000218]))
            .starts_with("Active « Autoriser"));
    }

    #[test]
    fn cancel_timeout_and_other_failures_are_explained() {
        assert_eq!(
            explain_own(&AuthError::Denied {
                description: "AADSTS65004: User declined".into()
            }),
            "Connexion annulée dans le navigateur. (AADSTS65004: User declined)"
        );
        assert_eq!(
            explain_own(&AuthError::Denied {
                description: String::new()
            }),
            "Connexion annulée dans le navigateur."
        );
        assert_eq!(
            explain_own(&AuthError::Timeout),
            "Connexion non terminée dans le navigateur après 5 minutes."
        );
        assert!(explain_own(&AuthError::StateMismatch).contains("state"));
        assert!(explain_own(&AuthError::Network("refused".into())).contains("(refused)"));
        // Whatever the application.
        assert_eq!(
            explain(&AuthError::Timeout, SignInApp::Microsoft),
            explain_own(&AuthError::Timeout)
        );
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
        let key = secret_key(&Target::for_config(&config, None).unwrap());
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
        secret_key(&Target::for_config(&config(), None).unwrap())
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
        entra_in(refused, None)
    }

    /// An access token named `name` of an account of tenant `tid`.
    fn jwt(tid: &str, name: &str) -> String {
        format!(
            "{}.{}.{name}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(format!(r#"{{"tid":"{tid}"}}"#))
        )
    }

    /// `entra`, its access tokens of an account of tenant `tid` (`jwt`).
    fn entra_in(refused: &'static [&'static str], tid: Option<&'static str>) -> Box<Answer> {
        let rotation = AtomicUsize::new(1);
        let access = move |name: &str| tid.map_or(name.to_string(), |tid| jwt(tid, name));
        Box::new(move |form| {
            match form["grant_type"].as_str() {
            "authorization_code" if form["code"] == "the-code" => {
                (200, tokens_json(&access("AT-code"), "RT-1"))
            }
            "refresh_token" if !refused.contains(&form["refresh_token"].as_str()) => {
                let n = rotation.fetch_add(1, Ordering::SeqCst) + 1;
                (200, tokens_json(&access(&format!("AT-{n}")), &format!("RT-{n}")))
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
        /// Calls the redirect URI with these queries in turn (`{state}` =
        /// the real one).
        Redirects(&'static [&'static str]),
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
        /// Status and HTML of the pages shown after each redirect.
        pages: Mutex<Vec<(u16, String)>>,
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
                pages: Mutex::new(Vec::new()),
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
            let queries: Vec<String> = match self.behaviour {
                Behaviour::SignIn => vec!["code=the-code&state={state}".into()],
                Behaviour::Redirects(queries) => queries.iter().map(|q| q.to_string()).collect(),
                Behaviour::Idle => return,
            };
            let state = q["state"].clone();
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
                for query in queries {
                    let query = query.replace("{state}", &state);
                    let page = get(format!("{redirect_uri}/?{query}"));
                    me.pages.lock().unwrap().push(page);
                }
            });
            *self.thread.lock().unwrap() = Some(thread);
        }

        /// The last page shown.
        fn last_page(&self) -> (u16, String) {
            self.pages.lock().unwrap().last().cloned().unwrap()
        }

        /// Wait for the simulated browser to have read its pages.
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
        /// A connection (else a reconnection, which never opens the browser).
        interactive: bool,
        /// The server's answer to the FEDAUTHINFO probe…
        probed: Arc<Mutex<Result<Option<FedAuthInfo>, String>>>,
        /// …and the servers it was asked of.
        asked: Arc<Mutex<Vec<(String, u16)>>>,
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
                interactive: true,
                probed: Arc::new(Mutex::new(Ok(None))),
                asked: Arc::default(),
            }
        }

        fn deps(&self) -> Deps<'_> {
            let browser = self.browser.clone();
            let events = self.events.clone();
            let clock = self.clock.clone();
            let (probed, asked) = (self.probed.clone(), self.asked.clone());
            Deps {
                authority: Some(self.authority.clone()),
                probe: Box::new(move |host, port| {
                    asked.lock().unwrap().push((host, port));
                    let answer = probed.lock().unwrap().clone();
                    Box::pin(async move { answer.map_err(anyhow::Error::msg) })
                }),
                open_browser: Box::new(move |url| browser.open(url)),
                notify: Box::new(move |i| events.lock().unwrap().push(i)),
                store: self.store,
                session: self.session,
                now: Box::new(move || {
                    SystemTime::now() + Duration::from_secs(clock.load(Ordering::SeqCst))
                }),
                timeout: self.timeout,
                interactive: self.interactive,
            }
        }

        async fn token(&self) -> anyhow::Result<String> {
            self.token_for(&config()).await
        }

        async fn token_for(&self, config: &ConnectionConfig) -> anyhow::Result<String> {
            token_with(config, &self.deps()).await
        }

        fn probes(&self) -> usize {
            self.asked.lock().unwrap().len()
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
        let (status, html) = h.browser.last_page();
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
    async fn a_reconnection_never_opens_the_browser() {
        let mut h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        h.interactive = false;
        // Nothing kept: the user has to connect again.
        let e = h.token().await.unwrap_err().to_string();
        assert_eq!(e, session_expired("Prod Azure"));
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 0);
        assert!(h.grants().is_empty());
        assert!(h.events().is_empty(), "no wait to show");
        // A refresh token still renews it silently.
        h.store.set(&key(), "RT-7").unwrap();
        assert_eq!(h.token().await.unwrap(), "AT-2");
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_refresh_token_ends_a_reconnection() {
        let mut h = Harness::new(entra(&["RT-old"]), Behaviour::SignIn).await;
        h.interactive = false;
        h.store.set(&key(), "RT-old").unwrap();
        let e = h.token().await.unwrap_err().to_string();
        assert_eq!(e, session_expired("Prod Azure"));
        assert_eq!(h.grants(), ["refresh_token"]);
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 0);
        assert_eq!(h.stored(), None, "the refused token is dropped");
        assert!(h.events().is_empty());
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
    async fn wrong_state_is_refused_and_the_wait_goes_on() {
        let mut h = Harness::new(
            entra(&[]),
            Behaviour::Redirects(&["code=the-code&state=forged"]),
        )
        .await;
        h.timeout = Duration::from_millis(500);
        let e = h.token().await.unwrap_err().to_string();
        h.browser.finish();
        // Refused, then still waiting for the real redirect until the end.
        assert_eq!(e, explain(&AuthError::Timeout, SignInApp::Own));
        assert!(h.grants().is_empty(), "the code must not be exchanged");
        let (status, html) = h.browser.last_page();
        assert_eq!(status, 400);
        assert!(html.contains("state"), "{html}");
        assert!(matches!(h.events().last(), Some(Interaction::Done { .. })));
        assert_eq!(h.stored(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forged_redirect_then_the_real_one_signs_in() {
        let h = Harness::new(
            entra(&[]),
            Behaviour::Redirects(&["code=stolen&state=forged", "code=the-code&state={state}"]),
        )
        .await;
        assert_eq!(h.token().await.unwrap(), "AT-code");
        h.browser.finish();
        let pages = h.browser.pages.lock().unwrap().clone();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].0, 400);
        assert_eq!(pages[1].0, 200);
        assert_eq!(h.grants(), ["authorization_code"]);
        assert_eq!(h.requests.lock().unwrap()[0].form["code"], "the-code");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_in_the_browser() {
        let h = Harness::new(
            entra(&[]),
            Behaviour::Redirects(&[
                "error=access_denied&error_description=AADSTS65004%3A+User+declined&state={state}",
            ]),
        )
        .await;
        let e = h.token().await.unwrap_err().to_string();
        h.browser.finish();
        assert_eq!(
            e,
            "Connexion annulée dans le navigateur. (AADSTS65004: User declined)"
        );
        let (_, html) = h.browser.last_page();
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
        // The browser waited for the exchange: it says why it failed.
        let (status, html) = h.browser.last_page();
        assert_eq!(status, 400);
        assert!(
            html.contains("Active « Autoriser les flux clients publics » dans l'application."),
            "{html}"
        );
        assert!(!html.contains("Connexion réussie"), "{html}");
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
    async fn accept_errors_do_not_end_the_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let failures = AtomicUsize::new(2);
        let (listener, failures) = (&listener, &failures);
        let accept = move || {
            let fail = failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            async move {
                if fail {
                    Err(std::io::Error::other("too many open files"))
                } else {
                    listener.accept().await.map(|(s, _)| s)
                }
            }
        };
        let browser = std::thread::spawn(move || {
            let _ = ureq::get(&format!("http://127.0.0.1:{port}/?code=c&state=s")).call();
        });
        let waited = tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_code(accept, "s", SignInApp::Own),
        )
        .await
        .unwrap();
        let (code, browser_connection) = waited.unwrap();
        assert_eq!(code, "c");
        drop(browser_connection);
        assert_eq!(failures.load(Ordering::SeqCst), 0);
        browser.join().unwrap();
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
        let mut sign_in = Box::pin(token_with(&config, &deps));
        // Run it until it waits for the browser (however slow the machine)…
        let started = std::time::Instant::now();
        while !h
            .events()
            .iter()
            .any(|e| matches!(e, Interaction::Waiting { .. }))
        {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "never waited for the browser"
            );
            let step = tokio::time::timeout(Duration::from_millis(10), &mut sign_in).await;
            assert!(step.is_err(), "ended without the browser: {step:?}");
        }
        // …then the app gives up: the future is dropped.
        drop(sign_in);
        let events = h.events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[1], Interaction::Done { .. }));
        // The listener is closed.
        let redirect_uri = h.browser.redirect_uri.lock().unwrap().clone().unwrap();
        let port = redirect_uri.rsplit(':').next().unwrap();
        assert!(std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_err());
    }

    // ----- Without tenant nor application -----

    const TENANT: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

    fn zero_config() -> ConnectionConfig {
        ConnectionConfig {
            tenant_id: None,
            azure_client_id: None,
            username: Some("jane@contoso.com".into()),
            port: Some(1433),
            ..config()
        }
    }

    fn azure_sql() -> Result<Option<FedAuthInfo>, String> {
        Ok(Some(FedAuthInfo {
            sts_url: format!("https://login.windows.net/{TENANT}"),
            spn: "https://database.windows.net/".into(),
        }))
    }

    fn zero_config_key(tenant: &str) -> String {
        format!("entra:{tenant}:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:jane@contoso.com")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn zero_config_signs_in_with_the_servers_tenant_and_microsofts_app() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-code");
        h.browser.finish();
        assert_eq!(
            *h.asked.lock().unwrap(),
            [("srv.database.windows.net".to_string(), 1433)]
        );
        let events = h.events();
        let authorize = format!("{}/{TENANT}/oauth2/v2.0/authorize?", h.authority);
        assert!(
            matches!(&events[0], Interaction::Waiting { url, .. }
                if url.starts_with(&authorize)
                    && url.contains("client_id=7f98cb04-cd1e-40df-9140-3bf7e2cea4db")),
            "{events:?}"
        );
        {
            let requests = h.requests.lock().unwrap();
            assert_eq!(requests[0].path, format!("/{TENANT}/oauth2/v2.0/token"));
            assert_eq!(requests[0].form["client_id"], DEFAULT_CLIENT_ID);
        }
        assert_eq!(
            h.store.get(&zero_config_key(TENANT)).unwrap().as_deref(),
            Some("RT-1")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn errors_of_microsofts_application_are_worded_for_it() {
        const BLOCKED: &str = "Ton organisation n'autorise pas l'application des drivers Microsoft";
        // Refused at the token endpoint…
        let h = Harness::new(
            Box::new(|_| {
                (
                    400,
                    r#"{"error":"invalid_grant","error_description":"AADSTS65001: The user or administrator has not consented.","error_codes":[65001]}"#
                        .into(),
                )
            }),
            Behaviour::SignIn,
        )
        .await;
        *h.probed.lock().unwrap() = azure_sql();
        let e = h.token_for(&zero_config()).await.unwrap_err().to_string();
        h.browser.finish();
        assert!(e.starts_with(BLOCKED), "{e}");
        let (status, html) = h.browser.last_page();
        assert_eq!(status, 400);
        assert!(html.contains(BLOCKED), "{html}");

        // …or in the redirect.
        let h = Harness::new(
            entra(&[]),
            Behaviour::Redirects(&[
                "error=consent_required&error_description=AADSTS65001%3A+no+consent&state={state}",
            ]),
        )
        .await;
        *h.probed.lock().unwrap() = azure_sql();
        let e = h.token_for(&zero_config()).await.unwrap_err().to_string();
        h.browser.finish();
        assert!(e.starts_with(BLOCKED), "{e}");
        let (_, html) = h.browser.last_page();
        assert!(html.contains(BLOCKED), "{html}");
        assert!(h.grants().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_servers_answer_is_remembered() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        h.token_for(&zero_config()).await.unwrap();
        h.browser.finish();
        // Expired: renewed silently, without asking the server again.
        h.clock.store(2 * 3600, Ordering::SeqCst);
        *h.probed.lock().unwrap() = Err("unreachable".into());
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-2");
        assert_eq!(h.probes(), 1);
        assert_eq!(
            h.requests.lock().unwrap()[1].path,
            format!("/{TENANT}/oauth2/v2.0/token")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_the_servers_answer_organizations_signs_in() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-code");
        h.browser.finish();
        {
            let requests = h.requests.lock().unwrap();
            assert_eq!(requests[0].path, "/organizations/oauth2/v2.0/token");
            assert_eq!(requests[0].form["client_id"], DEFAULT_CLIENT_ID);
        }
        assert_eq!(
            h.store
                .get(&zero_config_key("organizations"))
                .unwrap()
                .as_deref(),
            Some("RT-1")
        );
        // The failure is remembered for a while…
        h.token_for(&zero_config()).await.unwrap();
        assert_eq!(h.probes(), 1);
        // …then the server is asked again. Where the organizations sign-in
        // landed is unknown (no tenant in its token): the server's tenant
        // signs in anew.
        h.clock.store(11 * 60, Ordering::SeqCst);
        *h.probed.lock().unwrap() = azure_sql();
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-code");
        h.browser.finish();
        assert_eq!(h.probes(), 2);
        assert_eq!(h.grants(), ["authorization_code", "authorization_code"]);
        assert_eq!(
            h.requests.lock().unwrap()[1].path,
            format!("/{TENANT}/oauth2/v2.0/token")
        );
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 2);
    }

    /// An access token of the account, whose home tenant is `TENANT`.
    fn home(name: &str) -> String {
        jwt(TENANT, name)
    }

    /// Signed in under `organizations` while the server didn't answer, with
    /// an account of `TENANT`.
    async fn signed_in_without_the_server() -> Harness {
        let h = Harness::new(entra_in(&[], Some(TENANT)), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), home("AT-code"));
        h.browser.finish();
        assert_eq!(
            h.store
                .get(&tid_note(&zero_config_key("organizations")))
                .unwrap()
                .as_deref(),
            Some(TENANT)
        );
        // Later on, the server answers.
        h.clock.store(11 * 60, Ordering::SeqCst);
        *h.probed.lock().unwrap() = azure_sql();
        h
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reconnection_keeps_the_servers_last_answer() {
        let mut h = signed_in_without_the_server().await;
        h.interactive = false;
        // Not asked again under other operations: the sign-in still works.
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), home("AT-code"));
        assert_eq!(h.probes(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_made_without_the_server_outlives_its_answer() {
        for interactive in [true, false] {
            let mut h = signed_in_without_the_server().await;
            h.interactive = interactive;
            // Only the keyring is left (the app was restarted): the tenant
            // the organizations sign-in landed in is read from there…
            restart(&h);
            // …and the server, asked again, names its tenant, which holds
            // nothing but where the organizations sign-in landed: that one
            // is renewed, no browser.
            assert_eq!(h.token_for(&zero_config()).await.unwrap(), home("AT-2"));
            assert_eq!(h.probes(), 2);
            assert_eq!(h.grants(), ["authorization_code", "refresh_token"]);
            assert_eq!(
                h.requests.lock().unwrap()[1].path,
                "/organizations/oauth2/v2.0/token"
            );
            assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
            assert_eq!(
                h.store
                    .get(&zero_config_key("organizations"))
                    .unwrap()
                    .as_deref(),
                Some("RT-2")
            );
            // Noted under the server's tenant, not organizations.
            assert_eq!(
                h.store.get(&note_of(&zero_config())).unwrap().as_deref(),
                Some(TENANT)
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_guest_tenant_never_borrows_the_home_sign_in() {
        const HOME: &str = "11111111-2222-3333-4444-555555555555";
        let h = Harness::new(entra_in(&[], Some(HOME)), Behaviour::SignIn).await;
        // Server A didn't answer: signed in under organizations (home
        // tenant).
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        h.token_for(&on_server("a.database.windows.net"))
            .await
            .unwrap();
        h.browser.finish();
        // Server B is in another tenant, where Jane is a guest.
        *h.probed.lock().unwrap() = azure_sql();
        let b = on_server("b.database.windows.net");
        h.token_for(&b).await.unwrap();
        h.browser.finish();
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 2);
        assert_eq!(h.grants(), ["authorization_code", "authorization_code"]);
        assert_eq!(
            h.requests.lock().unwrap()[1].path,
            format!("/{TENANT}/oauth2/v2.0/token")
        );
        assert_eq!(h.store.get(&note_of(&b)).unwrap().as_deref(), Some(TENANT));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_servers_tenant_wins_once_it_holds_a_sign_in() {
        let h = signed_in_without_the_server().await;
        h.session.tokens().clear();
        h.store.set(&zero_config_key(TENANT), "RT-7").unwrap();
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), home("AT-2"));
        assert_eq!(
            h.requests.lock().unwrap()[1].path,
            format!("/{TENANT}/oauth2/v2.0/token")
        );
        assert_eq!(h.requests.lock().unwrap()[1].form["refresh_token"], "RT-7");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_typed_tenant_never_asks_the_server() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        h.token().await.unwrap();
        h.browser.finish();
        assert_eq!(h.probes(), 0);
        assert_eq!(h.stored().as_deref(), Some("RT-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reconnection_asks_the_server_too() {
        let mut h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        h.interactive = false;
        *h.probed.lock().unwrap() = azure_sql();
        h.store.set(&zero_config_key(TENANT), "RT-7").unwrap();
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-2");
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 0);
        assert_eq!(h.probes(), 1);
    }

    // ----- Forgetting without a typed tenant -----

    /// The keyring entry noting the tenant of `config`'s sign-in.
    fn note_of(config: &ConnectionConfig) -> String {
        kept_key(config).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_tenant_of_a_sign_in_is_noted() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        h.token_for(&zero_config()).await.unwrap();
        h.browser.finish();
        assert_eq!(
            note_of(&zero_config()),
            "entra:auto:srv.database.windows.net:7f98cb04-cd1e-40df-9140-3bf7e2cea4db:\
             jane@contoso.com"
        );
        assert_eq!(
            h.store.get(&note_of(&zero_config())).unwrap().as_deref(),
            Some(TENANT)
        );
        // Without the server's answer: organizations.
        let h = signed_in_without_the_server().await;
        assert_eq!(
            h.store.get(&note_of(&zero_config())).unwrap().as_deref(),
            Some("organizations")
        );
        // A typed tenant needs no note.
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        h.token().await.unwrap();
        h.browser.finish();
        let c = ConnectionConfig {
            tenant_id: None,
            ..config()
        };
        assert_eq!(h.store.get(&note_of(&c)).unwrap(), None);
    }

    /// `forget_sign_in` of `config`, no other connection left, with a server
    /// answering `answer`; how many times it was asked.
    fn forget_with(
        config: &ConnectionConfig,
        session: &Session,
        store: &MemoryStore,
        answer: Option<FedAuthInfo>,
    ) -> usize {
        forget_beside(config, &[], session, store, answer)
    }

    /// `forget_with`, the `remaining` connections kept.
    fn forget_beside(
        config: &ConnectionConfig,
        remaining: &[ConnectionConfig],
        session: &Session,
        store: &MemoryStore,
        answer: Option<FedAuthInfo>,
    ) -> usize {
        let asked = AtomicUsize::new(0);
        forget_sign_in(config, remaining, session, store, &|_, _| {
            asked.fetch_add(1, Ordering::SeqCst);
            answer.clone()
        });
        asked.into_inner()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_finds_the_noted_tenant_without_the_server() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        h.token_for(&zero_config()).await.unwrap();
        h.browser.finish();
        assert!(h.store.get(&zero_config_key(TENANT)).unwrap().is_some());
        // The app was restarted: the server's answer is gone, and the
        // server is unreachable.
        let restarted = Session::default();
        let store = h.store;
        tokio::task::spawn_blocking(move || {
            assert_eq!(forget_with(&zero_config(), &restarted, store, None), 0);
        })
        .await
        .unwrap();
        assert_eq!(h.store.get(&zero_config_key(TENANT)).unwrap(), None);
        assert_eq!(h.store.get(&note_of(&zero_config())).unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_drops_every_key_known_offline() {
        let h = signed_in_without_the_server().await;
        // Also kept under the server's tenant, from an earlier sign-in.
        h.store.set(&zero_config_key(TENANT), "RT-7").unwrap();
        h.session.servers().insert(
            server_key("srv.database.windows.net", 1433),
            Probed {
                at: SystemTime::UNIX_EPOCH,
                info: azure_sql().unwrap(),
            },
        );
        let (session, store) = (h.session, h.store);
        tokio::task::spawn_blocking(move || {
            assert_eq!(forget_with(&zero_config(), session, store, None), 0);
        })
        .await
        .unwrap();
        for key in [
            zero_config_key("organizations"),
            tid_note(&zero_config_key("organizations")),
            zero_config_key(TENANT),
            note_of(&zero_config()),
        ] {
            assert_eq!(h.store.get(&key).unwrap(), None, "{key}");
        }
        assert!(h.session.tokens().is_empty());
        assert!(h.session.noted().is_empty());
        assert!(h.session.tids().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_note_the_server_is_asked_last() {
        let store: &'static MemoryStore = Box::leak(Box::default());
        let session: &'static Session = Box::leak(Box::default());
        store
            .set(&zero_config_key("organizations"), "RT-1")
            .unwrap();
        store.set(&zero_config_key(TENANT), "RT-7").unwrap();
        // Unreachable: the organizations key goes, the tenant's can't be
        // found.
        let asked =
            tokio::task::spawn_blocking(move || forget_with(&zero_config(), session, store, None))
                .await
                .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(store.get(&zero_config_key("organizations")).unwrap(), None);
        assert!(store.get(&zero_config_key(TENANT)).unwrap().is_some());
        // Reachable: its tenant's goes too.
        let asked = tokio::task::spawn_blocking(move || {
            forget_with(&zero_config(), session, store, azure_sql().unwrap())
        })
        .await
        .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(store.get(&zero_config_key(TENANT)).unwrap(), None);
    }

    #[test]
    fn work_left_behind_does_not_hold_forgets_thread() {
        let started = std::time::Instant::now();
        let out = run_bounded(async {
            // A name resolution stuck in getaddrinfo, say.
            drop(tokio::task::spawn_blocking(|| {
                std::thread::sleep(Duration::from_secs(3))
            }));
            7
        });
        assert_eq!(out, Some(7));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    // ----- Keys another connection uses -----

    fn on_server(host: &str) -> ConnectionConfig {
        ConnectionConfig {
            name: host.into(),
            host: Some(host.into()),
            ..zero_config()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_spares_the_keys_other_connections_use() {
        // Two servers of one tenant: one sign-in, under its key.
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        let (a, b) = (
            on_server("a.database.windows.net"),
            on_server("b.database.windows.net"),
        );
        h.token_for(&a).await.unwrap();
        h.browser.finish();
        h.token_for(&b).await.unwrap();
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        assert!(orphaned(&a, std::slice::from_ref(&b)));
        // Deleting `a` keeps what `b` uses (noted in the keyring)…
        let restarted: &'static Session = Box::leak(Box::default());
        let store = h.store;
        let kept = vec![b.clone()];
        tokio::task::spawn_blocking(move || {
            forget_beside(&a, &kept, restarted, store, None);
        })
        .await
        .unwrap();
        assert_eq!(
            h.store.get(&zero_config_key(TENANT)).unwrap().as_deref(),
            Some("RT-1")
        );
        assert_eq!(
            h.store
                .get(&note_of(&on_server("a.database.windows.net")))
                .unwrap(),
            None
        );
        assert!(h.store.get(&note_of(&b)).unwrap().is_some());
        // …and so does the session (its answer from b's server).
        let a = on_server("a.database.windows.net");
        let (session, store) = (h.session, h.store);
        tokio::task::spawn_blocking(move || {
            store.delete(&note_of(&b)).unwrap();
            forget_beside(&a, &[b], session, store, None);
        })
        .await
        .unwrap();
        assert!(h.store.get(&zero_config_key(TENANT)).unwrap().is_some());
        assert!(!h.session.tokens().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_spares_a_typed_tenants_key_and_a_shared_fallback() {
        let store: &'static MemoryStore = Box::leak(Box::default());
        let session: &'static Session = Box::leak(Box::default());
        let a = on_server("a.database.windows.net");
        store.set(&note_of(&a), TENANT).unwrap();
        store.set(&zero_config_key(TENANT), "RT-7").unwrap();
        store
            .set(&zero_config_key("organizations"), "RT-1")
            .unwrap();
        // The same tenant and application, typed.
        let typed = ConnectionConfig {
            tenant_id: Some(TENANT.into()),
            ..on_server("c.database.windows.net")
        };
        // A server that didn't answer this session: signed in under
        // organizations.
        let failed = on_server("d.database.windows.net");
        session.servers().insert(
            server_key("d.database.windows.net", 1433),
            Probed {
                at: SystemTime::UNIX_EPOCH,
                info: None,
            },
        );
        tokio::task::spawn_blocking(move || {
            forget_beside(&a, &[typed, failed], session, store, None);
        })
        .await
        .unwrap();
        assert!(store.get(&zero_config_key(TENANT)).unwrap().is_some());
        assert!(store
            .get(&zero_config_key("organizations"))
            .unwrap()
            .is_some());
        // Nobody else: both go.
        let a = on_server("a.database.windows.net");
        tokio::task::spawn_blocking(move || {
            store.set(&note_of(&a), TENANT).unwrap();
            forget_beside(&a, &[], session, store, None);
        })
        .await
        .unwrap();
        assert_eq!(store.get(&zero_config_key(TENANT)).unwrap(), None);
        assert_eq!(store.get(&zero_config_key("organizations")).unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_spares_a_home_sign_in_another_connection_borrows() {
        // `r` reuses the organizations sign-in, which landed in its server's
        // tenant.
        let h = signed_in_without_the_server().await;
        let r = on_server("r.database.windows.net");
        h.token_for(&r).await.unwrap();
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1, "borrowed");
        // Deleting the first connection keeps it, even after a restart.
        let restarted: &'static Session = Box::leak(Box::default());
        let store = h.store;
        tokio::task::spawn_blocking(move || {
            forget_beside(&zero_config(), &[r], restarted, store, None);
        })
        .await
        .unwrap();
        assert!(h
            .store
            .get(&zero_config_key("organizations"))
            .unwrap()
            .is_some());
        assert!(h
            .store
            .get(&tid_note(&zero_config_key("organizations")))
            .unwrap()
            .is_some());
    }

    // ----- The noted tenant when the server doesn't answer -----

    /// Memory of a restarted app: only the keyring is left.
    fn restart(h: &Harness) {
        h.session.tokens().clear();
        h.session.servers().clear();
        h.session.noted().clear();
        h.session.tids().clear();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_that_no_longer_answers_keeps_its_noted_tenant() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = azure_sql();
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-code");
        h.browser.finish();
        // Restarted, the server doesn't answer: its tenant is the noted one,
        // its refresh token renews the sign-in, no browser.
        restart(&h);
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        assert_eq!(h.token_for(&zero_config()).await.unwrap(), "AT-2");
        assert_eq!(h.probes(), 2);
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        assert_eq!(
            h.requests.lock().unwrap()[1].path,
            format!("/{TENANT}/oauth2/v2.0/token")
        );
        // The note stays.
        assert_eq!(
            h.store.get(&note_of(&zero_config())).unwrap().as_deref(),
            Some(TENANT)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sovereign_tenant_is_noted_with_its_authority() {
        let h = Harness::new(entra(&[]), Behaviour::SignIn).await;
        // A custom DNS name of a US Government server.
        *h.probed.lock().unwrap() = Ok(Some(FedAuthInfo {
            sts_url: format!("https://login.microsoftonline.us/{TENANT}"),
            spn: "https://database.usgovcloudapi.net/".into(),
        }));
        let c = on_server("sql.contoso.internal");
        h.token_for(&c).await.unwrap();
        h.browser.finish();
        // A token for US Government's Azure SQL, asked for and redeemed.
        const SCOPE: &str =
            "https://database.usgovcloudapi.net/.default offline_access openid profile";
        let Interaction::Waiting { url, .. } = &h.events()[0] else {
            panic!("no browser sign-in");
        };
        let authorize: HashMap<String, String> = Url::parse(url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        assert_eq!(authorize["scope"], SCOPE);
        assert_eq!(h.requests.lock().unwrap()[0].form["scope"], SCOPE);
        let note = h.store.get(&note_of(&c)).unwrap().unwrap();
        assert_eq!(note, format!("{TENANT}@login.microsoftonline.us"));
        // What it stands for when the server doesn't answer.
        let t = noted_target(&c, &note).unwrap();
        assert_eq!(t.authority, "https://login.microsoftonline.us");
        assert_eq!(t.tenant, TENANT);
        assert!(t
            .scope
            .starts_with("https://database.usgovcloudapi.net/.default "));
        assert_eq!(
            noted_target(&c, "organizations").unwrap().tenant,
            "organizations"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_organizations_note_still_asks_the_server_last() {
        let store: &'static MemoryStore = Box::leak(Box::default());
        let session: &'static Session = Box::leak(Box::default());
        store
            .set(&note_of(&zero_config()), "organizations")
            .unwrap();
        store
            .set(&zero_config_key("organizations"), "RT-1")
            .unwrap();
        store.set(&zero_config_key(TENANT), "RT-7").unwrap();
        // Unreachable: the tenant's refresh token can't be found.
        let asked =
            tokio::task::spawn_blocking(move || forget_with(&zero_config(), session, store, None))
                .await
                .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(store.get(&zero_config_key("organizations")).unwrap(), None);
        assert!(store.get(&zero_config_key(TENANT)).unwrap().is_some());
        // Reachable: it goes too.
        store
            .set(&note_of(&zero_config()), "organizations")
            .unwrap();
        let asked = tokio::task::spawn_blocking(move || {
            forget_with(&zero_config(), session, store, azure_sql().unwrap())
        })
        .await
        .unwrap();
        assert_eq!(asked, 1);
        assert_eq!(store.get(&zero_config_key(TENANT)).unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_borrowed_sign_in_signs_in_to_the_servers_tenant() {
        for restarted in [true, false] {
            // The organizations sign-in landed in TENANT; its refresh token
            // will be refused.
            let h = Harness::new(entra_in(&["RT-1"], Some(TENANT)), Behaviour::SignIn).await;
            *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
            h.token_for(&zero_config()).await.unwrap();
            h.browser.finish();
            // Restarted, or later in the session, the server names TENANT:
            // the organizations sign-in is borrowed, refused, and the
            // browser signs in to the server's tenant, not to organizations.
            if restarted {
                restart(&h);
            } else {
                h.clock.store(2 * 3600, Ordering::SeqCst);
            }
            *h.probed.lock().unwrap() = azure_sql();
            h.token_for(&zero_config()).await.unwrap();
            h.browser.finish();
            assert_eq!(
                h.grants(),
                ["authorization_code", "refresh_token", "authorization_code"]
            );
            {
                let requests = h.requests.lock().unwrap();
                assert_eq!(requests[1].path, "/organizations/oauth2/v2.0/token");
                assert_eq!(requests[2].path, format!("/{TENANT}/oauth2/v2.0/token"));
            }
            assert_eq!(
                h.store.get(&zero_config_key(TENANT)).unwrap().as_deref(),
                Some("RT-1")
            );
            // The refused sign-in is dropped with the tenant it landed in.
            let organizations = zero_config_key("organizations");
            assert_eq!(h.store.get(&organizations).unwrap(), None);
            assert_eq!(h.store.get(&tid_note(&organizations)).unwrap(), None);
            assert!(!h.session.tids().contains_key(&organizations));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_borrowed_sign_in_ends_a_reconnection() {
        let mut h = Harness::new(entra_in(&["RT-1"], Some(TENANT)), Behaviour::SignIn).await;
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        h.token_for(&zero_config()).await.unwrap();
        h.browser.finish();
        // Restarted, the server names TENANT: a reconnection borrows the
        // organizations sign-in, whose refresh token is refused…
        restart(&h);
        *h.probed.lock().unwrap() = azure_sql();
        h.interactive = false;
        let e = h.token_for(&zero_config()).await.unwrap_err().to_string();
        // …and ends there, without the browser.
        assert_eq!(e, session_expired("Prod Azure"));
        assert!(e.starts_with("Session Azure AD expirée"), "{e}");
        assert_eq!(h.grants(), ["authorization_code", "refresh_token"]);
        assert_eq!(
            h.requests.lock().unwrap()[1].path,
            "/organizations/oauth2/v2.0/token"
        );
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 1);
        assert_eq!(h.events().len(), 2, "no wait to show");
        let organizations = zero_config_key("organizations");
        assert_eq!(h.store.get(&organizations).unwrap(), None);
        assert_eq!(h.store.get(&tid_note(&organizations)).unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_us_government_organizations_sign_in_has_its_own_keys() {
        let h = Harness::new(entra_in(&[], Some(TENANT)), Behaviour::SignIn).await;
        // The server doesn't answer: organizations, of its cloud.
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        let c = on_server("srv.database.usgovcloudapi.net");
        assert_eq!(h.token_for(&c).await.unwrap(), home("AT-code"));
        h.browser.finish();
        {
            let requests = h.requests.lock().unwrap();
            assert_eq!(requests[0].path, "/organizations/oauth2/v2.0/token");
            assert_eq!(
                requests[0].form["scope"],
                "https://database.usgovcloudapi.net/.default offline_access openid profile"
            );
        }
        // Kept under the cloud's organizations key, with its tid note…
        let key = format!(
            "entra:organizations@login.microsoftonline.us:{DEFAULT_CLIENT_ID}:jane@contoso.com"
        );
        let tid = format!(
            "entra:tid:organizations@login.microsoftonline.us:{DEFAULT_CLIENT_ID}:\
             jane@contoso.com"
        );
        assert_eq!(h.store.get(&key).unwrap().as_deref(), Some("RT-1"));
        assert_eq!(h.store.get(&tid).unwrap().as_deref(), Some(TENANT));
        // …not under the public cloud's.
        let public = zero_config_key("organizations");
        assert_eq!(h.store.get(&public).unwrap(), None);
        assert_eq!(h.store.get(&tid_note(&public)).unwrap(), None);
        // Forgetting the connection removes both.
        let (session, store, forgotten) = (h.session, h.store, c.clone());
        tokio::task::spawn_blocking(move || {
            forget_with(&forgotten, session, store, None);
        })
        .await
        .unwrap();
        for k in [key, tid, note_of(&c)] {
            assert_eq!(h.store.get(&k).unwrap(), None, "{k}");
        }
        assert!(h.session.tokens().is_empty());
        assert!(h.session.tids().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_guest_connection_does_not_keep_the_home_sign_in() {
        const HOME: &str = "11111111-2222-3333-4444-555555555555";
        let h = Harness::new(entra_in(&[], Some(HOME)), Behaviour::SignIn).await;
        // `a` signed in under organizations (home tenant)…
        *h.probed.lock().unwrap() = Err("Le serveur ne répond pas".into());
        let a = on_server("a.database.windows.net");
        h.token_for(&a).await.unwrap();
        h.browser.finish();
        // …`x` in TENANT, where the account is a guest.
        *h.probed.lock().unwrap() = azure_sql();
        let x = on_server("x.database.windows.net");
        h.token_for(&x).await.unwrap();
        h.browser.finish();
        assert_eq!(h.browser.opened.load(Ordering::SeqCst), 2);
        // Deleting `a` forgets the home sign-in: `x` doesn't use it.
        let (session, store) = (h.session, h.store);
        tokio::task::spawn_blocking(move || {
            forget_beside(&a, &[x], session, store, None);
        })
        .await
        .unwrap();
        assert_eq!(
            h.store.get(&zero_config_key("organizations")).unwrap(),
            None
        );
        assert_eq!(
            h.store
                .get(&tid_note(&zero_config_key("organizations")))
                .unwrap(),
            None
        );
        assert!(h.store.get(&zero_config_key(TENANT)).unwrap().is_some());
    }
}
