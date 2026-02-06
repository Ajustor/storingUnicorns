use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::TokioAsyncWriteCompatExt;

use crate::models::{AzureAuthMethod, ConnectionConfig, QueryResult, SchemaInfo};

use super::SqlServerClient;

// Azure AD constants
const AZURE_SQL_SCOPE: &str = "https://database.windows.net/.default";
const AZURE_SQL_RESOURCE: &str = "https://database.windows.net/";
/// Well-known Azure CLI client ID (public, multi-tenant)
const AZURE_CLI_CLIENT_ID: &str = "04b07795-a710-4f87-9e5b-0c2e8a34b4af";

// ========== Device Code Flow (Interactive Auth) ==========

#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[allow(dead_code)]
    pub expires_in: u64,
    pub interval: u64,
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: Option<String>,
    #[allow(dead_code)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[allow(dead_code)]
    error_description: Option<String>,
}

// ========== Connect Function ==========

/// Connect to Azure SQL Database
pub async fn connect(config: &ConnectionConfig) -> Result<SqlServerClient> {
    let auth_method = config
        .azure_auth_method
        .as_ref()
        .cloned()
        .unwrap_or_default();

    match auth_method {
        AzureAuthMethod::Credentials => connect_with_credentials(config).await,
        AzureAuthMethod::Interactive => connect_with_interactive_auth(config).await,
        AzureAuthMethod::ManagedIdentity => connect_with_managed_identity(config).await,
    }
}

/// Connect using SQL Server authentication (username/password)
async fn connect_with_credentials(config: &ConnectionConfig) -> Result<SqlServerClient> {
    // Azure SQL with SQL Server auth uses the standard SQL Server connector
    super::sqlserver::connect(config).await
}

/// Connect using Azure AD Interactive authentication (Device Code Flow)
async fn connect_with_interactive_auth(config: &ConnectionConfig) -> Result<SqlServerClient> {
    let tenant_id = config.tenant_id.as_deref().unwrap_or("common");

    let device_code = request_device_code(tenant_id).await?;
    let _ = open::that(&device_code.verification_uri);
    let token = poll_for_token(tenant_id, &device_code).await?;
    connect_with_aad_token(config, &token).await
}

/// Step 1: Request a device code from Azure AD (public for TUI integration)
pub async fn request_device_code(tenant_id: &str) -> Result<DeviceCodeResponse> {
    let device_code_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/devicecode",
        tenant_id
    );

    let http_client = reqwest::Client::new();
    let resp = http_client
        .post(&device_code_url)
        .form(&[
            ("client_id", AZURE_CLI_CLIENT_ID),
            ("scope", AZURE_SQL_SCOPE),
        ])
        .send()
        .await
        .map_err(|e| anyhow!("Failed to request device code: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Device code request failed: {}", body));
    }

    resp.json()
        .await
        .map_err(|e| anyhow!("Failed to parse device code response: {}", e))
}

/// Step 2: Poll Azure AD until the user completes authentication (public for TUI integration)
pub async fn poll_for_token(tenant_id: &str, device_code: &DeviceCodeResponse) -> Result<String> {
    let token_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/token",
        tenant_id
    );

    let http_client = reqwest::Client::new();
    let poll_interval = std::time::Duration::from_secs(device_code.interval.max(5));
    let max_attempts = 60;

    for attempt in 0..max_attempts {
        tokio::time::sleep(poll_interval).await;

        tracing::debug!(
            "Polling for token (attempt {}/{})",
            attempt + 1,
            max_attempts
        );

        let token_resp = http_client
            .post(&token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", AZURE_CLI_CLIENT_ID),
                ("device_code", &device_code.device_code),
            ])
            .send()
            .await
            .map_err(|e| anyhow!("Token polling failed: {}", e))?;

        let status = token_resp.status();
        let body = token_resp
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read token response: {}", e))?;

        if status.is_success() {
            let token: TokenResponse =
                serde_json::from_str(&body).map_err(|e| anyhow!("Failed to parse token: {}", e))?;
            return Ok(token.access_token);
        } else if let Ok(err_resp) = serde_json::from_str::<TokenErrorResponse>(&body) {
            match err_resp.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                "expired_token" => {
                    return Err(anyhow!("Device code expired. Please try again."));
                }
                "authorization_declined" => {
                    return Err(anyhow!("Authentication was declined by the user."));
                }
                _ => {
                    return Err(anyhow!(
                        "Authentication error: {} - {}",
                        err_resp.error,
                        err_resp.error_description.unwrap_or_default()
                    ));
                }
            }
        } else {
            return Err(anyhow!("Unexpected token response: {}", body));
        }
    }

    Err(anyhow!("Authentication timed out. Please try again."))
}

/// Connect using Managed Identity (IMDS endpoint)
async fn connect_with_managed_identity(config: &ConnectionConfig) -> Result<SqlServerClient> {
    tracing::info!("Requesting token from Azure Instance Metadata Service (IMDS)...");

    let http_client = reqwest::Client::new();

    // Azure IMDS endpoint for Managed Identity tokens
    let imds_url = format!(
        "http://169.254.169.254/metadata/identity/oauth2/token?api-version=2018-02-01&resource={}",
        AZURE_SQL_RESOURCE
    );

    let resp = http_client
        .get(&imds_url)
        .header("Metadata", "true")
        .send()
        .await
        .map_err(|e| {
            anyhow!(
                "Failed to contact Azure IMDS. Is this app running in an Azure environment? Error: {}",
                e
            )
        })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "IMDS token request failed (HTTP {}): {}. \
             Make sure this app is running on an Azure resource with Managed Identity enabled.",
            status,
            body
        ));
    }

    let token_resp: TokenResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse IMDS token response: {}", e))?;

    tracing::info!("✅ Managed Identity token acquired successfully");

    connect_with_aad_token(config, &token_resp.access_token).await
}

/// Step 3: Connect to Azure SQL using an AAD access token (public for TUI integration)
pub async fn connect_with_aad_token(
    config: &ConnectionConfig,
    token: &str,
) -> Result<SqlServerClient> {
    let mut tib_config = Config::new();
    tib_config.host(
        config
            .host
            .as_deref()
            .unwrap_or("localhost.database.windows.net"),
    );
    tib_config.port(config.port.unwrap_or(1433));
    tib_config.database(&config.database);
    tib_config.authentication(AuthMethod::AADToken(token.to_string()));
    tib_config.trust_cert();

    let tcp = TcpStream::connect(tib_config.get_addr()).await?;
    tcp.set_nodelay(true)?;
    let client = Client::connect(tib_config, tcp.compat_write()).await?;
    Ok(Arc::new(Mutex::new(client)))
}

// ========== Delegated Operations (same as SQL Server) ==========

/// Execute a query on Azure SQL Database
pub async fn execute_query(client: &SqlServerClient, query: &str) -> Result<QueryResult> {
    super::sqlserver::execute_query(client, query).await
}

/// Get list of tables grouped by schema
pub async fn get_tables_by_schema(client: &SqlServerClient) -> Result<Vec<SchemaInfo>> {
    super::sqlserver::get_tables_by_schema(client).await
}

/// Test the Azure connection
pub async fn test(client: &SqlServerClient) -> Result<()> {
    super::sqlserver::test(client).await
}
