//! French explanations for TLS connection failures (PostgreSQL / MySQL).

use std::path::Path;

/// A hint replacing the driver's message for a known TLS failure.
pub fn explain(message: &str, host: &str) -> Option<String> {
    let m = message.to_ascii_lowercase();
    if (m.contains("pg_hba.conf") && m.contains("no encryption"))
        || m.contains("require_secure_transport")
    {
        Some("Le serveur exige une connexion chiffrée : passe le mode SSL à Obligatoire.".into())
    } else if m.contains("notvalidforname") || m.contains("not valid for name") {
        // rustls: `NotValidForName`, or `NotValidForNameContext` printed as
        // "certificate not valid for name …".
        Some(format!("Le certificat ne correspond pas à {host}."))
    } else if m.contains("unknownissuer") || m.contains("invalid peer certificate") {
        Some(
            "Certificat du serveur non reconnu : indique son certificat CA, ou passe le \
             mode SSL à Obligatoire (chiffré, sans vérification)."
                .into(),
        )
    } else if m.contains("does not support tls") || m.contains("doesn't support tls") {
        Some("Le serveur ne propose pas de connexion chiffrée.".into())
    } else {
        None
    }
}

/// Fail early, in French, when the CA file can't be read or isn't PEM.
pub fn check_ca(path: Option<&Path>) -> Result<(), String> {
    let Some(p) = path else {
        return Ok(());
    };
    let unreadable = |why: String| format!("Certificat CA illisible : {} ({why})", p.display());
    let bytes = std::fs::read(p).map_err(|e| unreadable(e.to_string()))?;
    if String::from_utf8_lossy(&bytes).contains("-----BEGIN CERTIFICATE-----") {
        Ok(())
    } else {
        Err(unreadable("pas un certificat PEM".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tls_failures_get_a_french_hint() {
        let h = |m: &str| explain(m, "db.example.com");
        assert!(h("error returned from database: no pg_hba.conf entry for host \"1.2.3.4\", user \"u\", database \"d\", no encryption")
            .unwrap().contains("passe le mode SSL à Obligatoire"));
        assert!(h("error returned from database: 3159 (HY000): Connections using insecure transport are prohibited while --require_secure_transport=ON.")
            .unwrap().contains("exige une connexion chiffrée"));
        // rustls 0.23 prints `CertificateError::UnknownIssuer` with Debug.
        assert!(
            h("error communicating with database: invalid peer certificate: UnknownIssuer")
                .unwrap()
                .contains("Certificat du serveur non reconnu")
        );
        assert!(h("invalid peer certificate: NotValidForName")
            .unwrap()
            .contains("ne correspond pas à db.example.com"));
        // ...and the webpki name mismatch as `NotValidForNameContext`.
        assert!(h("error occurred while attempting to establish a TLS connection: invalid peer certificate: certificate not valid for name \"db.example.com\"; certificate is only valid for other.example.com")
            .unwrap().contains("ne correspond pas à db.example.com"));
        assert!(h("error occurred while attempting to establish a TLS connection: server does not support TLS")
            .unwrap().contains("ne propose pas de connexion chiffrée"));
        assert_eq!(
            h("invalid peer certificate: UnknownIssuer").unwrap(),
            "Certificat du serveur non reconnu : indique son certificat CA, ou passe le mode \
             SSL à Obligatoire (chiffré, sans vérification)."
        );
        assert_eq!(h("password authentication failed for user \"u\""), None);
    }

    #[test]
    fn unreadable_ca_file_is_reported() {
        let e = check_ca(Some(std::path::Path::new("Z:/does/not/exist.pem"))).unwrap_err();
        assert!(
            e.starts_with("Certificat CA illisible : Z:/does/not/exist.pem"),
            "{e}"
        );
        assert!(check_ca(None).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\n").unwrap();
        assert!(check_ca(Some(&ca)).is_ok());
        std::fs::write(&ca, "not a certificate").unwrap();
        let e = check_ca(Some(&ca)).unwrap_err();
        assert!(e.contains("pas un certificat PEM"), "{e}");
    }
}
