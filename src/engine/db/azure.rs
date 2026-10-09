use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

use crate::engine::models::{AzureAuthMethod, ConnectionConfig, QueryResult, SchemaInfo};

use super::sqlserver::TdsClient;
use super::SqlServerClient;

// ========== Azure Cloud ==========

/// The Azure cloud an Azure SQL server lives in, which decides where its
/// tokens come from (token resource and Entra ID authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AzureCloud {
    Public,
    UsGovernment,
    China,
}

/// Host suffixes (dot-anchored) of each sovereign cloud's SQL endpoints.
/// Azure SQL Database / Managed Instance (`<mi>.<zone>.database…`), their
/// `privatelink` aliases and Synapse SQL pools.
const US_GOVERNMENT_SUFFIXES: [&str; 2] = [
    ".database.usgovcloudapi.net",
    ".sql.azuresynapse.usgovcloudapi.net",
];
const CHINA_SUFFIXES: [&str; 2] = [".database.chinacloudapi.cn", ".sql.azuresynapse.azure.cn"];

impl AzureCloud {
    /// The cloud of the server `host` (`tcp:` prefix, `,port` / `:port`,
    /// case and trailing dot ignored).
    ///
    /// Public-cloud hosts (`*.database.windows.net`, Synapse
    /// `*.sql.azuresynapse.net`, Fabric `*.datawarehouse.fabric.microsoft.com`
    /// / `*.database.fabric.microsoft.com`) and any other name — a custom DNS
    /// alias or a private endpoint name we can't recognise — use the public
    /// cloud: a sovereign-cloud server reached through such an alias would
    /// need its real `*.database.usgovcloudapi.net` / `…chinacloudapi.cn` name.
    pub fn from_host(host: &str) -> Self {
        let h = host.trim();
        let h = h
            .get(..4)
            .filter(|p| p.eq_ignore_ascii_case("tcp:"))
            .map_or(h, |_| &h[4..]);
        let h = h.split([',', ':']).next().unwrap_or_default();
        let h = h.trim_end_matches('.').to_ascii_lowercase();
        if US_GOVERNMENT_SUFFIXES.iter().any(|s| h.ends_with(s)) {
            Self::UsGovernment
        } else if CHINA_SUFFIXES.iter().any(|s| h.ends_with(s)) {
            Self::China
        } else {
            Self::Public
        }
    }

    /// Entra ID resource of Azure SQL in this cloud.
    pub fn sql_resource(self) -> &'static str {
        match self {
            Self::Public => "https://database.windows.net",
            Self::UsGovernment => "https://database.usgovcloudapi.net",
            Self::China => "https://database.chinacloudapi.cn",
        }
    }

    /// OAuth 2 scope for Azure SQL in this cloud.
    pub fn sql_scope(self) -> String {
        format!("{}/.default", self.sql_resource())
    }

    /// Entra ID authority host of this cloud.
    pub fn authority_host(self) -> &'static str {
        match self {
            Self::Public => "login.microsoftonline.com",
            Self::UsGovernment => "login.microsoftonline.us",
            Self::China => "login.chinacloudapi.cn",
        }
    }

    /// Name of this cloud for `az cloud set --name`.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Public => "AzureCloud",
            Self::UsGovernment => "AzureUSGovernment",
            Self::China => "AzureChinaCloud",
        }
    }
}

impl std::fmt::Display for AzureCloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Public => "Azure (cloud public)",
            Self::UsGovernment => "Azure Government",
            Self::China => "Azure Chine (21Vianet)",
        })
    }
}

/// The cloud of `config`'s server.
fn cloud_of(config: &ConnectionConfig) -> AzureCloud {
    AzureCloud::from_host(config.host.as_deref().unwrap_or_default())
}

// ========== Connect Function ==========

/// Connect to Azure SQL Database. The shared client reconnects through
/// `open` (acquiring a fresh token) when an operation was interrupted.
pub async fn connect(config: &ConnectionConfig) -> Result<SqlServerClient> {
    Ok(SqlServerClient::new(config.clone(), open(config).await?))
}

