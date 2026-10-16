//! Google Cloud IAM database authentication (Cloud SQL PostgreSQL / MySQL).

use crate::engine::models::DatabaseType;

/// Check the form: GcpIam needs a user (French messages).
pub fn validate(db_type: &DatabaseType, username: &str) -> Result<(), String> {
    let _ = db_type;
    if username.trim().is_empty() {
        return Err("Indique ton compte Google".to_string());
    }
    Ok(())
}

/// The database user name the IAM account must use for this engine
/// (PostgreSQL: as typed; MySQL: the part before '@'), for a form hint.
pub fn database_user(db_type: &DatabaseType, account: &str) -> String {
    let account = account.trim();
    match db_type {
        DatabaseType::MySQL => account.split('@').next().unwrap_or("").to_string(),
        _ => account.to_string(),
    }
}
