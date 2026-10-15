//! Azure SQL sign-in with the user's own Entra ID account through the
//! browser (OAuth 2.0 authorization code + PKCE), like SSMS's "Active
//! Directory Interactive".

use super::azure::is_guid;

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
}