/// Open a client with the configured Azure authentication method.
pub async fn open(config: &ConnectionConfig) -> Result<TdsClient> {
    let auth_method = config
        .azure_auth_method
        .as_ref()
        .cloned()
        .unwrap_or_default();

    match auth_method {
        AzureAuthMethod::Credentials => connect_with_credentials(config).await,
        AzureAuthMethod::Interactive => connect_with_azure_cli(config).await,
        AzureAuthMethod::ManagedIdentity => connect_with_managed_identity(config).await,
    }
}

/// Connect using SQL Server authentication (username/password) to Azure:
/// unlike `sqlserver::open`, the certificate is verified (Azure SQL always
/// presents a publicly trusted one).
async fn connect_with_credentials(config: &ConnectionConfig) -> Result<TdsClient> {
    let auth = AuthMethod::sql_server(
        config.username.as_deref().unwrap_or_default(),
        config.password.as_deref().unwrap_or_default(),
    );
    connect_verified(azure_config(config, auth)).await
}

/// Connect using Azure CLI (`az account get-access-token`), with optional tenant_id
async fn connect_with_azure_cli(config: &ConnectionConfig) -> Result<TdsClient> {
    let tenant_id = config.tenant_id.as_deref();
    let token = get_azure_cli_token(cloud_of(config), tenant_id).await?;
    connect_with_aad_token(config, &token).await
}

// ========== Azure CLI Token Acquisition ==========

#[derive(Deserialize)]
struct CliTokenResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
}

/// Run `az` with `args` (through `cmd` on Windows, where `az` is a script).
async fn run_az(args: &[&str]) -> std::io::Result<std::process::Output> {
    use tokio::process::Command;

    if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "az"])
            .args(args)
            .output()
            .await
    } else {
        Command::new("az").args(args).output().await
    }
}

