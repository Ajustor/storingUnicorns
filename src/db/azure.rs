use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_util::compat::TokioAsyncWriteCompatExt;

use crate::models::{AzureAuthMethod, ConnectionConfig, QueryResult, SchemaInfo};

use super::SqlServerClient;

/// Azure SQL Database resource for token acquisition
const AZURE_SQL_RESOURCE: &str = "https://database.windows.net";

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
        AzureAuthMethod::Interactive => connect_with_azure_cli(config).await,
        AzureAuthMethod::ManagedIdentity => connect_with_managed_identity(config).await,
    }
}

/// Connect using SQL Server authentication (username/password)
async fn connect_with_credentials(config: &ConnectionConfig) -> Result<SqlServerClient> {
    super::sqlserver::connect(config).await
}

/// Connect using Azure CLI (`az account get-access-token`), with optional tenant_id
async fn connect_with_azure_cli(config: &ConnectionConfig) -> Result<SqlServerClient> {
    let tenant_id = config.tenant_id.as_deref();
    let token = get_azure_cli_token(tenant_id).await?;
    connect_with_aad_token(config, &token).await
}

// ========== Azure CLI Token Acquisition ==========

#[derive(Deserialize)]
struct CliTokenResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
}

/// Get an access token from Azure CLI, optionally scoped to a specific tenant
async fn get_azure_cli_token(tenant_id: Option<&str>) -> Result<String> {
    use tokio::process::Command;

    let mut args = vec![
        "account",
        "get-access-token",
        "--resource",
        AZURE_SQL_RESOURCE,
        "--output",
        "json",
    ];

    if let Some(tid) = tenant_id {
        args.push("--tenant");
        args.push(tid);
    }

    tracing::info!(
        "Acquiring Azure SQL token via Azure CLI (tenant: {})...",
        tenant_id.unwrap_or("default")
    );

    let output = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "az"])
            .args(&args)
            .output()
            .await
    } else {
        Command::new("az").args(&args).output().await
    };

    let output = output.map_err(|e| {
        anyhow!(
            "Failed to run 'az' CLI. Is Azure CLI installed and are you logged in? Error: {}",
            e
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "Azure CLI failed: {}. Run 'az login' first.",
            stderr.trim()
        ));
    }

    let response: CliTokenResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow!("Failed to parse Azure CLI response: {}", e))?;

    tracing::info!("Azure CLI token acquired successfully");
    Ok(response.access_token)
}

// ========== Managed Identity ==========

/// Connect using Managed Identity (via DefaultAzureCredential)
async fn connect_with_managed_identity(config: &ConnectionConfig) -> Result<SqlServerClient> {
    use azure_core::credentials::TokenCredential;
    use azure_identity::DefaultAzureCredential;

    tracing::info!("Acquiring token via DefaultAzureCredential (managed identity)...");

    let credential = DefaultAzureCredential::new()
        .map_err(|e| anyhow!("Failed to create DefaultAzureCredential: {}", e))?;

    let response = credential
        .get_token(&[&format!("{}/.default", AZURE_SQL_RESOURCE)])
        .await
        .map_err(|e| {
            anyhow!(
                "Failed to acquire Azure AD token via managed identity: {}",
                e
            )
        })?;

    let token = response.token.secret().to_string();

    tracing::info!("Managed Identity token acquired successfully");
    connect_with_aad_token(config, &token).await
}

// ========== AAD Token Connection ==========

async fn connect_with_aad_token(config: &ConnectionConfig, token: &str) -> Result<SqlServerClient> {
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
