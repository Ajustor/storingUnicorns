//! Azure SQL sign-in with the user's own Entra ID account through the
//! browser (OAuth 2.0 authorization code + PKCE), like SSMS's "Active
//! Directory Interactive".

use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::engine::models::ConnectionConfig;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::models::{AzureAuthMethod, DatabaseType};

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
}