/// Get an access token for the SQL resource of `cloud` from Azure CLI,
/// optionally scoped to a specific tenant.
async fn get_azure_cli_token(cloud: AzureCloud, tenant_id: Option<&str>) -> Result<String> {
    let mut args = vec![
        "account",
        "get-access-token",
        "--resource",
        cloud.sql_resource(),
        "--output",
        "json",
    ];

    if let Some(tid) = tenant_id {
        args.push("--tenant");
        args.push(tid);
    }

    tracing::info!(
        "Acquiring Azure SQL token via Azure CLI ({:?}, tenant: {})...",
        cloud,
        tenant_id.unwrap_or("default")
    );

    let output = run_az(&args).await.map_err(|e| {
        anyhow!(
            "Impossible de lancer Azure CLI (« az ») : est-il installé ? ({})",
            e
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A token for another cloud's resource fails in a confusing way:
        // say which cloud the CLI must be switched to.
        let active = active_cli_cloud().await;
        if let Some(hint) = cli_cloud_hint(cloud, active.as_deref()) {
            bail!("{}\n\nDétail d'Azure CLI : {}", hint, stderr.trim());
        }
        bail!(
            "Azure CLI n'a pas fourni de jeton : {}. Lance `az login` d'abord.",
            stderr.trim()
        );
    }

    let response: CliTokenResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow!("Réponse d'Azure CLI illisible : {}", e))?;

    tracing::info!("Azure CLI token acquired successfully");
    Ok(response.access_token)
}

/// The cloud Azure CLI is set to (`AzureCloud`…), if `az` answers.
async fn active_cli_cloud() -> Option<String> {
    let out = run_az(&["cloud", "show", "--query", "name", "--output", "tsv"])
        .await
        .ok()
        .filter(|o| o.status.success())?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The fix to suggest when Azure CLI is not set to the server's `cloud`:
/// `active` is the CLI's cloud, `None` if unknown (only sovereign clouds then
/// get a hint, the CLI defaulting to the public cloud).
pub fn cli_cloud_hint(cloud: AzureCloud, active: Option<&str>) -> Option<String> {
    let wrong = match active {
        Some(name) => !name.eq_ignore_ascii_case(cloud.cli_name()),
        None => cloud != AzureCloud::Public,
    };
    wrong.then(|| {
        format!(
            "Cet hôte est dans {} : lance `az cloud set --name {}` puis `az login`",
            cloud,
            cloud.cli_name()
        )
    })
}

// ========== Managed Identity ==========

/// How long to wait for the managed identity endpoint, which may be
/// unreachable (not an Azure resource) rather than refusing.
const MANAGED_IDENTITY_TIMEOUT: Duration = Duration::from_secs(15);

/// The managed identity's client ID: `username` trimmed, `None` (system
/// identity) when empty. A client ID is a GUID.
pub fn managed_identity_client_id(username: Option<&str>) -> Result<Option<String>> {
    let id = username.map(str::trim).unwrap_or_default();
    if id.is_empty() {
        return Ok(None);
    }
    if !is_guid(id) {
        bail!(
            "« {} » n'est pas un ID client d'identité managée : attendu un GUID \
             (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx), ou laisse vide pour l'identité système",
            id
        );
    }
    Ok(Some(id.to_ascii_lowercase()))
}

/// `8-4-4-4-12` hexadecimal digits.
fn is_guid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Message for a failed managed identity token request.
fn managed_identity_error(err: &azure_core::Error, client_id: Option<&str>) -> String {
    use azure_core::error::ErrorKind;

    match err.kind() {
        ErrorKind::Io => no_managed_identity(),
        _ if err.to_string().contains("not been assigned") => match client_id {
            Some(id) => format!(
                "L'identité managée d'ID client {id} n'est pas affectée à cette ressource Azure"
            ),
            None => "Aucune identité managée système n'est activée sur cette ressource Azure \
                     (ou précise l'ID client d'une identité affectée par l'utilisateur)"
                .to_string(),
        },
        _ => format!("Échec de l'obtention d'un jeton par identité managée : {err}"),
    }
}

fn no_managed_identity() -> String {
    "Aucune identité managée disponible : cette machine n'est pas une ressource Azure avec \
     identité (VM, App Service, Functions, Container Apps…) ou le point de terminaison \
     d'identité ne répond pas"
        .to_string()
}

/// Connect using a managed identity: the system-assigned one, or the
/// user-assigned identity whose client ID is in `username`.
async fn connect_with_managed_identity(config: &ConnectionConfig) -> Result<TdsClient> {
    use azure_core::credentials::TokenCredential;
    use azure_identity::{
        AppServiceManagedIdentityCredential, ImdsId, TokenCredentialOptions,
        VirtualMachineManagedIdentityCredential,
    };

    let client_id = managed_identity_client_id(config.username.as_deref())?;
    let cloud = cloud_of(config);
    tracing::info!(
        "Acquiring Azure SQL token via managed identity ({:?}, {})...",
        cloud,
        client_id.as_deref().unwrap_or("system-assigned")
    );

    // azure_identity 0.22 has no `ManagedIdentityCredential`: App Service,
    // Functions and Container Apps expose `IDENTITY_ENDPOINT` (system identity
    // only in this version), other Azure hosts (VM, scale sets, AKS…) IMDS.
    let options = TokenCredentialOptions::default();
    let credential: std::sync::Arc<dyn TokenCredential> =
        if std::env::var_os("IDENTITY_ENDPOINT").is_some() {
            if client_id.is_some() {
                bail!(
                    "Les identités affectées par l'utilisateur ne sont pas prises en charge \
                     sur App Service / Functions / Container Apps : laisse l'ID client vide \
                     pour utiliser l'identité système"
                );
            }
            AppServiceManagedIdentityCredential::new(options)
                .map_err(|e| anyhow!("{}: {e}", no_managed_identity()))?
        } else {
            let id = client_id
                .clone()
                .map_or(ImdsId::SystemAssigned, ImdsId::ClientId);
            VirtualMachineManagedIdentityCredential::new(id, options)
                .map_err(|e| anyhow!("{}: {e}", no_managed_identity()))?
        };

    let scope = cloud.sql_scope();
    let response = tokio::time::timeout(MANAGED_IDENTITY_TIMEOUT, credential.get_token(&[&scope]))
        .await
        .map_err(|_| anyhow!(no_managed_identity()))?
        .map_err(|e| anyhow!(managed_identity_error(&e, client_id.as_deref())))?;

    let token = response.token.secret().to_string();

    tracing::info!("Managed Identity token acquired successfully");
    connect_with_aad_token(config, &token).await
}

// ========== Connection ==========

/// The tiberius configuration for an Azure server: always encrypted and,
/// unlike plain SQL Server connections, with the server certificate verified
/// against the system roots (no `trust_cert`), since a password or a bearer
/// token is sent to whoever answers.
fn azure_config(config: &ConnectionConfig, auth: AuthMethod) -> Config {
    let mut tib_config = Config::new();
    tib_config.host(
        config
            .host
            .as_deref()
            .unwrap_or("localhost.database.windows.net"),
    );
    tib_config.port(config.port.unwrap_or(1433));
    tib_config.database(&config.database);
    tib_config.authentication(auth);
    tib_config.encryption(tiberius::EncryptionLevel::Required);
    tib_config
}

async fn connect_with_aad_token(config: &ConnectionConfig, token: &str) -> Result<TdsClient> {
    let auth = AuthMethod::AADToken(token.to_string());
    connect_verified(azure_config(config, auth)).await
}

async fn connect_verified(tib_config: Config) -> Result<TdsClient> {
    let tcp = TcpStream::connect(tib_config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    Ok(Client::connect(tib_config, tcp.compat_write()).await?)
}

// ========== Delegated Operations (same as SQL Server) ==========

/// Execute a query on Azure SQL Database, keeping at most `max_rows` rows
pub async fn execute_query_limited(
    client: &SqlServerClient,
    query: &str,
    max_rows: Option<usize>,
) -> Result<QueryResult> {
    super::sqlserver::execute_query_limited(client, query, max_rows).await
}

/// Get list of tables grouped by schema
pub async fn get_tables_by_schema(client: &SqlServerClient) -> Result<Vec<SchemaInfo>> {
    super::sqlserver::get_tables_by_schema(client).await
}

/// Test the Azure connection
pub async fn test(client: &SqlServerClient) -> Result<()> {
    super::sqlserver::test(client).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_cloud_hosts() {
        for host in [
            "srv.database.windows.net",
            "mi.abc123.database.windows.net",
            "srv.privatelink.database.windows.net",
            "ws.sql.azuresynapse.net",
            "x.datawarehouse.fabric.microsoft.com",
            "x.database.fabric.microsoft.com",
        ] {
            assert_eq!(AzureCloud::from_host(host), AzureCloud::Public, "{host}");
        }
        let c = AzureCloud::Public;
        assert_eq!(c.sql_resource(), "https://database.windows.net");
        assert_eq!(c.authority_host(), "login.microsoftonline.com");
        assert_eq!(c.cli_name(), "AzureCloud");
    }

    #[test]
    fn government_cloud_hosts() {
        for host in [
            "srv.database.usgovcloudapi.net",
            "ws.sql.azuresynapse.usgovcloudapi.net",
        ] {
            assert_eq!(
                AzureCloud::from_host(host),
                AzureCloud::UsGovernment,
                "{host}"
            );
        }
        let c = AzureCloud::UsGovernment;
        assert_eq!(c.sql_resource(), "https://database.usgovcloudapi.net");
        assert_eq!(c.authority_host(), "login.microsoftonline.us");
        assert_eq!(c.cli_name(), "AzureUSGovernment");
        assert_eq!(c.to_string(), "Azure Government");
    }

    #[test]
    fn china_cloud_hosts() {
        for host in [
            "srv.database.chinacloudapi.cn",
            "ws.sql.azuresynapse.azure.cn",
        ] {
            assert_eq!(AzureCloud::from_host(host), AzureCloud::China, "{host}");
        }
        let c = AzureCloud::China;
        assert_eq!(c.sql_resource(), "https://database.chinacloudapi.cn");
        assert_eq!(c.authority_host(), "login.chinacloudapi.cn");
        assert_eq!(c.cli_name(), "AzureChinaCloud");
    }

    #[test]
    fn host_matching_ignores_case_port_and_trailing_dot() {
        assert_eq!(
            AzureCloud::from_host("  SRV.Database.UsGovCloudApi.NET. "),
            AzureCloud::UsGovernment
        );
        assert_eq!(
            AzureCloud::from_host("tcp:srv.database.chinacloudapi.cn,1433"),
            AzureCloud::China
        );
        assert_eq!(
            AzureCloud::from_host("TCP:srv.database.usgovcloudapi.net:1433"),
            AzureCloud::UsGovernment
        );
    }

    #[test]
    fn lookalikes_and_custom_names_default_to_public() {
        for host in [
            "notdatabase.usgovcloudapi.net",
            "srv.database.usgovcloudapi.net.evil.com",
            "notdatabase.chinacloudapi.cn",
            "notdatabase.windows.net.evil.com",
            "database.usgovcloudapi.net",
            "sql.contoso.internal",
            "",
        ] {
            assert_eq!(AzureCloud::from_host(host), AzureCloud::Public, "{host}");
        }
    }

    #[test]
    fn scope_is_the_resource_with_default() {
        assert_eq!(
            AzureCloud::Public.sql_scope(),
            "https://database.windows.net/.default"
        );
        assert_eq!(
            AzureCloud::UsGovernment.sql_scope(),
            "https://database.usgovcloudapi.net/.default"
        );
        assert_eq!(
            AzureCloud::China.sql_scope(),
            "https://database.chinacloudapi.cn/.default"
        );
    }

    #[test]
    fn cli_hint_names_the_cloud_to_switch_to() {
        assert_eq!(
            cli_cloud_hint(AzureCloud::UsGovernment, Some("AzureCloud")).unwrap(),
            "Cet hôte est dans Azure Government : lance \
             `az cloud set --name AzureUSGovernment` puis `az login`"
        );
        assert!(cli_cloud_hint(AzureCloud::China, Some("AzureCloud"))
            .unwrap()
            .contains("--name AzureChinaCloud"));
        // Unknown CLI cloud: it defaults to the public one.
        assert!(cli_cloud_hint(AzureCloud::China, None).is_some());
        assert!(cli_cloud_hint(AzureCloud::Public, None).is_none());
        // Already on the right cloud: the failure is something else.
        assert!(cli_cloud_hint(AzureCloud::UsGovernment, Some("azureusgovernment")).is_none());
        assert!(cli_cloud_hint(AzureCloud::Public, Some("AzureCloud")).is_none());
        // Public host while the CLI is on a sovereign cloud.
        assert!(
            cli_cloud_hint(AzureCloud::Public, Some("AzureUSGovernment"))
                .unwrap()
                .contains("--name AzureCloud")
        );
    }

    #[test]
    fn client_id_is_an_optional_guid() {
        assert_eq!(managed_identity_client_id(None).unwrap(), None);
        assert_eq!(managed_identity_client_id(Some("  ")).unwrap(), None);
        assert_eq!(
            managed_identity_client_id(Some(" 8F1C2D3E-4A5B-6C7D-8E9F-0A1B2C3D4E5F "))
                .unwrap()
                .as_deref(),
            Some("8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f")
        );
        for bad in [
            "sa",
            "8f1c2d3e4a5b6c7d8e9f0a1b2c3d4e5f",
            "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5",
            "8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5g",
            "{8f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f}",
        ] {
            let e = managed_identity_client_id(Some(bad)).unwrap_err();
            assert!(
                e.to_string().contains("n'est pas un ID client"),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn unreachable_endpoint_means_no_managed_identity() {
        use azure_core::error::{Error, ErrorKind};
        let io = Error::message(ErrorKind::Io, "failed to execute `reqwest` request");
        assert!(managed_identity_error(&io, None).starts_with("Aucune identité managée disponible"));
        let unassigned = Error::message(
            ErrorKind::Credential,
            "the requested identity has not been assigned to this resource",
        );
        assert!(managed_identity_error(&unassigned, Some("abc")).contains("ID client abc"));
    }

    #[test]
    fn azure_connections_are_encrypted_and_verify_the_certificate() {
        let config = ConnectionConfig {
            host: Some("srv.database.windows.net".into()),
            ..Default::default()
        };
        for auth in [
            AuthMethod::AADToken("t".into()),
            AuthMethod::sql_server("u", "p"),
        ] {
            let debug = format!("{:?}", azure_config(&config, auth));
            assert!(debug.contains("trust: Default"), "{debug}");
            assert!(debug.contains("encryption: Required"), "{debug}");
        }
    }
}
